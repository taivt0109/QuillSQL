using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.Serialization;
using System.Text;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class ClaudeResult
    {
        [DataMember(Name = "is_error")] public bool IsError { get; set; }
        [DataMember(Name = "result")] public string Result { get; set; }
    }

    /// <summary>Optional %LOCALAPPDATA%\QuillSQL\ai.json: {"model": "sonnet", "language": "Vietnamese"}.</summary>
    [DataContract]
    internal sealed class AiSettings
    {
        [DataMember(Name = "model")] public string Model { get; set; }
        [DataMember(Name = "language")] public string Language { get; set; }
    }

    /// <summary>
    /// AI commands backed by the Claude Code CLI already installed and signed in on this machine.
    /// Each request runs `claude -p` with every tool disabled, so Claude can only answer; the
    /// script (or selection) and a summary of the tables it uses are sent on stdin.
    /// </summary>
    internal static class AiAssistant
    {
        private const int TimeoutSeconds = 180;
        private static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL");

        private const string SqlOnly =
            "You are a T-SQL assistant inside SQL Server Management Studio. Reply with T-SQL only: no Markdown, no code fences, " +
            "and any explanation as SQL comments. Use only the tables, columns and procedures in the schema you are given; " +
            "if something you need is missing, say so in a comment. Keep the user's style and casing.";

        private const string Explainer =
            "You explain T-SQL to the database developer who wrote it, inside SQL Server Management Studio. Be concise: " +
            "what the code does, step by step, then any bugs or risks you notice. Plain text, no Markdown headings.";

        private const string Reviewer =
            "You review T-SQL for SQL Server for correctness and performance. Reply with T-SQL only, no Markdown: first a " +
            "block comment listing the problems you found and why, then the improved script. If the script is already good, " +
            "say so in the comment and repeat it unchanged.";

        private const string PlanAnalyst =
            "You are a SQL Server performance specialist. You are given a query, the tables it uses with their row counts and " +
            "existing indexes, and its execution plan (estimated, or actual with measured rows). Reply with T-SQL only, no Markdown: " +
            "first a block comment that says where the cost goes, citing the plan's operators, estimated against actual rows, " +
            "warnings and missing-index hints; then the changes you recommend as runnable T-SQL (CREATE INDEX statements, " +
            "rewritten query, statistics updates), each with a comment explaining why. Never propose an index that already " +
            "exists or is covered by one; weigh table size and write cost. If the plan is already good, say so.";

        private const string WorkloadAnalyst =
            "You are a SQL Server performance specialist reviewing a database's most expensive queries from Query Store, with " +
            "the tables they use, their indexes and the plan of each query's slowest plan. Reply with T-SQL only, no Markdown: " +
            "first a block comment ranking the problems by impact (cite query_id, durations, reads, plan regressions where a " +
            "query has several plans of very different speed); then recommended fixes as runnable T-SQL with comments. Never " +
            "propose an index that already exists. Prefer a few high-impact changes over many small ones.";

        /// <summary>claude.exe on PATH or in the usual install folders, or null.</summary>
        public static string FindClaude()
        {
            var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
            var roaming = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
            var candidates = (Environment.GetEnvironmentVariable("PATH") ?? "")
                .Split(';')
                .Where(d => d.Trim().Length > 0)
                .SelectMany(d => new[] { Path.Combine(d.Trim(), "claude.exe"), Path.Combine(d.Trim(), "claude.cmd") })
                .Concat(new[] { Path.Combine(home, ".local", "bin", "claude.exe"), Path.Combine(roaming, "npm", "claude.cmd") });
            foreach (var path in candidates)
            {
                try
                {
                    if (File.Exists(path))
                    {
                        return path;
                    }
                }
                catch (ArgumentException)
                {
                    // A malformed PATH entry.
                }
            }
            return null;
        }

        private static AiSettings Settings()
        {
            var path = Path.Combine(Folder, "ai.json");
            try
            {
                return File.Exists(path) ? Json.Parse<AiSettings>(File.ReadAllText(path)) ?? new AiSettings() : new AiSettings();
            }
            catch (Exception ex)
            {
                Log.Error("reading " + path, ex);
                return new AiSettings();
            }
        }

        /// <summary>Quotes one argument the way CommandLineToArgvW parses it.</summary>
        private static string Quote(string arg)
        {
            if (arg.Length > 0 && arg.IndexOfAny(new[] { ' ', '\t', '"' }) < 0)
            {
                return arg;
            }
            var sb = new StringBuilder("\"");
            var slashes = 0;
            foreach (var c in arg)
            {
                if (c == '\\')
                {
                    slashes++;
                    continue;
                }
                sb.Append('\\', c == '"' ? slashes * 2 + 1 : slashes);
                slashes = 0;
                sb.Append(c);
            }
            sb.Append('\\', slashes * 2).Append('"');
            return sb.ToString();
        }

        /// <summary>Runs one Claude request and returns its answer; throws with a readable message on failure.</summary>
        private static string Ask(string systemPrompt, string input)
        {
            var claude = FindClaude() ?? throw new InvalidOperationException("Claude Code CLI was not found. Install it and sign in with `claude` first.");
            var settings = Settings();
            Directory.CreateDirectory(Folder);
            var systemFile = Path.Combine(Folder, "ai-system-prompt.txt");
            var system = systemPrompt + (string.IsNullOrEmpty(settings.Language) ? "" : " Write explanations and comments in " + settings.Language + ".");
            File.WriteAllText(systemFile, system, new UTF8Encoding(false));
            var args = new[]
            {
                "-p", "Do what the input asks.",
                "--system-prompt-file", systemFile,
                "--tools", "",
                "--restricted", "--strict-mcp-config", "--no-session-persistence", "--disable-slash-commands",
                "--output-format", "json",
            }.ToList();
            if (!string.IsNullOrEmpty(settings.Model))
            {
                args.Add("--model");
                args.Add(settings.Model);
            }
            var line = string.Join(" ", args.Select(Quote));
            var cmd = claude.EndsWith(".cmd", StringComparison.OrdinalIgnoreCase);
            // An empty working folder: Claude has no tools, and nothing here to read anyway.
            var work = Path.Combine(Path.GetTempPath(), "QuillSQL-ai");
            Directory.CreateDirectory(work);
            var start = new ProcessStartInfo(cmd ? "cmd.exe" : claude, cmd ? "/d /s /c \"" + Quote(claude) + " " + line + "\"" : line)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                StandardOutputEncoding = new UTF8Encoding(false),
                StandardErrorEncoding = new UTF8Encoding(false),
                WorkingDirectory = work,
            };
            var watch = Stopwatch.StartNew();
            using (var process = Process.Start(start))
            {
                // Both pipes are drained on their own threads so neither can fill up and block Claude.
                string output = "", errors = "";
                var readOut = new System.Threading.Thread(() => output = process.StandardOutput.ReadToEnd()) { IsBackground = true };
                var readErr = new System.Threading.Thread(() => errors = process.StandardError.ReadToEnd()) { IsBackground = true };
                readOut.Start();
                readErr.Start();
                using (var writer = new StreamWriter(process.StandardInput.BaseStream, new UTF8Encoding(false)))
                {
                    writer.Write(input);
                }
                if (!process.WaitForExit(TimeoutSeconds * 1000))
                {
                    try { process.Kill(); } catch (InvalidOperationException) { }
                    throw new TimeoutException("Claude did not answer within " + TimeoutSeconds + " seconds.");
                }
                readOut.Join();
                readErr.Join();
                Log.Write("claude answered in " + watch.ElapsedMilliseconds + " ms, exit " + process.ExitCode + ", " + output.Length + " chars");
                ClaudeResult result = null;
                try
                {
                    result = output.Trim().Length == 0 ? null : Json.Parse<ClaudeResult>(output.Trim());
                }
                catch (SerializationException)
                {
                    // Not JSON: fall through to the error below.
                }
                if (result == null || result.IsError || process.ExitCode != 0)
                {
                    var detail = result?.Result ?? errors.Trim();
                    throw new InvalidOperationException("Claude failed: " + (detail.Length > 300 ? detail.Substring(0, 300) : detail));
                }
                return result.Result ?? "";
            }
        }

        /// <summary>Removes a Markdown code fence if the model added one anyway.</summary>
        private static string StripFence(string text)
        {
            var t = text.Trim();
            if (!t.StartsWith("```", StringComparison.Ordinal))
            {
                return t;
            }
            var firstLine = t.IndexOf('\n');
            var end = t.LastIndexOf("```", StringComparison.Ordinal);
            return firstLine < 0 || end <= firstLine ? t : t.Substring(firstLine + 1, end - firstLine - 1).Trim();
        }

        private static string Context(EditorContext editor, string key)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var reply = EngineProcess.Shared.Request("schema_context", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text));
            var schema = reply == null ? null : Json.Parse<TextReply>(reply)?.Result?.Text;
            return string.IsNullOrEmpty(schema) ? "(No database metadata: not connected, or still loading.)" : schema;
        }

        private static void Start(AsyncPackage package, string what, Func<string> work, Action<string> done)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            EditorContext.Status("asking Claude to " + what + "...");
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                string answer = null, error = null;
                await TaskScheduler.Default;
                try
                {
                    answer = work();
                }
                catch (Exception ex)
                {
                    Log.Error("AI " + what, ex);
                    error = ex.Message;
                }
                await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (error != null)
                {
                    EditorContext.Status(error);
                    return;
                }
                Commands.Run("AI " + what, () =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    done(answer);
                });
            });
        }

        /// <summary>Writes or rewrites SQL from a request: replaces the selection, or inserts at the caret.</summary>
        public static void AskForSql(AsyncPackage package) => Commands.Run("Ask AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var request = EditorContext.Prompt("Ask AI", editor.HasSelection
                ? "What should Claude do with the selected SQL? (for example: add paging, convert to a MERGE)"
                : "What SQL should Claude write? (for example: top 10 customers by order total this year)", "");
            if (string.IsNullOrWhiteSpace(request))
            {
                return;
            }
            var key = Commands.MetadataKey(out _);
            var selected = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : null;
            var input = "Request: " + request + "\n\nSchema:\n" + Context(editor, key)
                + (selected != null ? "\nRewrite this SQL (reply with the replacement only):\n" + selected : "\nThe script so far, for context (reply with only the new SQL to insert):\n" + editor.Text);
            Start(package, "write SQL", () => StripFence(Ask(SqlOnly, input)), answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                var now = EditorContext.For(editor.View);
                if (now == null || now.Text != editor.Text)
                {
                    EditorContext.Status("the script changed while Claude was answering; nothing was inserted.");
                    Commands.WriteOutput(new[] { "Claude's answer (not inserted):", answer });
                    return;
                }
                if (selected != null)
                {
                    now.Replace(editor.SelectionStart, editor.SelectionEnd, answer);
                    now.Select(editor.SelectionStart, editor.SelectionStart + answer.Length);
                }
                else
                {
                    var text = (editor.Caret > 0 && editor.Text[editor.Caret - 1] != '\n' ? Environment.NewLine : "") + answer + Environment.NewLine;
                    now.Replace(editor.Caret, editor.Caret, text);
                    now.Select(editor.Caret, editor.Caret + text.Length);
                }
                EditorContext.Status("Claude's SQL inserted; review it before running. Undo removes it.");
            });
        });

        private static PlanTarget Target(string key, string sql)
        {
            var reply = EngineProcess.Shared.Request("plan_target", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(sql));
            var parsed = reply == null ? null : Json.Parse<PlanTargetReply>(reply);
            return parsed?.Result ?? throw new InvalidOperationException(parsed?.Error ?? "the engine did not answer");
        }

        private static string Summarize(IList<string> plans, int maxLines)
        {
            if (plans.Count == 0)
            {
                return "(no plan)";
            }
            var reply = EngineProcess.Shared.Request("plan_summary",
                "\"maxLines\":" + maxLines + ",\"plans\":[" + string.Join(",", plans.Select(Json.Str)) + "]");
            return (reply == null ? null : Json.Parse<TextReply>(reply)?.Result?.Text) ?? "(the plan could not be summarized)";
        }

        /// <summary>
        /// Reads the indexes and the execution plan of the selection or script and asks Claude where
        /// the time goes. With `actual`, runs the script (read-only scripts only, after asking).
        /// </summary>
        public static void AnalyzePlan(AsyncPackage package, bool actual) => Commands.Run("Analyze plan with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            var connection = ConnectionResolver.TryGetActive();
            if (editor == null || connection == null || !connection.CanQuery)
            {
                EditorContext.Status("open a query window connected with Windows or SQL Server authentication first.");
                return;
            }
            var key = Commands.MetadataKey(out _);
            var sql = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : editor.Text;
            var target = Target(key, sql);
            if (actual)
            {
                if (target.WriteReason != null)
                {
                    EditorContext.Status("not run for an actual plan because " + target.WriteReason + "; use Analyze Plan with AI (estimated) instead.");
                    return;
                }
                var run = EditorContext.Confirm("Analyze Actual Plan with AI",
                    "Run the query on " + connection.Description + " to measure its actual plan?\n\n"
                    + "The results are discarded. The script only reads, but it uses the server like any query.", "Run");
                if (!run)
                {
                    return;
                }
            }
            var schema = Context(editor, key);
            var description = connection.Description;
            Start(package, actual ? "analyze the actual plan" : "analyze the plan", () =>
            {
                var errors = new List<string>();
                var indexes = PlanCapture.Indexes(connection, target.Objects ?? new string[0]);
                var plans = PlanCapture.Plans(connection, target.Batches ?? new string[0], actual, errors);
                var plan = Summarize(plans, 150);
                var facts = "Server and database: " + description + "\n\nSchema:\n" + schema
                    + "\nTables, row counts and indexes:\n" + indexes
                    + "\n" + (actual ? "Actual" : "Estimated") + " execution plan:\n" + plan
                    + (errors.Count == 0 ? "" : "\n\nErrors while getting the plan:\n" + string.Join("\n", errors));
                ThreadHelper.JoinableTaskFactory.Run(async () =>
                {
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    Commands.WriteOutput(new[] { "Sent to Claude for plan analysis:", facts });
                });
                return StripFence(Ask(PlanAnalyst, "Analyze the performance of this SQL.\n\nSQL:\n" + sql + "\n\n" + facts));
            }, answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                EditorContext.OpenInNewQuery(answer + Environment.NewLine);
                EditorContext.Status("Claude's plan analysis opened in a new window; what it was given is in the Output window.");
            });
        });

        /// <summary>Query Store's most expensive queries of the last week, with their plans, reviewed by Claude.</summary>
        public static void ReviewSlowQueries(AsyncPackage package) => Commands.Run("Find slow queries with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var connection = ConnectionResolver.TryGetActive();
            if (connection == null || !connection.CanQuery)
            {
                EditorContext.Status("open a query window connected with Windows or SQL Server authentication first.");
                return;
            }
            var key = Commands.MetadataKey(out _);
            var description = connection.Description;
            Start(package, "review the slowest queries", () =>
            {
                var queries = PlanCapture.QueryStore(connection, 7, 10);
                if (queries.Count == 0)
                {
                    throw new InvalidOperationException("Query Store has no queries from the last 7 days.");
                }
                var all = string.Join("\n;\n", queries.Select(q => q.Text));
                var target = Target(key, all);
                var indexes = PlanCapture.Indexes(connection, target.Objects ?? new string[0]);
                var report = new StringBuilder("Server and database: " + description + "\n\nMost expensive queries of the last 7 days (Query Store):\n");
                for (var i = 0; i < queries.Count; i++)
                {
                    report.Append("\n").Append(i + 1).Append(". ").Append(PlanCapture.Describe(queries[i])).Append('\n');
                    if (i < 3 && queries[i].WorstPlanXml != null)
                    {
                        report.Append("Slowest plan of this query:\n").Append(Summarize(new[] { queries[i].WorstPlanXml }, 40)).Append('\n');
                    }
                }
                report.Append("\nTables, row counts and indexes:\n").Append(indexes);
                var facts = report.ToString();
                ThreadHelper.JoinableTaskFactory.Run(async () =>
                {
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    Commands.WriteOutput(new[] { "Sent to Claude for workload review:", facts });
                });
                return StripFence(Ask(WorkloadAnalyst, facts));
            }, answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                EditorContext.OpenInNewQuery(answer + Environment.NewLine);
                EditorContext.Status("Claude's review of the slowest queries opened in a new window; the data is in the Output window.");
            });
        });

        public static void Explain(AsyncPackage package) => Commands.Run("Explain with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var sql = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : editor.Text;
            var input = "Explain this SQL.\n\nSchema:\n" + Context(editor, Commands.MetadataKey(out _)) + "\nSQL:\n" + sql;
            Start(package, "explain the SQL", () => Ask(Explainer, input), answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                Commands.WriteOutput(new[] { "Claude's explanation:", answer.Trim() });
                EditorContext.Status("explanation is in the Output window.");
            });
        });

        public static void Review(AsyncPackage package) => Commands.Run("Review with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var sql = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : editor.Text;
            var input = "Review and improve this SQL.\n\nSchema:\n" + Context(editor, Commands.MetadataKey(out _)) + "\nSQL:\n" + sql;
            Start(package, "review the SQL", () => StripFence(Ask(Reviewer, input)), answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                EditorContext.OpenInNewQuery(answer + Environment.NewLine);
                EditorContext.Status("Claude's review opened in a new window.");
            });
        });
    }
}
