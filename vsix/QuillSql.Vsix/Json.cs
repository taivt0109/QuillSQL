using System.IO;
using System.Runtime.Serialization;
using System.Runtime.Serialization.Json;
using System.Text;

namespace QuillSql.Vsix
{
    internal static class Json
    {
        /// <summary>A JSON string literal. Non-ASCII is escaped so the pipe's encoding never matters.</summary>
        public static string Str(string value)
        {
            if (value == null)
            {
                return "null";
            }
            var sb = new StringBuilder(value.Length + 2);
            sb.Append('"');
            foreach (var c in value)
            {
                switch (c)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    default:
                        if (c < 0x20 || c > 0x7e)
                        {
                            sb.Append("\\u").Append(((int)c).ToString("x4"));
                        }
                        else
                        {
                            sb.Append(c);
                        }
                        break;
                }
            }
            return sb.Append('"').ToString();
        }

        public static T Parse<T>(string json) where T : class
        {
            var serializer = new DataContractJsonSerializer(typeof(T));
            using (var stream = new MemoryStream(Encoding.UTF8.GetBytes(json)))
            {
                return serializer.ReadObject(stream) as T;
            }
        }
    }

    [DataContract]
    internal sealed class CompletionReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public CompletionResult Result { get; set; }
    }

    [DataContract]
    internal sealed class CompletionResult
    {
        [DataMember(Name = "start")] public int Start { get; set; }
        [DataMember(Name = "items")] public CompletionItem[] Items { get; set; }
    }

    [DataContract]
    internal sealed class CompletionItem
    {
        [DataMember(Name = "label")] public string Label { get; set; }
        [DataMember(Name = "insert")] public string Insert { get; set; }
        [DataMember(Name = "kind")] public string Kind { get; set; }
        [DataMember(Name = "detail")] public string Detail { get; set; }
    }

    [DataContract]
    internal sealed class ErrorOnlyReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
    }
}

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class TabReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public TextEdit Result { get; set; }
    }

    /// <summary>Replace [Start, End) with Text, then select [SelectStart, SelectEnd). UTF-16 offsets.</summary>
    [DataContract]
    internal sealed class TextEdit
    {
        [DataMember(Name = "start")] public int Start { get; set; }
        [DataMember(Name = "end")] public int End { get; set; }
        [DataMember(Name = "text")] public string Text { get; set; }
        [DataMember(Name = "selectStart")] public int SelectStart { get; set; }
        [DataMember(Name = "selectEnd")] public int SelectEnd { get; set; }
    }

    [DataContract]
    internal sealed class RefactorReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public RefactorResult Result { get; set; }
    }

    [DataContract]
    internal sealed class RefactorResult
    {
        /// <summary>The rewritten script, or null when the result is a new script.</summary>
        [DataMember(Name = "text")] public string Text { get; set; }
        [DataMember(Name = "script")] public string Script { get; set; }
        [DataMember(Name = "count")] public int Count { get; set; }
        [DataMember(Name = "name")] public string Name { get; set; }
    }

    [DataContract]
    internal sealed class AnalyzeReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public AnalyzeResult Result { get; set; }
    }

    [DataContract]
    internal sealed class AnalyzeResult
    {
        [DataMember(Name = "issues")] public CodeIssue[] Issues { get; set; }
    }

    [DataContract]
    internal sealed class CodeIssue
    {
        [DataMember(Name = "code")] public string Code { get; set; }
        [DataMember(Name = "severity")] public string Severity { get; set; }
        [DataMember(Name = "message")] public string Message { get; set; }
        [DataMember(Name = "start")] public int Start { get; set; }
        [DataMember(Name = "end")] public int End { get; set; }
        [DataMember(Name = "fix")] public IssueFix Fix { get; set; }
    }

    [DataContract]
    internal sealed class IssueFix
    {
        [DataMember(Name = "title")] public string Title { get; set; }
        [DataMember(Name = "start")] public int Start { get; set; }
        [DataMember(Name = "end")] public int End { get; set; }
        [DataMember(Name = "text")] public string Text { get; set; }
    }

    [DataContract]
    internal sealed class TextReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public TextResult Result { get; set; }
    }

    [DataContract]
    internal sealed class TextResult
    {
        [DataMember(Name = "text")] public string Text { get; set; }
    }

    [DataContract]
    internal sealed class OccurrencesReply
    {
        [DataMember(Name = "result")] public OccurrencesResult Result { get; set; }
    }

    [DataContract]
    internal sealed class OccurrencesResult
    {
        [DataMember(Name = "ranges")] public TextRange[] Ranges { get; set; }
    }

    [DataContract]
    internal sealed class TextRange
    {
        [DataMember(Name = "start")] public int Start { get; set; }
        [DataMember(Name = "length")] public int Length { get; set; }
    }

    [DataContract]
    internal sealed class PlanTargetReply
    {
        [DataMember(Name = "error")] public string Error { get; set; }
        [DataMember(Name = "result")] public PlanTarget Result { get; set; }
    }

    [DataContract]
    internal sealed class PlanTarget
    {
        [DataMember(Name = "batches")] public string[] Batches { get; set; }
        [DataMember(Name = "objects")] public string[] Objects { get; set; }
        /// <summary>Why running the script could change something; null when it only reads.</summary>
        [DataMember(Name = "writeReason")] public string WriteReason { get; set; }
    }

    [DataContract]
    internal sealed class SnippetsReply
    {
        [DataMember(Name = "result")] public SnippetsResult Result { get; set; }
    }

    [DataContract]
    internal sealed class SnippetsResult
    {
        [DataMember(Name = "snippets")] public SnippetInfo[] Snippets { get; set; }
    }

    [DataContract]
    internal sealed class SnippetInfo
    {
        [DataMember(Name = "prefix")] public string Prefix { get; set; }
        [DataMember(Name = "description")] public string Description { get; set; }
    }
}
