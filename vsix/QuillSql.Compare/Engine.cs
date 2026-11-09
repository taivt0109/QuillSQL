using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace QuillSql.Compare
{
    public sealed class EngineException : Exception
    {
        public EngineException(string message) : base(message) { }
    }

    /// <summary>
    /// The `quill compare-serve` process. Requests run one at a time; each may report progress
    /// before its reply. A comparison can take minutes, so there is no fixed timeout: Cancel stops
    /// the process, and the next request starts a new one.
    /// </summary>
    public sealed class Engine : IDisposable
    {
        private readonly string _exe;
        private readonly SemaphoreSlim _gate = new SemaphoreSlim(1, 1);
        private readonly ConcurrentDictionary<long, Pending> _pending = new ConcurrentDictionary<long, Pending>();
        private Process _process;
        private long _nextId;

        private sealed class Pending
        {
            public TaskCompletionSource<object> Reply = new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously);
            public Action<string> Progress;
            public Action<JsonObject> Steps;
        }

        /// <summary>
        /// Microsoft Entra sign-in with MFA waits for the user: raised (on a background thread)
        /// with the page to open, the code to enter there and Entra's message.
        /// </summary>
        public event Action<string, string, string> SignIn;

        /// <summary>Raised (on a background thread) when a request has its reply.</summary>
        public event Action Replied;

        /// <param name="exe">Full path of quill.exe.</param>
        public Engine(string exe) => _exe = exe;

        /// <summary>quill.exe next to this assembly, or on the PATH.</summary>
        public static string FindQuill()
        {
            var here = Path.Combine(Path.GetDirectoryName(typeof(Engine).Assembly.Location) ?? ".", "quill.exe");
            if (File.Exists(here)) return here;
            var local = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "bin", "quill.exe");
            return File.Exists(local) ? local : "quill.exe";
        }

        private void EnsureStarted()
        {
            if (_process != null && !_process.HasExited) return;
            var info = new ProcessStartInfo(_exe, "compare-serve")
            {
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                CreateNoWindow = true,
                StandardOutputEncoding = new UTF8Encoding(false),
            };
            _process = Process.Start(info) ?? throw new EngineException("could not start " + _exe);
            var process = _process;
            var reader = new Thread(() => ReadReplies(process)) { IsBackground = true, Name = "QuillSQL compare engine" };
            reader.Start();
            process.ErrorDataReceived += (s, e) => { if (e.Data != null) Debug.WriteLine("quill: " + e.Data); };
            process.BeginErrorReadLine();
        }

        private void ReadReplies(Process process)
        {
            try
            {
                string line;
                while ((line = process.StandardOutput.ReadLine()) != null)
                {
                    if (!(Json.Parse(line) is JsonObject reply)) continue;
                    var id = reply.Long("id");
                    if (!_pending.TryGetValue(id, out var pending)) continue;
                    if (reply.Obj("deviceCode") is JsonObject code) SignIn?.Invoke(code.Str("verificationUri"), code.Str("userCode"), reply.Str("progress"));
                    if (reply.ContainsKey("progress"))
                    {
                        pending.Progress?.Invoke(reply.Str("progress"));
                        pending.Steps?.Invoke(reply);
                        continue;
                    }
                    _pending.TryRemove(id, out _);
                    Replied?.Invoke();
                    if (reply.ContainsKey("error")) pending.Reply.TrySetException(new EngineException(reply.Str("error")));
                    else pending.Reply.TrySetResult(reply.TryGetValue("result", out var r) ? r : null);
                }
            }
            catch (Exception ex)
            {
                Debug.WriteLine("compare engine reader: " + ex);
            }
            foreach (var p in _pending.Values)
            {
                p.Reply.TrySetException(new EngineException("the compare engine stopped"));
            }
            _pending.Clear();
        }

        /// <summary>
        /// Sends `{"op":op,...fields}` and returns the result. `progress` has the text of each progress
        /// line; `steps` the whole line, with the side, stage and counts a comparison reports.
        /// </summary>
        public async Task<object> Request(string op, IDictionary<string, object> fields = null, Action<string> progress = null, Action<JsonObject> steps = null)
        {
            await _gate.WaitAsync().ConfigureAwait(false);
            try
            {
                EnsureStarted();
                var id = Interlocked.Increment(ref _nextId);
                var request = new JsonObject { ["id"] = id, ["op"] = op };
                if (fields != null)
                {
                    foreach (var kv in fields) request[kv.Key] = kv.Value;
                }
                var pending = new Pending { Progress = progress, Steps = steps };
                _pending[id] = pending;
                await _process.StandardInput.WriteLineAsync(Json.Write(request)).ConfigureAwait(false);
                await _process.StandardInput.FlushAsync().ConfigureAwait(false);
                return await pending.Reply.Task.ConfigureAwait(false);
            }
            finally
            {
                _gate.Release();
            }
        }

        public async Task<JsonObject> RequestObject(string op, IDictionary<string, object> fields = null, Action<string> progress = null, Action<JsonObject> steps = null)
            => await Request(op, fields, progress, steps).ConfigureAwait(false) as JsonObject ?? new JsonObject();

        /// <summary>Stops the running request; open comparisons are lost.</summary>
        public void Cancel()
        {
            try
            {
                if (_process != null && !_process.HasExited) _process.Kill();
            }
            catch (InvalidOperationException)
            {
            }
            _process = null;
        }

        public void Dispose() => Cancel();
    }
}
