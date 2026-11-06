using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;
using Microsoft.Data.SqlClient;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;

namespace QuillSql.Vsix
{
    /// <summary>What one chat turn lets the AI read, and where to report the tools it uses.</summary>
    internal sealed class McpSession
    {
        /// <summary>The query windows the conversation is about, the first one by default.</summary>
        public List<IVsWindowFrame> Windows = new List<IVsWindowFrame>();
        public bool AllowData;
        /// <summary>A tool started: call id, tool name, its arguments.</summary>
        public Action<int, string, IDictionary<string, object>> Started;
        /// <summary>A tool finished: call id, its result, whether it failed.</summary>
        public Action<int, string, bool> Finished;
    }

    /// <summary>One tool call's query window: the one it names, or the conversation's first.</summary>
    internal sealed class ToolContext
    {
        public IVsWindowFrame Window;
        public string WindowName;
        public ConnectionTarget Target;
        public string Key;
        public bool AllowData;
        /// <summary>All the conversation's windows that are still open.</summary>
        public List<QueryWindow> Windows;
    }

    /// <summary>
    /// A Model Context Protocol server inside SSMS, so the AI chat can look things up in the
    /// database itself: objects, columns, indexes, statistics, plans, Query Store, and (only when
    /// the user allows it) rows from read-only queries. It listens on 127.0.0.1 only, answers only
    /// requests carrying the token of a chat turn in progress, and every tool only reads.
    /// </summary>
    internal static class McpServer
    {
        private const int MaxResult = 30000;
        private static readonly ConcurrentDictionary<string, McpSession> Sessions = new ConcurrentDictionary<string, McpSession>();
        private static readonly object Gate = new object();
        private static TcpListener _listener;
        private static int _calls;

        public static int Port { get; private set; }

        /// <summary>Starts the server if needed and returns a token for this session.</summary>
        public static string Register(McpSession session)
        {
            lock (Gate)
            {
                if (_listener == null)
                {
                    _listener = new TcpListener(IPAddress.Loopback, 0);
                    _listener.Start();
                    Port = ((IPEndPoint)_listener.LocalEndpoint).Port;
                    new Thread(Accept) { IsBackground = true, Name = "QuillSQL MCP" }.Start();
                    Log.Write("AI tools listening on 127.0.0.1:" + Port);
                }
            }
            var bytes = new byte[24];
            using (var random = RandomNumberGenerator.Create())
            {
                random.GetBytes(bytes);
            }
            var token = Convert.ToBase64String(bytes).Replace('+', '-').Replace('/', '_').TrimEnd('=');
            Sessions[token] = session;
            return token;
        }

        public static void Unregister(string token)
        {
            if (token != null)
            {
                Sessions.TryRemove(token, out _);
            }
        }

        private static void Accept()
        {
            while (true)
            {
                TcpClient client;
                try
                {
                    client = _listener.AcceptTcpClient();
                }
                catch (Exception ex)
                {
                    Log.Error("AI tools listener", ex);
                    return;
                }
                ThreadPool.QueueUserWorkItem(_ => Serve(client));
            }
        }

        // ---- HTTP, just enough for MCP's streamable HTTP transport with JSON answers ----

        private static void Serve(TcpClient client)
        {
            using (client)
            {
                try
                {
                    client.ReceiveTimeout = 30000;
                    var stream = client.GetStream();
                    var head = ReadHead(stream);
                    if (head == null)
                    {
                        return;
                    }
                    var lines = head.Split(new[] { "\r\n" }, StringSplitOptions.None);
                    var method = lines[0].Split(' ')[0];
                    var headers = lines.Skip(1).Where(l => l.Contains(":"))
                        .ToDictionary(l => l.Substring(0, l.IndexOf(':')).Trim().ToLowerInvariant(), l => l.Substring(l.IndexOf(':') + 1).Trim(), StringComparer.Ordinal);
                    headers.TryGetValue("content-length", out var lengthText);
                    int.TryParse(lengthText, out var length);
                    var body = new byte[Math.Max(0, Math.Min(length, 4 * 1024 * 1024))];
                    var read = 0;
                    while (read < body.Length)
                    {
                        var n = stream.Read(body, read, body.Length - read);
                        if (n <= 0)
                        {
                            break;
                        }
                        read += n;
                    }
                    headers.TryGetValue("authorization", out var authorization);
                    var token = authorization != null && authorization.StartsWith("Bearer ", StringComparison.Ordinal) ? authorization.Substring(7) : null;
                    if (token == null || !Sessions.TryGetValue(token, out var session))
                    {
                        Respond(stream, "401 Unauthorized", null);
                        return;
                    }
                    if (method != "POST")
                    {
                        Respond(stream, "405 Method Not Allowed", null);
                        return;
                    }
                    var answer = Handle(Encoding.UTF8.GetString(body, 0, read), session);
                    Respond(stream, answer == null ? "202 Accepted" : "200 OK", answer);
                }
                catch (Exception ex)
                {
                    Log.Error("AI tools request", ex);
                }
            }
        }

        private static string ReadHead(Stream stream)
        {
            var bytes = new List<byte>();
            while (bytes.Count < 65536)
            {
                var b = stream.ReadByte();
                if (b < 0)
                {
                    return null;
                }
                bytes.Add((byte)b);
                var n = bytes.Count;
                if (n >= 4 && bytes[n - 4] == '\r' && bytes[n - 3] == '\n' && bytes[n - 2] == '\r' && bytes[n - 1] == '\n')
                {
                    return Encoding.ASCII.GetString(bytes.ToArray(), 0, n - 4);
                }
            }
            return null;
        }

        private static void Respond(Stream stream, string status, string json)
        {
            var body = json == null ? new byte[0] : Encoding.UTF8.GetBytes(json);
            var head = "HTTP/1.1 " + status + "\r\nContent-Type: application/json\r\nContent-Length: " + body.Length + "\r\nConnection: close\r\n\r\n";
            var bytes = Encoding.ASCII.GetBytes(head);
            stream.Write(bytes, 0, bytes.Length);
            stream.Write(body, 0, body.Length);
            stream.Flush();
        }

        // ---- JSON-RPC ----

        private static string Handle(string body, McpSession session)
        {
            var json = new JavaScriptSerializer { MaxJsonLength = int.MaxValue };
            var request = json.DeserializeObject(body) as Dictionary<string, object>;
            if (request == null || !request.TryGetValue("id", out var id))
            {
                return null; // A notification.
            }
            var method = request.TryGetValue("method", out var m) ? m as string : null;
            var parameters = request.TryGetValue("params", out var p) ? p as Dictionary<string, object> : null;
            object result;
            switch (method)
            {
                case "initialize":
                    result = new Dictionary<string, object>
                    {
                        ["protocolVersion"] = parameters != null && parameters.TryGetValue("protocolVersion", out var version) ? version : "2025-06-18",
                        ["capabilities"] = new Dictionary<string, object> { ["tools"] = new Dictionary<string, object>() },
                        ["serverInfo"] = new Dictionary<string, object> { ["name"] = "quillsql", ["version"] = QuillSqlPackage.Version },
                    };
                    break;
                case "ping":
                    result = new Dictionary<string, object>();
                    break;
                case "tools/list":
                    result = new Dictionary<string, object> { ["tools"] = Tools.Select(t => t.Describe()).ToArray() };
                    break;
                case "tools/call":
                    result = Call(parameters, session);
                    break;
                default:
                    return json.Serialize(new Dictionary<string, object>
                    {
                        ["jsonrpc"] = "2.0",
                        ["id"] = id,
                        ["error"] = new Dictionary<string, object> { ["code"] = -32601, ["message"] = "Unknown method " + method },
                    });
            }
            return json.Serialize(new Dictionary<string, object> { ["jsonrpc"] = "2.0", ["id"] = id, ["result"] = result });
        }

