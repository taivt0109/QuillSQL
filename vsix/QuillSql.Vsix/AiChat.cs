using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.Serialization;
using System.Runtime.Serialization.Json;
using System.Text;
using System.Threading;

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class ChatMessage
    {
        /// <summary>"user", "assistant" or "error".</summary>
        [DataMember(Name = "role")] public string Role { get; set; }
        [DataMember(Name = "text")] public string Text { get; set; }
        /// <summary>What was sent with a user message, as short labels such as "Query (24 lines)".</summary>
        [DataMember(Name = "attached")] public List<string> Attached { get; set; } = new List<string>();
        /// <summary>The context sent with a user message, shown on request.</summary>
        [DataMember(Name = "context")] public string Context { get; set; }
        [DataMember(Name = "time")] public DateTime Time { get; set; } = DateTime.Now;
    }

    /// <summary>One conversation on one topic, saved in %LOCALAPPDATA%\QuillSQL\chats.</summary>
    [DataContract]
    internal sealed class ChatThread
    {
        [DataMember(Name = "id")] public string Id { get; set; } = Guid.NewGuid().ToString("N");
        [DataMember(Name = "title")] public string Title { get; set; }
        [DataMember(Name = "connection")] public string Connection { get; set; }
        /// <summary>The Claude Code session that remembers the conversation.</summary>
        [DataMember(Name = "session")] public string Session { get; set; }
        [DataMember(Name = "updated")] public DateTime Updated { get; set; } = DateTime.Now;
        [DataMember(Name = "messages")] public List<ChatMessage> Messages { get; set; } = new List<ChatMessage>();
    }

    internal static class ChatStore
    {
        public static readonly string Folder = Path.Combine(Updater.Folder, "chats");
        private const int Keep = 100;

        public static void Save(ChatThread thread)
        {
            try
            {
                Directory.CreateDirectory(Folder);
                thread.Updated = DateTime.Now;
                using (var stream = File.Create(Path.Combine(Folder, thread.Id + ".json")))
                {
                    new DataContractJsonSerializer(typeof(ChatThread)).WriteObject(stream, thread);
                }
                foreach (var old in new DirectoryInfo(Folder).GetFiles("*.json").OrderByDescending(f => f.LastWriteTimeUtc).Skip(Keep))
                {
                    old.Delete();
                }
            }
            catch (Exception ex)
            {
                Log.Error("saving chat", ex);
            }
        }

        /// <summary>Saved conversations, newest first.</summary>
        public static List<ChatThread> All()
        {
            var threads = new List<ChatThread>();
            if (!Directory.Exists(Folder))
            {
                return threads;
            }
            foreach (var file in new DirectoryInfo(Folder).GetFiles("*.json").OrderByDescending(f => f.LastWriteTimeUtc))
            {
                try
                {
                    var thread = Json.Parse<ChatThread>(File.ReadAllText(file.FullName));
                    if (thread?.Messages != null && thread.Messages.Count > 0)
                    {
                        threads.Add(thread);
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("reading chat " + file.Name, ex);
                }
            }
            return threads;
        }

        public static void Delete(ChatThread thread)
        {
            try
            {
                File.Delete(Path.Combine(Folder, thread.Id + ".json"));
            }
            catch (Exception ex)
            {
                Log.Error("deleting chat", ex);
            }
        }
    }

    [DataContract]
    internal sealed class StreamLine
    {
        [DataMember(Name = "type")] public string Type { get; set; }
        [DataMember(Name = "event")] public StreamEvent Event { get; set; }
        [DataMember(Name = "result")] public string Result { get; set; }
        [DataMember(Name = "is_error")] public bool IsError { get; set; }
        [DataMember(Name = "session_id")] public string SessionId { get; set; }
    }

    [DataContract]
    internal sealed class StreamEvent
    {
        [DataMember(Name = "type")] public string Type { get; set; }
        [DataMember(Name = "delta")] public StreamDelta Delta { get; set; }
    }

    [DataContract]
    internal sealed class StreamDelta
    {
        [DataMember(Name = "type")] public string Type { get; set; }
        [DataMember(Name = "text")] public string Text { get; set; }
    }

    /// <summary>
    /// Conversations with the Claude Code CLI: each turn runs `claude -p` with every tool disabled
    /// and resumes the topic's session, so Claude remembers the conversation; the answer streams in.
    /// </summary>
    internal static class AiChat
    {
        private const int TimeoutSeconds = 300;

        /// <summary>The line Claude ends each answer with, offering follow-up questions.</summary>
        public const string NextMarker = ">>> next:";

        private const string SystemPrompt =
            "You are a SQL Server expert talking with a database developer inside SQL Server Management Studio. " +
            "Answer in Markdown: short paragraphs, bullet lists, and every piece of T-SQL in a ```sql fenced block that can be run as is. " +
            "Use only the tables, columns, indexes and plans in the context you are given; if something you need is missing, say what. " +
            "You cannot connect to the database and never claim to have run anything. When analyzing a plan, cite its operators, " +
            "estimated against actual rows, warnings and missing-index hints, and never propose an index that already exists. " +
            "Be concise and concrete; the developer can ask for more. " +
            "End every answer with one last line exactly in the form `" + NextMarker + " question one | question two | question three`: " +
            "up to three short follow-up questions the developer would likely ask next, written as the developer would ask them.";

        public static string Language()
        {
            var configured = AiAssistant.LoadSettings().Language;
            return !string.IsNullOrWhiteSpace(configured) ? configured : Loc.Vietnamese ? "Vietnamese" : "English";
        }

        /// <summary>
        /// Sends one turn and streams the answer through `delta` (called on a background thread).
        /// Returns the full answer; `session` is set to the conversation's session.
        /// </summary>
        public static string Send(ref string session, string input, Action<string> delta, CancellationToken cancel)
        {
            var claude = AiAssistant.FindClaude() ?? throw new InvalidOperationException(Loc.T("Claude Code CLI was not found. Install it from https://code.claude.com and sign in with `claude` to use the AI commands."));
            var settings = AiAssistant.LoadSettings();
            Directory.CreateDirectory(Updater.Folder);
            var systemFile = Path.Combine(Updater.Folder, "ai-chat-prompt.txt");
            File.WriteAllText(systemFile, SystemPrompt + " Write in " + Language() + ".", new UTF8Encoding(false));
            var args = new List<string>
            {
                "-p", "Answer the message in the input.",
                "--system-prompt-file", systemFile,
                "--tools", "",
                "--restricted", "--strict-mcp-config", "--disable-slash-commands",
                "--output-format", "stream-json", "--verbose", "--include-partial-messages",
            };
            if (!string.IsNullOrEmpty(session))
            {
                args.Add("--resume");
                args.Add(session);
            }
            if (!string.IsNullOrEmpty(settings.Model))
            {
                args.Add("--model");
                args.Add(settings.Model);
            }
            var line = string.Join(" ", args.Select(AiAssistant.Quote));
            var cmd = claude.EndsWith(".cmd", StringComparison.OrdinalIgnoreCase);
            // An empty working folder: Claude has no tools, and nothing here to read anyway.
            var work = Path.Combine(Path.GetTempPath(), "QuillSQL-ai");
            Directory.CreateDirectory(work);
            var start = new ProcessStartInfo(cmd ? "cmd.exe" : claude, cmd ? "/d /s /c \"" + AiAssistant.Quote(claude) + " " + line + "\"" : line)
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
            using (cancel.Register(() =>
            {
                try
                {
                    process.Kill();
                }
                catch (InvalidOperationException)
                {
                    // Already finished.
                }
            }))
            {
                var errors = "";
                var readErr = new Thread(() => errors = process.StandardError.ReadToEnd()) { IsBackground = true };
                readErr.Start();
                using (var writer = new StreamWriter(process.StandardInput.BaseStream, new UTF8Encoding(false)))
                {
                    writer.Write(input);
                }
                var streamed = new StringBuilder();
                StreamLine result = null;
                string next;
                while ((next = process.StandardOutput.ReadLine()) != null)
                {
                    if (!next.StartsWith("{", StringComparison.Ordinal))
                    {
                        continue;
                    }
                    StreamLine parsed;
                    try
                    {
                        parsed = Json.Parse<StreamLine>(next);
                    }
                    catch (SerializationException)
                    {
                        continue;
                    }
                    if (parsed?.Type == "stream_event" && parsed.Event?.Type == "content_block_delta" && parsed.Event.Delta?.Type == "text_delta" && parsed.Event.Delta.Text != null)
                    {
                        streamed.Append(parsed.Event.Delta.Text);
                        delta(parsed.Event.Delta.Text);
                    }
                    else if (parsed?.Type == "result")
                    {
                        result = parsed;
                    }
                    if (watch.Elapsed.TotalSeconds > TimeoutSeconds)
                    {
                        process.Kill();
                        throw new TimeoutException(Loc.T("Claude did not answer within " + TimeoutSeconds + " seconds."));
                    }
                }
                process.WaitForExit(5000);
                readErr.Join(2000);
                cancel.ThrowIfCancellationRequested();
                Log.Write("claude chat answered in " + watch.ElapsedMilliseconds + " ms, " + streamed.Length + " chars" + (session == null ? ", new session" : ""));
                if (result == null || result.IsError)
                {
                    var detail = result?.Result ?? errors.Trim();
                    throw new InvalidOperationException("Claude failed: " + (detail.Length > 300 ? detail.Substring(0, 300) : detail));
                }
                session = result.SessionId ?? session;
                return result.Result ?? streamed.ToString();
            }
        }

        /// <summary>Splits the follow-up questions off an answer.</summary>
        public static string WithoutNext(string answer, out List<string> next)
        {
            next = new List<string>();
            var text = (answer ?? "").TrimEnd();
            var at = text.LastIndexOf(NextMarker, StringComparison.OrdinalIgnoreCase);
            if (at < 0 || text.IndexOf('\n', at) >= 0)
            {
                // Mid-stream, hide a marker still being written.
                var partial = text.LastIndexOf('\n') + 1;
                if (NextMarker.StartsWith(text.Substring(partial).Trim(), StringComparison.OrdinalIgnoreCase) && text.Length > partial)
                {
                    return text.Substring(0, partial).TrimEnd();
                }
                return text;
            }
            next = text.Substring(at + NextMarker.Length).Split('|').Select(q => q.Trim().Trim('`', '"')).Where(q => q.Length > 0).Take(3).ToList();
            // Drop a backtick quoting the marker line, but not the fence that closes a code block above it.
            var head = text.Substring(0, at);
            var lineStart = head.LastIndexOf('\n') + 1;
            if (head.Substring(lineStart).Trim() == "`")
            {
                head = head.Substring(0, lineStart);
            }
            return head.TrimEnd();
        }
    }
}
