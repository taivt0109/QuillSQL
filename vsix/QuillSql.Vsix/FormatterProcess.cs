using System;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.VisualStudio.Threading;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Runs the bundled Rust engine in its own process, so a bug in it cannot take SSMS down.
    /// </summary>
    internal static class FormatterProcess
    {
        private const int TimeoutMilliseconds = 15000;
        private static readonly Encoding Utf8 = new UTF8Encoding(false);

        public static async Task<string> RunAsync(string sql, CancellationToken cancellationToken)
        {
            await TaskScheduler.Default;

            var directory = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location);
            var arguments = "--stdin";
            var style = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".quillsql.json");
            if (File.Exists(style))
            {
                arguments += " --style \"" + style + "\"";
            }

            var startInfo = new ProcessStartInfo(Path.Combine(directory, "quill.exe"), arguments)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                StandardOutputEncoding = Utf8,
                StandardErrorEncoding = Utf8,
            };

            using (var process = Process.Start(startInfo))
            {
                // Read and write concurrently: a large script would otherwise fill a pipe and deadlock.
                var output = process.StandardOutput.ReadToEndAsync();
                var errors = process.StandardError.ReadToEndAsync();
                var input = WriteInputAsync(process, sql);
                var finished = Task.WhenAll(output, errors, input);

                var completed = await Task.WhenAny(finished, Task.Delay(TimeoutMilliseconds, cancellationToken));
                if (completed != finished)
                {
                    process.Kill();
                    cancellationToken.ThrowIfCancellationRequested();
                    throw new TimeoutException("the formatter did not finish in time.");
                }

                await finished;
                process.WaitForExit();
                if (process.ExitCode != 0)
                {
                    throw new InvalidOperationException((await errors).Trim());
                }
                return await output;
            }
        }

        private static async Task WriteInputAsync(Process process, string sql)
        {
            var bytes = Utf8.GetBytes(sql);
            using (var stdin = process.StandardInput.BaseStream)
            {
                await stdin.WriteAsync(bytes, 0, bytes.Length);
            }
        }
    }
}