        private static object Call(Dictionary<string, object> parameters, McpSession session)
        {
            var name = parameters != null && parameters.TryGetValue("name", out var n) ? n as string : null;
            var arguments = (parameters != null && parameters.TryGetValue("arguments", out var a) ? a as Dictionary<string, object> : null) ?? new Dictionary<string, object>();
            var tool = Tools.FirstOrDefault(t => t.Name == name);
            var call = Interlocked.Increment(ref _calls);
            session.Started?.Invoke(call, name, arguments);
            string text;
            var failed = false;
            var watch = Stopwatch.StartNew();
            try
            {
                if (tool == null)
                {
                    throw new ToolException("There is no tool named " + name + ".");
                }
                var context = OnUiThread(() => Resolve(session, Text(arguments, "window", false)));
                if (tool.NeedsDatabase && (context.Target == null || !context.Target.CanQuery))
                {
                    throw new ToolException("Query window " + (context.WindowName ?? "(none)") + " is not connected, or its login cannot be used by QuillSQL, so its database cannot be read. Answer from what you were given.");
                }
                text = tool.Run(context, arguments);
            }
            catch (ToolException ex)
            {
                text = ex.Message;
                failed = true;
            }
            catch (SqlException ex)
            {
                text = "SQL Server error " + ex.Number + ": " + ex.Message;
                failed = true;
            }
            catch (Exception ex)
            {
                Log.Error("AI tool " + name, ex);
                text = "The tool failed: " + ex.Message;
                failed = true;
            }
            if (text.Length > MaxResult)
            {
                text = text.Substring(0, MaxResult) + "\n(cut: the result was longer than " + MaxResult + " characters)";
            }
            Log.Write("AI tool " + name + (failed ? " failed" : "") + " in " + watch.ElapsedMilliseconds + " ms, " + text.Length + " chars");
            session.Finished?.Invoke(call, text, failed);
            return new Dictionary<string, object>
            {
                ["content"] = new object[] { new Dictionary<string, object> { ["type"] = "text", ["text"] = text } },
                ["isError"] = failed,
            };
        }

        /// <summary>The window a tool call is about: the one named in `window`, which must be one of the conversation's, or the first.</summary>
        private static ToolContext Resolve(McpSession session, string name)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var windows = session.Windows.Select(QueryWindows.Refresh).Where(w => w != null).ToList();
            QueryWindow window;
            if (name == null)
            {
                window = windows.FirstOrDefault();
            }
            else
            {
                window = windows.FirstOrDefault(w => w.Caption.IndexOf(name, StringComparison.OrdinalIgnoreCase) >= 0);
                if (window == null)
                {
                    throw new ToolException("'" + name + "' is not one of this conversation's query windows (" + string.Join(", ", windows.Select(w => w.Name))
                        + "). Ask the developer to add it with + Add tab above the chat box.");
                }
            }
            return new ToolContext
            {
                Window = window?.Frame,
                WindowName = window?.Name,
                Target = window?.Connection,
                Key = window?.Key() ?? "",
                AllowData = session.AllowData,
                Windows = windows,
            };
        }

        // ---- The tools ----

        private sealed class ToolException : Exception
        {
            public ToolException(string message) : base(message) { }
        }

        private sealed class Tool
        {
            public string Name, Description;
            /// <summary>False for the tools that read the editor rather than the database.</summary>
            public bool NeedsDatabase = true;
            public string[][] Parameters; // name, type, description, "required" or ""
            public Func<ToolContext, IDictionary<string, object>, string> Run;

            private static readonly string[] WindowParameter =
                { "window", "string", "Tab name of one of the conversation's query windows; the first one if omitted. The tool uses that window's connection.", "" };

            public object Describe()
            {
                // Database tools, and current_query, take the window to work on.
                var parameters = NeedsDatabase || Name == "current_query" ? Parameters.Concat(new[] { WindowParameter }).ToArray() : Parameters;
                return new Dictionary<string, object>
                {
                    ["name"] = Name,
                    ["description"] = Description,
                    ["annotations"] = new Dictionary<string, object> { ["readOnlyHint"] = true },
                    ["inputSchema"] = new Dictionary<string, object>
                    {
                        ["type"] = "object",
                        ["properties"] = parameters.ToDictionary(p => p[0], p => (object)new Dictionary<string, object> { ["type"] = p[1], ["description"] = p[2] }),
                        ["required"] = parameters.Where(p => p[3] == "required").Select(p => p[0]).ToArray(),
                    },
                };
            }
        }

        private static string Text(IDictionary<string, object> arguments, string name, bool required = true)
        {
            var value = arguments.TryGetValue(name, out var v) ? Convert.ToString(v, CultureInfo.InvariantCulture)?.Trim() : null;
            if (required && string.IsNullOrEmpty(value))
            {
                throw new ToolException("The argument " + name + " is required.");
            }
            return string.IsNullOrEmpty(value) ? null : value;
        }

        private static int Number(IDictionary<string, object> arguments, string name, int fallback, int min, int max)
        {
            var value = arguments.TryGetValue(name, out var v) && v != null && int.TryParse(Convert.ToString(v, CultureInfo.InvariantCulture), out var n) ? n : fallback;
            return Math.Max(min, Math.Min(max, value));
        }

        /// <summary>Runs a catalog query and returns its rows as a pipe-separated table.</summary>
        private static string Table(ToolContext session, string sql, int maxRows, params SqlParameter[] parameters) =>
            Tables(session, sql, maxRows, 160, null, parameters);

        /// <summary>
        /// Runs a catalog query and returns every result set as a table, titled by `titles` when
        /// given; cells longer than `maxCell` are cut.
        /// </summary>
        private static string Tables(ToolContext session, string sql, int maxRows, int maxCell, string[] titles, params SqlParameter[] parameters)
        {
            using (var connection = session.Target.CreateConnection())
            using (var command = new SqlCommand(Marker + sql, connection) { CommandTimeout = 60 })
            {
                command.Parameters.AddRange(parameters);
                connection.Open();
                using (var reader = command.ExecuteReader())
                {
                    var sb = new StringBuilder();
                    var set = 0;
                    do
                    {
                        if (reader.FieldCount == 0)
                        {
                            continue;
                        }
                        if (sb.Length > 0)
                        {
                            sb.Append('\n');
                        }
                        if (titles != null && set < titles.Length)
                        {
                            sb.Append(titles[set]).Append(":\n");
                        }
                        sb.Append(Format(reader, maxRows, maxCell));
                        set++;
                    }
                    while (reader.NextResult());
                    return sb.ToString();
                }
            }
        }

        /// <summary>The rows of a query, for tools that work on them further.</summary>
        private static List<object[]> Rows(ToolContext session, string sql, params SqlParameter[] parameters)
        {
            var rows = new List<object[]>();
            using (var connection = session.Target.CreateConnection())
            using (var command = new SqlCommand(Marker + sql, connection) { CommandTimeout = 60 })
            {
                command.Parameters.AddRange(parameters);
                connection.Open();
                using (var reader = command.ExecuteReader())
                {
                    while (reader.Read())
                    {
                        var values = new object[reader.FieldCount];
                        reader.GetValues(values);
                        rows.Add(values.Select(v => v is DBNull ? null : v).ToArray());
                    }
                }
            }
            return rows;
        }

        /// <summary>Starts QuillSQL's own catalog queries, so the plan cache tools can leave them out.</summary>
        private const string Marker = "/* QuillSQL tool */\n";

