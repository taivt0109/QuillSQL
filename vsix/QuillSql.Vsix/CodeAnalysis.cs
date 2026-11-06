using System;
using System.IO;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    /// <summary>Code analysis results in the Error List, and the switch for editor underlines.</summary>
    internal static class CodeAnalysis
    {
        private static readonly string OffFlag = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "analysis-off");
        private static ErrorListProvider _errors;

        /// <summary>Raised when underlines are switched on or off.</summary>
        public static event Action Toggled;

        public static bool UnderlinesOn => !File.Exists(OffFlag);

        public static void Initialize(IServiceProvider package)
        {
            _errors = new ErrorListProvider(package) { ProviderName = "QuillSQL Code Analysis" };
        }

        public static CodeIssue[] Analyze(string text, string key)
        {
            var reply = EngineProcess.Shared.Request("analyze", "\"key\":" + Json.Str(key ?? "") + ",\"text\":" + Json.Str(text));
            return reply == null ? null : Json.Parse<AnalyzeReply>(reply)?.Result?.Issues;
        }

        public static void ToggleUnderlines() => Commands.Run("Toggle code analysis", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Directory.CreateDirectory(Path.GetDirectoryName(OffFlag));
            if (UnderlinesOn)
            {
                File.WriteAllText(OffFlag, DateTime.Now.ToString("s"));
            }
            else
            {
                File.Delete(OffFlag);
            }
            Toggled?.Invoke();
            EditorContext.Status("code analysis underlines " + (UnderlinesOn ? "on." : "off."));
        });

        /// <summary>Analyzes the active script and lists the issues in the Error List.</summary>
        public static void ShowInErrorList() => Commands.Run("Analyze code", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null || _errors == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var key = Commands.MetadataKey(out var waiting);
            var issues = Analyze(editor.Text, key);
            if (issues == null)
            {
                EditorContext.Status("the engine did not answer.");
                return;
            }
            _errors.SuspendRefresh();
            try
            {
                _errors.Tasks.Clear();
                var view = editor.View;
                foreach (var issue in issues)
                {
                    editor.Lines.GetLineIndexOfPosition(issue.Start, out var line, out var column);
                    var task = new ErrorTask
                    {
                        Text = issue.Code + ": " + issue.Message + (issue.Fix != null ? " [fix: " + issue.Fix.Title + "]" : ""),
                        Line = line,
                        Column = column,
                        ErrorCategory = issue.Severity == "warning" ? TaskErrorCategory.Warning : TaskErrorCategory.Message,
                        Category = TaskCategory.CodeSense,
                        Document = DocumentName(),
                    };
                    var start = issue.Start;
                    var end = issue.End;
                    task.Navigate += (s, e) =>
                    {
                        ThreadHelper.ThrowIfNotOnUIThread();
                        var current = EditorContext.For(view);
                        if (current != null)
                        {
                            view.SendExplicitFocus();
                            current.Select(Math.Min(start, current.Text.Length), Math.Min(end, current.Text.Length));
                        }
                    };
                    _errors.Tasks.Add(task);
                }
            }
            finally
            {
                _errors.ResumeRefresh();
            }
            _errors.Show();
            _errors.BringToFront();
            EditorContext.Status(issues.Length + " issue(s)" + (waiting == null ? "" : "; " + waiting) + ".");
        });

        private static string DocumentName()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                return EditorContext.Dte?.ActiveDocument?.FullName ?? "";
            }
            catch (Exception)
            {
                return "";
            }
        }
    }
}
