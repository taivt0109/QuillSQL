using System;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace QuillSql.Vsix
{
    /// <summary>
    /// One long-running `quill --serve` process. Requests are serialized; a crashed or
    /// hung engine is restarted on the next request instead of affecting SSMS.
    /// </summary>
    internal sealed class EngineProcess : IDisposable
    {
        private const int TimeoutMilliseconds = 5000;
        private readonly object _gate = new object();
        private Process _process;
        private BlockingCollection<string> _replies;
        private int _nextId;

        public static EngineProcess Shared { get; } = new EngineProcess();

        /// <summary>
        /// Sends `{"op":op, ...fields}` and returns the reply line, or null on failure.
        /// Replies are read by a dedicated thread, so a busy thread pool cannot delay them.
        /// </summary>
        public string Request(string op, string fields)
        {
            // A request stuck behind another must not wait forever either.
            if (!Monitor.TryEnter(_gate, TimeoutMilliseconds * 2))
            {
                Log.Write("engine busy; " + op + " skipped");
                return null;
            }
            try
            {
                try
                {
                    EnsureStarted();
                    var id = ++_nextId;
                    var line = "{\"id\":" + id + ",\"op\":" + Json.Str(op) + (string.IsNullOrEmpty(fields) ? "" : "," + fields) + "}";
                    // Writing blocks once the pipe is full, if the engine stops reading.
                    var input = _process.StandardInput;
                    var sent = Task.Run(() =>
                    {
                        input.WriteLine(line);
                        input.Flush();
                    });
                    if (!sent.Wait(TimeoutMilliseconds))
                    {
                        Log.Write("engine stopped reading; " + op + " not sent");
                        _ = sent.ContinueWith(t => t.Exception, CancellationToken.None, TaskContinuationOptions.OnlyOnFaulted, TaskScheduler.Default);
                        Stop();
                        return null;
                    }
                    if (!_replies.TryTake(out var reply, TimeoutMilliseconds) || reply == null)
                    {
                        Log.Write("engine did not answer " + op + " (" + line.Length + " chars): " + (line.Length > 400 ? line.Substring(0, 400) + "..." : line));
                        Stop();
                        return null;
                    }
                    return reply;
                }
                catch (Exception ex)
                {
                    Log.Error("engine request " + op, ex);
                    Stop();
                    return null;
                }
            }
            finally
            {
                Monitor.Exit(_gate);
            }
        }

        private void EnsureStarted()
        {
            if (_process != null && !_process.HasExited)
            {
                return;
            }
            Stop();
            var directory = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location);
            var arguments = "--serve";
            var style = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".quillsql.json");
            if (File.Exists(style))
            {
                arguments += " --style \"" + style + "\"";
            }
            _process = Process.Start(new ProcessStartInfo(Path.Combine(directory, "quill.exe"), arguments)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                StandardOutputEncoding = new UTF8Encoding(false),
            });
            // Drain stderr so style warnings cannot fill the pipe and block the engine.
            var process = _process;
            var replies = _replies = new BlockingCollection<string>();
            new Thread(() =>
            {
                try
                {
                    string reply;
                    while ((reply = process.StandardOutput.ReadLine()) != null)
                    {
                        replies.Add(reply);
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("reading engine output", ex);
                }
                replies.Add(null);
            }) { IsBackground = true, Name = "QuillSQL engine reader" }.Start();
            _process.ErrorDataReceived += (s, e) => { if (!string.IsNullOrEmpty(e.Data)) Log.Write("engine: " + e.Data); };
            _process.BeginErrorReadLine();
            Log.Write("engine started, pid " + _process.Id);
        }

        private void Stop()
        {
            if (_process == null)
            {
                return;
            }
            try
            {
                if (!_process.HasExited)
                {
                    _process.Kill();
                }
            }
            catch (Exception ex)
            {
                Log.Error("stopping engine", ex);
            }
            _process.Dispose();
            _process = null;
        }

        public void Dispose()
        {
            lock (_gate)
            {
                Stop();
            }
        }
    }
}