        /// <summary>Text to find with LIKE ... ESCAPE '\', so %, _ and [ match themselves.</summary>
        private static string Literal(string text) => text.Replace("\\", "\\\\").Replace("%", "\\%").Replace("_", "\\_").Replace("[", "\\[");

        private static string Format(SqlDataReader reader, int maxRows, int maxCell = 160)
        {
            var sb = new StringBuilder();
            var columns = Enumerable.Range(0, reader.FieldCount).Select(i => reader.GetName(i)).ToArray();
            sb.Append(string.Join(" | ", columns)).Append('\n');
            var rows = 0;
            while (reader.Read())
            {
                if (rows == maxRows)
                {
                    sb.Append("(more rows not shown)\n");
                    break;
                }
                var values = new string[reader.FieldCount];
                for (var i = 0; i < reader.FieldCount; i++)
                {
                    var value = reader.IsDBNull(i) ? "NULL" : Convert.ToString(reader.GetValue(i), CultureInfo.InvariantCulture) ?? "";
                    value = value.Replace("\r", " ").Replace("\n", " ").Replace("|", "/");
                    values[i] = value.Length > maxCell ? value.Substring(0, maxCell) + "..." : value;
                }
                sb.Append(string.Join(" | ", values)).Append('\n');
                rows++;
            }
            return rows == 0 ? "(no rows)" : sb.ToString();
        }

        private static string Pattern(string text) => text.IndexOfAny(new[] { '%', '_' }) >= 0 ? text : "%" + text + "%";

        private static string Describe(ObjectDetails d)
        {
            var sb = new StringBuilder();
            sb.Append(d.Kind.ToUpperInvariant()).Append(' ').Append(d.FullName);
            if (d.Rows != null)
            {
                sb.Append(", ").Append(d.Rows.Value.ToString("N0", CultureInfo.InvariantCulture)).Append(" rows");
            }
            if (d.SizeKb != null)
            {
                sb.Append(", ").Append((d.SizeKb.Value / 1024.0).ToString("N1", CultureInfo.InvariantCulture)).Append(" MB");
            }
            sb.Append(", created ").Append(d.Created.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture))
              .Append(", modified ").Append(d.Modified.ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture)).Append('\n');
            var p = d.Partitioning;
            if (p != null)
            {
                sb.Append("Partitioned on ").Append(p.Column).Append(" by ").Append(p.Function).Append(" RANGE ").Append(p.Range)
                  .Append(" (").Append(string.Join(", ", p.Boundaries)).Append(") through scheme ").Append(p.Scheme)
                  .Append("; rows per partition: ").Append(string.Join(", ", p.Rows)).Append('\n');
            }
            if (d.Columns.Count > 0)
            {
                sb.Append("Columns:\n");
                foreach (var c in d.Columns)
                {
                    sb.Append("  ").Append(c.Name).Append(' ').Append(c.Computed != null ? "AS " + c.Computed + (c.Persisted ? " PERSISTED" : "") : c.Type)
                      .Append(c.Nullable ? " NULL" : " NOT NULL")
                      .Append(c.Identity != null ? " IDENTITY(" + c.Identity + ")" : "")
                      .Append(c.PrimaryKey ? " PRIMARY KEY" : "")
                      .Append(c.DefaultDefinition != null ? " DEFAULT " + c.DefaultDefinition : "")
                      .Append(c.References != null ? " REFERENCES " + c.References : "")
                      .Append(c.Collation != null ? " COLLATE " + c.Collation : "").Append('\n');
                }
            }
            if (d.Indexes.Count > 0)
            {
                sb.Append("Indexes:\n");
                foreach (var i in d.Indexes)
                {
                    sb.Append("  ").Append(i.Name ?? "(heap)").Append(' ').Append(i.PrimaryKey ? "PRIMARY KEY " : i.Unique ? "UNIQUE " : "").Append(i.Kind);
                    if (!string.IsNullOrEmpty(i.Keys))
                    {
                        sb.Append(" (").Append(i.Keys).Append(')');
                    }
                    if (!string.IsNullOrEmpty(i.Includes))
                    {
                        sb.Append(" INCLUDE (").Append(i.Includes).Append(')');
                    }
                    if (i.Filter != null)
                    {
                        sb.Append(" WHERE ").Append(i.Filter);
                    }
                    sb.Append('\n');
                }
            }
            foreach (var f in d.ForeignKeys)
            {
                sb.Append("Foreign key ").Append(f[0]).Append(" (").Append(f[1]).Append(") REFERENCES ").Append(f[2]).Append('.').Append(f[3])
                  .Append(" (").Append(f[4]).Append(") ON DELETE ").Append(f[5]).Append(" ON UPDATE ").Append(f[6]).Append('\n');
            }
            foreach (var c in d.Checks)
            {
                sb.Append("Check ").Append(c[0]).Append(": ").Append(c[1]).Append('\n');
            }
            if (d.Parameters.Count > 0)
            {
                sb.Append("Parameters: ").Append(string.Join(", ", d.Parameters)).Append('\n');
            }
            return sb.ToString();
        }

        private static ObjectDetails Load(ToolContext session, string name)
        {
            var details = ObjectScripts.Load(session.Target, name);
            return details ?? throw new ToolException("There is no object named " + name + " in " + session.Target.Description + ". Use search_objects to find its name.");
        }

        /// <summary>Runs `work` on the UI thread, where the editor and window frames live.</summary>
        private static T OnUiThread<T>(Func<T> work) => ThreadHelper.JoinableTaskFactory.Run(async () =>
        {
            await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
            return work();
        });

        /// <summary>One query window as the AI reads it: where it is connected, the selection, the caret, the script.</summary>
        private static string WindowText(QueryWindow window, string role)
        {
            var editor = window.Editor;
            if (editor == null)
            {
                return "Query window " + window.Name + " could not be read.";
            }
            editor.Lines.GetLineIndexOfPosition(editor.Caret, out var line, out _);
            var sb = new StringBuilder();
            sb.Append("Query window: ").Append(window.Name).Append(role).Append('\n');
            sb.Append("Connected to: ").Append(window.Connection?.Description ?? "(not connected)").Append('\n');
            sb.Append("Caret on line ").Append(line + 1).Append('\n');
            if (editor.HasSelection)
            {
                sb.Append("Selected text:\n").Append(editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart)).Append("\n\nWhole script:\n");
            }
            else
            {
                sb.Append("Script:\n");
            }
            return sb.Append(editor.Text).ToString();
        }

        /// <summary>Background waits that say nothing about a workload.</summary>
        private static readonly string[] BenignWaits =
        {
            "CHECKPOINT_QUEUE", "CHKPT", "CLR_AUTO_EVENT", "CLR_MANUAL_EVENT", "CLR_SEMAPHORE", "DBMIRROR_DBM_EVENT", "DBMIRROR_EVENTS_QUEUE",
            "DBMIRROR_WORKER_QUEUE", "DBMIRRORING_CMD", "DIRTY_PAGE_POLL", "DISPATCHER_QUEUE_SEMAPHORE", "EXECSYNC", "FSAGENT",
            "FT_IFTS_SCHEDULER_IDLE_WAIT", "FT_IFTSHC_MUTEX", "KSOURCE_WAKEUP", "LAZYWRITER_SLEEP", "LOGMGR_QUEUE", "MEMORY_ALLOCATION_EXT",
            "ONDEMAND_TASK_QUEUE", "PARALLEL_REDO_DRAIN_WORKER", "PARALLEL_REDO_LOG_CACHE", "PARALLEL_REDO_TRAN_LIST", "PARALLEL_REDO_WORKER_SYNC",
            "PARALLEL_REDO_WORKER_WAIT_WORK", "PREEMPTIVE_XE_GETTARGETSTATE", "PWAIT_ALL_COMPONENTS_INITIALIZED", "PWAIT_DIRECTLOGCONSUMER_GETNEXT",
            "REDO_THREAD_PENDING_WORK", "REQUEST_FOR_DEADLOCK_SEARCH", "RESOURCE_QUEUE", "SERVER_IDLE_CHECK", "SNI_HTTP_ACCEPT", "SOS_WORK_DISPATCHER",
            "SP_SERVER_DIAGNOSTICS_SLEEP", "SQLTRACE_BUFFER_FLUSH", "SQLTRACE_INCREMENTAL_FLUSH_SLEEP", "SQLTRACE_WAIT_ENTRIES", "UCS_SESSION_REGISTRATION",
            "VDI_CLIENT_OTHER", "WAIT_FOR_RESULTS", "WAITFOR", "WAITFOR_TASKSHUTDOWN", "WAIT_XTP_RECOVERY", "WAIT_XTP_HOST_WAIT",
            "WAIT_XTP_OFFLINE_CKPT_NEW_LOG", "WAIT_XTP_CKPT_CLOSE", "PVS_PREALLOCATE", "SOS_WORKER_MIGRATION", "PREEMPTIVE_OS_FLUSHFILEBUFFERS",
        };

        private static readonly Tool[] Tools =
        {
            new Tool
            {
                Name = "current_query",
                Description = "The query windows this conversation is about, chosen by the developer (or only the one named in window): tab name, the server and database it is connected to, the selected text if any, the caret line and the whole script. 'This query' or 'my query' means the first one; database tools use the window you name, or the first.",
                Parameters = new string[0][],
                NeedsDatabase = false,
                Run = (s, a) => OnUiThread(() =>
                {
                    if (s.Windows.Count == 0)
                    {
                        return "The query windows of this conversation were closed. Ask the developer to choose one above the chat box.";
                    }
                    var only = Text(a, "window", false);
                    var windows = only == null ? s.Windows : s.Windows.Where(w => ReferenceEquals(w.Frame, s.Window)).ToList();
                    return string.Join("\n\n----\n\n", windows.Select((w, i) => WindowText(w, i == 0 && only == null ? " (the first window of this conversation)" : " (a window of this conversation)")));
                }),
            },
            new Tool
            {
                Name = "open_query_windows",
                Description = "Lists the open query windows: tab name, connection, number of lines and first line of each, marking the ones this conversation is about. Database tools work only on those.",
                Parameters = new string[0][],
                NeedsDatabase = false,
                Run = (s, a) => OnUiThread(() =>
                {
                    var windows = QueryWindows.All();
                    if (windows.Count == 0)
                    {
                        return "No query window is open.";
                    }
                    var sb = new StringBuilder();
                    foreach (var w in windows)
                    {
                        var script = w.Editor?.Text ?? "";
                        var first = script.Split('\n').Select(l => l.Trim()).FirstOrDefault(l => l.Length > 0) ?? "(empty)";
                        sb.Append(s.Windows.Any(c => ReferenceEquals(c.Frame, w.Frame)) ? "* " : "- ").Append(w.Name).Append(" | ").Append(w.Connection?.Description ?? "(not connected)")
                          .Append(" | ").Append(script.Split('\n').Length).Append(" lines | ").Append(first.Length > 100 ? first.Substring(0, 100) + "..." : first).Append('\n');
                    }
                    return sb.Append("(* marks the windows of this conversation)").ToString();
                }),
            },
            new Tool
            {
                Name = "read_query_window",
                Description = "The script of any open query window, by part of its tab name. Database tools work only on the conversation's windows; if this one is not among them, ask the developer to add it.",
                Parameters = new[] { new[] { "name", "string", "Part of the tab name, for example SQLQuery3.", "required" } },
                NeedsDatabase = false,
                Run = (s, a) =>
                {
                    var name = Text(a, "name");
                    return OnUiThread(() =>
                    {
                        var window = QueryWindows.All().FirstOrDefault(w => w.Caption.IndexOf(name, StringComparison.OrdinalIgnoreCase) >= 0);
                        return window == null ? throw new ToolException("No open query window has '" + name + "' in its name.")
                            : WindowText(window, s.Windows.Any(c => ReferenceEquals(c.Frame, window.Frame)) ? " (a window of this conversation)" : " (not one of this conversation's windows)");
                    });
                },
            },            new Tool
            {
                Name = "search_objects",
                Description = "Finds tables, views, procedures and functions whose name contains a text (or matches a LIKE pattern), with the row count of tables.",
                Parameters = new[] { new[] { "pattern", "string", "Part of the name, or a LIKE pattern such as 'Order%'.", "required" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (60) SCHEMA_NAME(o.schema_id) + '.' + o.name AS name, o.type_desc AS kind,
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)) AS rows
FROM sys.objects AS o
WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V', 'P', 'FN', 'IF', 'TF', 'SN')
  AND (o.name LIKE @p OR SCHEMA_NAME(o.schema_id) + '.' + o.name LIKE @p)
ORDER BY CASE WHEN o.name = @n THEN 0 ELSE 1 END, o.name;", 60,
                    new SqlParameter("@p", Pattern(Text(a, "pattern"))), new SqlParameter("@n", Text(a, "pattern"))),
            },
            new Tool
            {
                Name = "find_columns",
                Description = "Finds columns whose name contains a text, with their table and type: useful to find where a key or value is stored.",
                Parameters = new[] { new[] { "pattern", "string", "Part of the column name, or a LIKE pattern.", "required" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (100) SCHEMA_NAME(o.schema_id) + '.' + o.name AS [table], c.name AS [column], TYPE_NAME(c.user_type_id) AS type, c.is_nullable AS nullable
FROM sys.columns AS c JOIN sys.objects AS o ON o.object_id = c.object_id
WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V') AND c.name LIKE @p
ORDER BY o.name, c.column_id;", 100, new SqlParameter("@p", Pattern(Text(a, "pattern")))),
            },
            new Tool
            {
                Name = "describe_object",
                Description = "Describes a table, view, procedure or function: row count, size, columns with types and keys, indexes, foreign keys, checks, partitioning and parameters.",
                Parameters = new[] { new[] { "name", "string", "schema.name, for example dbo.Orders.", "required" } },
                Run = (s, a) => Describe(Load(s, Text(a, "name"))),
            },
            new Tool
            {
                Name = "get_definition",
                Description = "The source of a view, procedure, function or trigger, or a CREATE TABLE script for a table.",
                Parameters = new[] { new[] { "name", "string", "schema.name.", "required" } },
                Run = (s, a) => ObjectScripts.Script(Load(s, Text(a, "name")), false) ?? "(no definition)",
            },
            new Tool
            {
                Name = "index_usage",
                Description = "How each index of a table is used since the server started (seeks, scans, lookups, updates, last use) and its size: finds unused or heavily updated indexes.",
                Parameters = new[] { new[] { "table", "string", "schema.name of the table.", "required" } },
                Run = (s, a) => Table(s, @"
SELECT i.name, i.type_desc AS kind, s.user_seeks AS seeks, s.user_scans AS scans, s.user_lookups AS lookups, s.user_updates AS updates,
    s.last_user_seek, s.last_user_scan,
    (SELECT CAST(SUM(a.used_pages) * 8 / 1024.0 AS decimal(18, 1)) FROM sys.partitions AS p JOIN sys.allocation_units AS a ON a.container_id = p.partition_id
     WHERE p.object_id = i.object_id AND p.index_id = i.index_id) AS used_mb
FROM sys.indexes AS i
LEFT JOIN sys.dm_db_index_usage_stats AS s ON s.object_id = i.object_id AND s.index_id = i.index_id AND s.database_id = DB_ID()
WHERE i.object_id = OBJECT_ID(@t)
ORDER BY i.index_id;", 100, new SqlParameter("@t", Text(a, "table"))),
            },
            new Tool
            {
                Name = "missing_indexes",
                Description = "SQL Server's missing-index suggestions for the database or one table, by estimated impact. Treat them as hints: check existing indexes first.",
                Parameters = new[] { new[] { "table", "string", "schema.name to limit to one table; omit for the whole database.", "" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (25) OBJECT_SCHEMA_NAME(d.object_id, d.database_id) + '.' + OBJECT_NAME(d.object_id, d.database_id) AS [table],
    d.equality_columns, d.inequality_columns, d.included_columns, gs.user_seeks AS seeks,
    CAST(gs.avg_total_user_cost AS decimal(18, 2)) AS avg_cost, gs.avg_user_impact AS impact_percent, gs.last_user_seek
FROM sys.dm_db_missing_index_details AS d
JOIN sys.dm_db_missing_index_groups AS g ON g.index_handle = d.index_handle
JOIN sys.dm_db_missing_index_group_stats AS gs ON gs.group_handle = g.index_group_handle
WHERE d.database_id = DB_ID() AND (@t IS NULL OR d.object_id = OBJECT_ID(@t))
ORDER BY gs.user_seeks * gs.avg_total_user_cost * gs.avg_user_impact DESC;", 25, new SqlParameter("@t", (object)Text(a, "table", false) ?? DBNull.Value)),
            },
            new Tool
            {
                Name = "statistics",
                Description = "The statistics of a table: columns, when each was last updated, rows sampled, and rows modified since.",
                Parameters = new[] { new[] { "table", "string", "schema.name of the table.", "required" } },
                Run = (s, a) => Table(s, @"
SELECT st.name, STUFF((SELECT ', ' + c.name FROM sys.stats_columns AS sc JOIN sys.columns AS c ON c.object_id = sc.object_id AND c.column_id = sc.column_id
                     WHERE sc.object_id = st.object_id AND sc.stats_id = st.stats_id ORDER BY sc.stats_column_id FOR XML PATH('')), 1, 2, '') AS columns,
    st.auto_created, p.last_updated, p.rows, p.rows_sampled, p.modification_counter AS modified_since
FROM sys.stats AS st CROSS APPLY sys.dm_db_stats_properties(st.object_id, st.stats_id) AS p
WHERE st.object_id = OBJECT_ID(@t)
ORDER BY st.stats_id;", 100, new SqlParameter("@t", Text(a, "table"))),
            },
            new Tool
            {
                Name = "estimated_plan",
                Description = "Compiles SQL without running it and returns a summary of its estimated execution plan (operators, estimated rows and costs, warnings, missing-index hints), with the indexes of the tables it uses. It uses the connection of the window you name (or the first); get the SQL from current_query.",
                Parameters = new[] { new[] { "sql", "string", "The SQL to compile.", "required" } },
                Run = (s, a) =>
                {
                    var sql = Text(a, "sql");
                    var target = AiAssistant.Target(s.Key, sql);
                    var errors = new List<string>();
                    var plans = PlanCapture.Plans(s.Target, target.Batches ?? new string[0], false, errors);
                    return "Estimated plan:\n" + AiAssistant.Summarize(plans, 150)
                        + (errors.Count == 0 ? "" : "\nErrors:\n" + string.Join("\n", errors))
                        + "\n\nTables, row counts and indexes:\n" + PlanCapture.Indexes(s.Target, target.Objects ?? new string[0]);
                },
            },
            new Tool
            {
                Name = "query_store_top",
                Description = "The most expensive queries of the last days from Query Store, with durations, CPU, reads, executions and plan counts; the slowest plan of the first three.",
                Parameters = new[]
                {
                    new[] { "days", "integer", "How many days back (1 to 30, default 7).", "" },
                    new[] { "top", "integer", "How many queries (1 to 25, default 10).", "" },
                },
                Run = (s, a) =>
                {
                    var queries = PlanCapture.QueryStore(s.Target, Number(a, "days", 7, 1, 30), Number(a, "top", 10, 1, 25));
                    if (queries.Count == 0)
                    {
                        return "Query Store has no queries for that period (it may be turned off for this database).";
                    }
                    var sb = new StringBuilder();
                    for (var i = 0; i < queries.Count; i++)
                    {
                        sb.Append(i + 1).Append(". ").Append(PlanCapture.Describe(queries[i])).Append('\n');
                        if (i < 3 && queries[i].WorstPlanXml != null)
                        {
                            sb.Append("Slowest plan:\n").Append(AiAssistant.Summarize(new[] { queries[i].WorstPlanXml }, 40)).Append('\n');
                        }
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "current_activity",
                Description = "The requests running on the server now: duration, status, wait type, blocking session, CPU, reads and the statement. Use it for 'why is it slow right now' and blocking.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (25) r.session_id, r.blocking_session_id AS blocked_by, r.status, r.command, r.wait_type, r.wait_time AS wait_ms,
    r.total_elapsed_time AS elapsed_ms, r.cpu_time AS cpu_ms, r.logical_reads, DB_NAME(r.database_id) AS [database],
    SUBSTRING(t.text, r.statement_start_offset / 2 + 1,
        (CASE WHEN r.statement_end_offset = -1 THEN DATALENGTH(t.text) ELSE r.statement_end_offset END - r.statement_start_offset) / 2 + 1) AS statement
FROM sys.dm_exec_requests AS r CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) AS t
WHERE r.session_id <> @@SPID AND r.session_id > 50
ORDER BY r.total_elapsed_time DESC;", 25),
            },
            new Tool
            {
                Name = "server_info",
                Description = "The server version and edition, and the database's compatibility level, recovery model, isolation settings, size, MAXDOP and Query Store state.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT @@VERSION AS version, SERVERPROPERTY('Edition') AS edition, DB_NAME() AS [database], d.compatibility_level, d.recovery_model_desc AS recovery,
    d.is_read_committed_snapshot_on AS rcsi, d.snapshot_isolation_state_desc AS snapshot_isolation, d.is_query_store_on AS query_store,
    (SELECT CAST(SUM(CAST(size AS bigint)) * 8 / 1024 AS bigint) FROM sys.database_files) AS size_mb,
    (SELECT CAST(value AS int) FROM sys.database_scoped_configurations WHERE name = 'MAXDOP') AS maxdop
FROM sys.databases AS d WHERE d.database_id = DB_ID();", 5),
            },
            new Tool
            {
                Name = "dependencies",
                Description = "What uses an object and what it uses: procedures, views, functions and triggers that reference it, the objects it references, foreign keys pointing at it and synonyms of it. Dynamic SQL is not tracked; for one column, use search_definitions with the column name.",
                Parameters = new[] { new[] { "name", "string", "schema.name of a table, view, procedure or function.", "required" } },
                Run = (s, a) => Table(s, @"
DECLARE @id int = OBJECT_ID(@n);
IF @id IS NULL THROW 50000, 'No object with that name; use search_objects to find it.', 1;
SELECT TOP (300) direction, object, kind FROM (
    SELECT 'used by' COLLATE DATABASE_DEFAULT AS direction, (OBJECT_SCHEMA_NAME(d.referencing_id) + '.' + OBJECT_NAME(d.referencing_id)) COLLATE DATABASE_DEFAULT AS object, o.type_desc COLLATE DATABASE_DEFAULT AS kind
    FROM sys.sql_expression_dependencies AS d JOIN sys.objects AS o ON o.object_id = d.referencing_id
    WHERE d.referenced_id = @id
    UNION
    SELECT 'uses', (COALESCE(d.referenced_database_name + '.', '') + COALESCE(d.referenced_schema_name, OBJECT_SCHEMA_NAME(d.referenced_id), '?') + '.' + d.referenced_entity_name) COLLATE DATABASE_DEFAULT,
        COALESCE(o.type_desc, CASE WHEN d.referenced_database_name IS NOT NULL THEN 'in another database' ELSE 'not found' END) COLLATE DATABASE_DEFAULT
    FROM sys.sql_expression_dependencies AS d LEFT JOIN sys.objects AS o ON o.object_id = d.referenced_id
    WHERE d.referencing_id = @id
    UNION
    SELECT 'foreign key from', (OBJECT_SCHEMA_NAME(f.parent_object_id) + '.' + OBJECT_NAME(f.parent_object_id)) COLLATE DATABASE_DEFAULT, ('FOREIGN KEY ' + f.name) COLLATE DATABASE_DEFAULT
    FROM sys.foreign_keys AS f WHERE f.referenced_object_id = @id
    UNION
    SELECT 'trigger on it', (OBJECT_SCHEMA_NAME(tr.object_id) + '.' + tr.name) COLLATE DATABASE_DEFAULT, 'TRIGGER' FROM sys.triggers AS tr WHERE tr.parent_id = @id
    UNION
    SELECT 'synonym of it', (SCHEMA_NAME(sy.schema_id) + '.' + sy.name) COLLATE DATABASE_DEFAULT, 'SYNONYM' FROM sys.synonyms AS sy WHERE OBJECT_ID(sy.base_object_name) = @id
) AS x ORDER BY direction, object;", 300, new SqlParameter("@n", Text(a, "name"))),
            },
            new Tool
            {
                Name = "search_definitions",
                Description = "Finds procedures, views, functions and triggers whose source contains a text (a column name, a table name, a literal such as 'Status = 5', NOLOCK...), with the line of the first match and the code around it.",
                Parameters = new[] { new[] { "text", "string", "The text to find, as written in the code.", "required" } },
                Run = (s, a) => Tables(s, @"
SELECT TOP (60) OBJECT_SCHEMA_NAME(m.object_id) + '.' + OBJECT_NAME(m.object_id) AS object, o.type_desc AS kind,
    LEN(LEFT(m.definition, CHARINDEX(@t, m.definition))) - LEN(REPLACE(LEFT(m.definition, CHARINDEX(@t, m.definition)), CHAR(10), '')) + 1 AS line,
    SUBSTRING(m.definition, CASE WHEN CHARINDEX(@t, m.definition) > 80 THEN CHARINDEX(@t, m.definition) - 80 ELSE 1 END, LEN(@t) + 160) AS around
FROM sys.sql_modules AS m JOIN sys.objects AS o ON o.object_id = m.object_id
WHERE o.is_ms_shipped = 0 AND m.definition LIKE '%' + @e + '%' ESCAPE '\'
ORDER BY object;", 60, 260, null, new SqlParameter("@t", Text(a, "text")), new SqlParameter("@e", Literal(Text(a, "text")))),
            },
            new Tool
            {
                Name = "plan_cache_top",
                Description = "The most expensive statements in the plan cache for this database (what runs now, also when Query Store is off): executions, average and total duration, CPU, reads, the procedure and the statement; the plans of the first three.",
                Parameters = new[]
                {
                    new[] { "order_by", "string", "duration (default), cpu, reads or executions.", "" },
                    new[] { "top", "integer", "How many statements (1 to 25, default 10).", "" },
                },
                Run = (s, a) =>
                {
                    var rows = Rows(s, @"
SELECT TOP (@top) qs.execution_count,
    CAST(qs.total_elapsed_time / 1000.0 / qs.execution_count AS decimal(18, 1)),
    CAST(qs.total_worker_time / 1000.0 / qs.execution_count AS decimal(18, 1)),
    qs.total_logical_reads / qs.execution_count,
    CAST(qs.total_elapsed_time / 1000000.0 AS decimal(18, 1)),
    qs.last_execution_time, qs.creation_time,
    OBJECT_SCHEMA_NAME(st.objectid, st.dbid) + '.' + OBJECT_NAME(st.objectid, st.dbid),
    SUBSTRING(st.text, qs.statement_start_offset / 2 + 1,
        (CASE WHEN qs.statement_end_offset = -1 THEN DATALENGTH(st.text) ELSE qs.statement_end_offset END - qs.statement_start_offset) / 2 + 1),
    (SELECT qp.query_plan FROM sys.dm_exec_text_query_plan(qs.plan_handle, qs.statement_start_offset, qs.statement_end_offset) AS qp)
FROM sys.dm_exec_query_stats AS qs
CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) AS st
CROSS APPLY (SELECT CONVERT(int, pa.value) AS dbid FROM sys.dm_exec_plan_attributes(qs.plan_handle) AS pa WHERE pa.attribute = 'dbid') AS db
WHERE db.dbid = DB_ID() AND st.text NOT LIKE '%/* QuillSQL tool */%'
ORDER BY CASE @o WHEN 'cpu' THEN qs.total_worker_time WHEN 'reads' THEN qs.total_logical_reads WHEN 'executions' THEN qs.execution_count ELSE qs.total_elapsed_time END DESC;",
                        new SqlParameter("@top", Number(a, "top", 10, 1, 25)), new SqlParameter("@o", (Text(a, "order_by", false) ?? "duration").ToLowerInvariant()));
                    if (rows.Count == 0)
                    {
                        return "The plan cache has no statements for this database (it may have been cleared, or the server restarted).";
                    }
                    var sb = new StringBuilder("Plan cache, most expensive first (times in ms; total in seconds since cached):\n");
                    for (var i = 0; i < rows.Count; i++)
                    {
                        var r = rows[i];
                        var statement = Convert.ToString(r[8], CultureInfo.InvariantCulture) ?? "";
                        sb.Append('\n').Append(i + 1).Append(". executions ").Append(r[0]).Append(", avg ").Append(r[1]).Append(" ms, avg CPU ").Append(r[2])
                          .Append(" ms, avg reads ").Append(r[3]).Append(", total ").Append(r[4]).Append(" s, last run ").Append(r[5]).Append(", cached since ").Append(r[6])
                          .Append(r[7] != null ? ", in " + r[7] : "").Append('\n')
                          .Append(statement.Length > 1500 ? statement.Substring(0, 1500) + "..." : statement).Append('\n');
                        if (i < 3 && r[9] is string plan && plan.Length > 0)
                        {
                            sb.Append("Plan:\n").Append(AiAssistant.Summarize(new[] { plan }, 40)).Append('\n');
                        }
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "query_store_query",
                Description = "The history of one Query Store query: its text, each plan with executions and average duration, CPU and reads, the daily trend per plan, and the fastest and slowest plans side by side. Use it to find when and why a query got slower (plan regression).",
                Parameters = new[]
                {
                    new[] { "query_id", "integer", "The query_id, from query_store_top or query_store_search.", "required" },
                    new[] { "days", "integer", "Days of daily trend (1 to 60, default 14).", "" },
                },
                Run = (s, a) =>
                {
                    var id = Number(a, "query_id", 0, 0, int.MaxValue);
                    var history = Tables(s, @"
SELECT q.query_id, OBJECT_SCHEMA_NAME(q.object_id) + '.' + OBJECT_NAME(q.object_id) AS [procedure], qt.query_sql_text AS text
FROM sys.query_store_query AS q JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id WHERE q.query_id = @q;
SELECT p.plan_id, p.is_forced_plan AS forced, SUM(rs.count_executions) AS executions,
    CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)) AS avg_ms,
    CAST(SUM(rs.avg_cpu_time * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)) AS avg_cpu_ms,
    CAST(SUM(rs.avg_logical_io_reads * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) AS bigint) AS avg_reads,
    MIN(rs.first_execution_time) AS first_run, MAX(rs.last_execution_time) AS last_run
FROM sys.query_store_plan AS p LEFT JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
WHERE p.query_id = @q GROUP BY p.plan_id, p.is_forced_plan ORDER BY last_run DESC;
SELECT CAST(i.start_time AS date) AS day, rs.plan_id, SUM(rs.count_executions) AS executions,
    CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)) AS avg_ms
FROM sys.query_store_runtime_stats AS rs
JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
JOIN sys.query_store_plan AS p ON p.plan_id = rs.plan_id
WHERE p.query_id = @q AND i.start_time >= DATEADD(day, -@days, SYSUTCDATETIME())
GROUP BY CAST(i.start_time AS date), rs.plan_id ORDER BY day, rs.plan_id;", 120, 4000, new[] { "Query", "Plans", "Daily trend" },
                        new SqlParameter("@q", (long)id), new SqlParameter("@days", Number(a, "days", 14, 1, 60)));
                    // The fastest and slowest plans, to see what changed.
                    var plans = Rows(s, @"
SELECT p.plan_id, CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)), p.query_plan
FROM sys.query_store_plan AS p JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
WHERE p.query_id = @q GROUP BY p.plan_id, p.query_plan ORDER BY 2;", new SqlParameter("@q", (long)id));
                    var sb = new StringBuilder(history);
                    if (plans.Count > 0)
                    {
                        var fast = plans.First();
                        sb.Append("\nFastest plan ").Append(fast[0]).Append(" (avg ").Append(fast[1]).Append(" ms):\n").Append(AiAssistant.Summarize(new[] { (string)fast[2] }, 40)).Append('\n');
                        if (plans.Count > 1)
                        {
                            var slow = plans.Last();
                            sb.Append("\nSlowest plan ").Append(slow[0]).Append(" (avg ").Append(slow[1]).Append(" ms):\n").Append(AiAssistant.Summarize(new[] { (string)slow[2] }, 40)).Append('\n');
                        }
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "query_store_search",
                Description = "Finds Query Store queries whose text contains a text (a table, procedure or column name), with executions, average duration, number of plans and last run, most costly first.",
                Parameters = new[]
                {
                    new[] { "text", "string", "The text to find in the query text.", "required" },
                    new[] { "days", "integer", "Only queries run in the last days (1 to 60, default 7).", "" },
                },
                Run = (s, a) => Tables(s, @"
SELECT TOP (25) q.query_id, OBJECT_SCHEMA_NAME(q.object_id) + '.' + OBJECT_NAME(q.object_id) AS [procedure], SUM(rs.count_executions) AS executions,
    CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)) AS avg_ms,
    COUNT(DISTINCT p.plan_id) AS plans, MAX(rs.last_execution_time) AS last_run, qt.query_sql_text AS text
FROM sys.query_store_query_text AS qt
JOIN sys.query_store_query AS q ON q.query_text_id = qt.query_text_id
JOIN sys.query_store_plan AS p ON p.query_id = q.query_id
JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
WHERE qt.query_sql_text LIKE '%' + @e + '%' ESCAPE '\' AND rs.last_execution_time >= DATEADD(day, -@days, SYSUTCDATETIME())
GROUP BY q.query_id, q.object_id, qt.query_sql_text
ORDER BY SUM(rs.avg_duration * rs.count_executions) DESC;", 25, 600, null,
                    new SqlParameter("@e", Literal(Text(a, "text"))), new SqlParameter("@days", Number(a, "days", 7, 1, 60))),
            },
            new Tool
            {
                Name = "wait_stats",
                Description = "What the database (Azure SQL Database) or the server has been waiting on, cumulative since it started: top wait types with total and average wait and the share of all waits, harmless background waits left out. The first thing to check when everything is slow.",
                Parameters = new string[0][],
                Run = (s, a) =>
                {
                    var azure = Convert.ToInt32(Rows(s, "SELECT CAST(SERVERPROPERTY('EngineEdition') AS int);")[0][0], CultureInfo.InvariantCulture) == 5;
                    var since = azure ? "Waits of this database since it was started or last scaled." : "Waits of the whole server since " + Rows(s, "SELECT sqlserver_start_time FROM sys.dm_os_sys_info;")[0][0] + ".";
                    return since + "\n" + Table(s, @"
SELECT TOP (15) wait_type, waiting_tasks_count AS waits, CAST(wait_time_ms / 1000.0 AS decimal(18, 1)) AS wait_s,
    CAST(100.0 * wait_time_ms / SUM(wait_time_ms) OVER () AS decimal(5, 1)) AS share_percent,
    CAST(wait_time_ms * 1.0 / NULLIF(waiting_tasks_count, 0) AS decimal(18, 1)) AS avg_ms,
    CAST(signal_wait_time_ms / 1000.0 AS decimal(18, 1)) AS cpu_queue_s
FROM " + (azure ? "sys.dm_db_wait_stats" : "sys.dm_os_wait_stats") + @"
WHERE wait_time_ms > 0 AND wait_type NOT IN (" + string.Join(", ", BenignWaits.Select(w => "'" + w + "'")) + @")
    AND wait_type NOT LIKE 'SLEEP[_]%' AND wait_type NOT LIKE 'QDS[_]%' AND wait_type NOT LIKE 'HADR[_]%' AND wait_type NOT LIKE 'XE[_]%' AND wait_type NOT LIKE 'BROKER[_]%'
ORDER BY wait_time_ms DESC;", 15);
                },
            },
            new Tool
            {
                Name = "measure_query",
                Description = "Runs a read-only query once and measures it: reads per table (STATISTICS IO), CPU and elapsed time (STATISTICS TIME), rows returned, and the actual execution plan with estimated against actual rows. Rows are not returned. Only allowed when the developer has turned on 'Let AI read data'.",
                Parameters = new[] { new[] { "sql", "string", "The read-only SQL to measure.", "required" } },
                Run = (s, a) =>
                {
                    if (!s.AllowData)
                    {
                        throw new ToolException("Measuring runs the query, so it is turned off. Use estimated_plan, or ask the developer to turn on 'Let AI read data' below the chat box.");
                    }
                    var sql = Text(a, "sql");
                    var target = AiAssistant.Target(s.Key, sql);
                    if (target.WriteReason != null)
                    {
                        throw new ToolException("Not run: only read-only queries can be measured, and " + target.WriteReason + ".");
                    }
                    var messages = new List<string>();
                    var plans = new List<string>();
                    var rowCounts = new List<long>();
                    using (var connection = s.Target.CreateConnection())
                    {
                        connection.InfoMessage += (o, e) => { foreach (SqlError m in e.Errors) { messages.Add(m.Message.Trim()); } };
                        connection.Open();
                        using (var setup = new SqlCommand("SET LOCK_TIMEOUT 5000; SET STATISTICS IO ON; SET STATISTICS TIME ON; SET STATISTICS XML ON;", connection))
                        {
                            setup.ExecuteNonQuery();
                        }
                        using (var transaction = connection.BeginTransaction())
                        {
                            foreach (var batch in target.Batches ?? new[] { sql })
                            {
                                using (var command = new SqlCommand(batch, connection, transaction) { CommandTimeout = 120 })
                                using (var reader = command.ExecuteReader())
                                {
                                    do
                                    {
                                        if (reader.FieldCount == 1 && reader.GetName(0).IndexOf("Showplan", StringComparison.OrdinalIgnoreCase) >= 0)
                                        {
                                            while (reader.Read())
                                            {
                                                plans.Add(reader.GetString(0));
                                            }
                                        }
                                        else if (reader.FieldCount > 0)
                                        {
                                            long n = 0;
                                            while (reader.Read())
                                            {
                                                n++;
                                            }
                                            rowCounts.Add(n);
                                        }
                                    }
                                    while (reader.NextResult());
                                }
                            }
                            transaction.Rollback();
                        }
                    }
                    var useful = messages.Where(m => m.StartsWith("Table ", StringComparison.Ordinal) || m.Contains("Execution Times") || m.Contains("CPU time")).ToList();
                    return "Rows returned per result set: " + (rowCounts.Count == 0 ? "none" : string.Join(", ", rowCounts)) + "\n\n"
                        + "Statistics IO and TIME:\n" + (useful.Count == 0 ? "(none reported)" : string.Join("\n", useful.Take(80))) + "\n\n"
                        + "Actual plan:\n" + AiAssistant.Summarize(plans, 150);
                },
            },
            new Tool
            {
                Name = "stats_histogram",
                Description = "The histogram of a statistic: when it was updated, rows sampled, and its steps (range high key, rows equal to it, rows and distinct values below it). Use it to explain wrong row estimates and parameter sniffing.",
                Parameters = new[]
                {
                    new[] { "table", "string", "schema.name of the table.", "required" },
                    new[] { "statistic", "string", "The statistic or index name, or a column name (its statistic is used).", "required" },
                },
                Run = (s, a) => Tables(s, @"
DECLARE @id int = OBJECT_ID(@t);
IF @id IS NULL THROW 50000, 'No table with that name; use search_objects to find it.', 1;
DECLARE @s int = (SELECT TOP (1) st.stats_id FROM sys.stats AS st
    LEFT JOIN sys.stats_columns AS sc ON sc.object_id = st.object_id AND sc.stats_id = st.stats_id AND sc.stats_column_id = 1
    LEFT JOIN sys.columns AS c ON c.object_id = sc.object_id AND c.column_id = sc.column_id
    WHERE st.object_id = @id AND (st.name = @n OR c.name = @n)
    ORDER BY CASE WHEN st.name = @n THEN 0 ELSE 1 END, st.stats_id);
IF @s IS NULL THROW 50000, 'No statistic or leading column with that name on the table; use the statistics tool to list them.', 1;
SELECT st.name AS statistic, p.last_updated, p.rows, p.rows_sampled, p.steps, p.modification_counter AS modified_since
FROM sys.stats AS st CROSS APPLY sys.dm_db_stats_properties(st.object_id, st.stats_id) AS p WHERE st.object_id = @id AND st.stats_id = @s;
SELECT h.step_number AS step, CONVERT(nvarchar(200), h.range_high_key) AS range_high_key, h.equal_rows, h.range_rows, h.distinct_range_rows,
    CAST(h.average_range_rows AS decimal(18, 2)) AS average_range_rows
FROM sys.dm_db_stats_histogram(@id, @s) AS h ORDER BY h.step_number;", 210, 200, new[] { "Statistic", "Histogram" },
                    new SqlParameter("@t", Text(a, "table")), new SqlParameter("@n", Text(a, "statistic"))),
            },
            new Tool
            {
                Name = "largest_tables",
                Description = "The largest tables of the database: rows, reserved space, data and index size in MB.",
                Parameters = new[] { new[] { "top", "integer", "How many tables (1 to 50, default 20).", "" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (@top) SCHEMA_NAME(o.schema_id) + '.' + o.name AS [table],
    SUM(CASE WHEN p.index_id IN (0, 1) AND a.type = 1 THEN p.rows ELSE 0 END) AS rows,
    CAST(SUM(a.total_pages) * 8 / 1024.0 AS decimal(18, 1)) AS reserved_mb,
    CAST(SUM(CASE WHEN p.index_id IN (0, 1) THEN a.used_pages ELSE 0 END) * 8 / 1024.0 AS decimal(18, 1)) AS data_mb,
    CAST(SUM(CASE WHEN p.index_id > 1 THEN a.used_pages ELSE 0 END) * 8 / 1024.0 AS decimal(18, 1)) AS index_mb
FROM sys.objects AS o
JOIN sys.partitions AS p ON p.object_id = o.object_id
JOIN sys.allocation_units AS a ON a.container_id = p.partition_id
WHERE o.type = 'U' AND o.is_ms_shipped = 0
GROUP BY o.schema_id, o.name
ORDER BY SUM(a.total_pages) DESC;", 50, new SqlParameter("@top", Number(a, "top", 20, 1, 50))),
            },
            new Tool
            {
                Name = "index_fragmentation",
                Description = "Fragmentation and page count of each index (and partition) of one table, read in the light LIMITED mode.",
                Parameters = new[] { new[] { "table", "string", "schema.name of the table.", "required" } },
                Run = (s, a) => Table(s, @"
DECLARE @id int = OBJECT_ID(@t);
IF @id IS NULL THROW 50000, 'No table with that name; use search_objects to find it.', 1;
SELECT i.name AS [index], ps.index_type_desc AS kind, ps.partition_number AS partition,
    CAST(ps.avg_fragmentation_in_percent AS decimal(5, 1)) AS fragmentation_percent, ps.page_count AS pages, ps.fragment_count AS fragments
FROM sys.dm_db_index_physical_stats(DB_ID(), @id, NULL, NULL, 'LIMITED') AS ps
LEFT JOIN sys.indexes AS i ON i.object_id = ps.object_id AND i.index_id = ps.index_id
ORDER BY ps.index_id, ps.partition_number;", 200, new SqlParameter("@t", Text(a, "table"))),
            },            new Tool
            {
                Name = "run_query",
                Description = "Runs a read-only SELECT and returns up to max_rows rows. Only allowed when the user has turned on 'Let AI read data'; prefer the other tools for metadata, and keep queries small (TOP, WHERE).",
                Parameters = new[]
                {
                    new[] { "sql", "string", "One read-only SELECT statement.", "required" },
                    new[] { "max_rows", "integer", "Rows to return (1 to 200, default 50).", "" },
                },
                Run = (s, a) =>
                {
                    if (!s.AllowData)
                    {
                        throw new ToolException("Reading data is turned off. If the rows are really needed, say so and ask the developer to turn on 'Let AI read data' below the chat box.");
                    }
                    var sql = Text(a, "sql");
                    var target = AiAssistant.Target(s.Key, sql);
                    if (target.WriteReason != null)
                    {
                        throw new ToolException("Not run: only read-only queries are allowed, and " + target.WriteReason + ".");
                    }
                    using (var connection = s.Target.CreateConnection())
                    {
                        connection.Open();
                        using (var setup = new SqlCommand("SET LOCK_TIMEOUT 5000;", connection))
                        {
                            setup.ExecuteNonQuery();
                        }
                        // Inside a transaction that is always rolled back, should anything still write.
                        using (var transaction = connection.BeginTransaction())
                        using (var command = new SqlCommand(sql, connection, transaction) { CommandTimeout = 30 })
                        {
                            string result;
                            using (var reader = command.ExecuteReader())
                            {
                                result = Format(reader, Number(a, "max_rows", 50, 1, 200));
                            }
                            transaction.Rollback();
                            return result;
                        }
                    }
                },
            },
        };
    }
}
