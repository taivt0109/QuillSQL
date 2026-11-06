using System;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>Menu commands that run engine refactorings and database checks on the active query window.</summary>
    internal static class Commands
    {
        private static readonly Guid OutputPaneId = new Guid("3f3b9c3e-6d5a-4f0e-9a7b-2c4d8e1f6a90");

        /// <summary>Runs a command body, reporting failures in the status bar instead of throwing into SSMS.</summary>
        public static void Run(string name, Action body)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                body();
            }
            catch (Exception ex)
            {
                Log.Error(name, ex);
                EditorContext.Status(name + " failed: " + ex.Message);
            }
        }

        private static EditorContext RequireEditor()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
            }
            return editor;
        }

        /// <summary>The connection key whose metadata the engine has, or "" when there is none yet.</summary>
        public static string MetadataKey(out string message)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            message = null;
            var target = ConnectionResolver.TryGetActive();
            if (target == null || !target.CanQuery)
            {
                message = "not connected, or the login type is not supported for metadata";
                return "";
            }
            if (!MetadataCache.EnsureLoaded(target))
            {
                message = "database metadata is still loading; try again in a moment";
            }
            return target.Key;
        }

        private static RefactorResult Refactor(EditorContext editor, string action, string extra = "")
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var key = MetadataKey(out var waiting);
            var reply = EngineProcess.Shared.Request("refactor",
                "\"action\":" + Json.Str(action) + ",\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text)
                + ",\"caret\":" + editor.Caret + ",\"start\":" + editor.SelectionStart + ",\"end\":" + editor.SelectionEnd + extra);
            var parsed = reply == null ? null : Json.Parse<RefactorReply>(reply);
            if (parsed?.Error != null || parsed?.Result == null)
            {
                EditorContext.Status((parsed?.Error ?? "the engine did not answer") + (waiting == null ? "" : " (" + waiting + ")"));
                return null;
            }
            return parsed.Result;
        }

        private static void ApplyRewrite(EditorContext editor, RefactorResult result, string done)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (result?.Text == null)
            {
                return;
            }
            EditorContext.Status(editor.ReplaceAll(result.Text) ? done + (result.Count > 0 ? " (" + result.Count + ")" : "") : "nothing to change.");
        }

        public static void ExpandWildcards() => Run("Expand wildcards", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor != null)
            {
                ApplyRewrite(editor, Refactor(editor, "expand_wildcards"), "wildcards expanded");
            }
        });

        public static void QualifyObjectNames() => Run("Qualify object names", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor != null)
            {
                ApplyRewrite(editor, Refactor(editor, "qualify"), "names qualified");
            }
        });

        public static void TableAliases(bool add) => Run(add ? "Add AS" : "Remove AS", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor != null)
            {
                ApplyRewrite(editor, Refactor(editor, add ? "add_as" : "remove_as"), add ? "AS added to aliases" : "AS removed from aliases");
            }
        });

        public static void EncapsulateAsProcedure() => Run("Encapsulate as stored procedure", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor == null)
            {
                return;
            }
            if (!editor.HasSelection)
            {
                EditorContext.Status("select the statements to put in the procedure.");
                return;
            }
            var name = EditorContext.Prompt("Encapsulate as Stored Procedure", "Procedure name:", "dbo.NewProcedure");
            if (string.IsNullOrWhiteSpace(name))
            {
                return;
            }
            var result = Refactor(editor, "encapsulate", ",\"name\":" + Json.Str(name.Trim()));
            if (result?.Script != null)
            {
                EditorContext.OpenInNewQuery(result.Script);
                EditorContext.Status("procedure script opened in a new window; review it, then run it.");
            }
        });

        private static string WordAt(string text, int caret)
        {
            bool word(char c) => char.IsLetterOrDigit(c) || c == '_' || c == '@' || c == '#' || c == '$';
            var start = caret;
            while (start > 0 && word(text[start - 1]))
            {
                start--;
            }
            var end = caret;
            while (end < text.Length && word(text[end]))
            {
                end++;
            }
            return text.Substring(start, end - start);
        }

        public static void Rename() => Run("Rename", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor == null)
            {
                return;
            }
            var current = WordAt(editor.Text, editor.Caret);
            if (current.Length == 0)
            {
                EditorContext.Status("put the caret on a name to rename.");
                return;
            }
            var name = EditorContext.Prompt("Smart Rename", "New name for " + current + ":", current);
            if (string.IsNullOrWhiteSpace(name) || name == current)
            {
                return;
            }
            var result = Refactor(editor, "rename", ",\"name\":" + Json.Str(name));
            if (result?.Script != null)
            {
                EditorContext.OpenInNewQuery(result.Script);
                EditorContext.Status("rename script opened in a new window; review it, then run it.");
            }
            else
            {
                ApplyRewrite(editor, result, "renamed");
            }
        });

        public static void InlineExec(AsyncPackage package) => Run("Inline EXEC", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor == null)
            {
                return;
            }
            var reply = EngineProcess.Shared.Request("exec_target", "\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret);
            var name = reply == null ? null : Json.Parse<RefactorReply>(reply)?.Result?.Name;
            var target = ConnectionResolver.TryGetActive();
            if (name == null || target == null || !target.CanQuery)
            {
                EditorContext.Status(name == null ? "put the caret in an EXEC statement." : "connect to the database with Windows or SQL Server authentication first.");
                return;
            }
            EditorContext.Status("reading the definition of " + name + "...");
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                string definition = null, error = null;
                await TaskScheduler.Default;
                try
                {
                    definition = Database.ProcedureDefinition(target, name);
                }
                catch (Exception ex)
                {
                    error = ex.Message;
                }
                await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (definition == null)
                {
                    EditorContext.Status(error ?? "no definition for " + name + " (it may be encrypted, or not a T-SQL procedure).");
                    return;
                }
                Run("Inline EXEC", () =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    var now = EditorContext.Active();
                    if (now == null || now.Text != editor.Text)
                    {
                        EditorContext.Status("the script changed while reading the definition; try again.");
                        return;
                    }
                    ApplyRewrite(now, Refactor(now, "inline_exec", ",\"definition\":" + Json.Str(definition)), name + " inlined");
                });
            });
        });

        /// <summary>The snippet environment: selection, clipboard, date, user and connection.</summary>
        public static string SnippetEnv(string selectedText)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var target = ConnectionResolver.TryGetActive();
            var description = target?.Description ?? "";
            var slash = description.IndexOf(" / ", StringComparison.Ordinal);
            string clipboard = "";
            try
            {
                clipboard = Clipboard.ContainsText() ? Clipboard.GetText() : "";
            }
            catch (Exception)
            {
                // The clipboard can be locked by another application.
            }
            var now = DateTime.Now;
            return "{\"selectedText\":" + Json.Str(selectedText ?? "")
                + ",\"date\":" + Json.Str(now.ToString("yyyy-MM-dd"))
                + ",\"time\":" + Json.Str(now.ToString("HH:mm:ss"))
                + ",\"user\":" + Json.Str(Environment.UserName)
                + ",\"machine\":" + Json.Str(Environment.MachineName)
                + ",\"server\":" + Json.Str(slash < 0 ? description : description.Substring(0, slash))
                + ",\"database\":" + Json.Str(slash < 0 ? "" : description.Substring(slash + 3))
                + ",\"clipboard\":" + Json.Str(clipboard) + "}";
        }

        /// <summary>Applies an engine tab/snippet edit to the editor and selects what it asks for.</summary>
        public static bool ApplyEdit(EditorContext editor, TextEdit edit)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (edit == null)
            {
                return false;
            }
            editor.Replace(edit.Start, edit.End, edit.Text);
            editor.Select(edit.SelectStart, edit.SelectEnd);
            return true;
        }

        public static void InsertSnippet() => Run("Insert snippet", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor == null)
            {
                return;
            }
            var reply = EngineProcess.Shared.Request("snippets", "");
            var snippets = reply == null ? null : Json.Parse<SnippetsReply>(reply)?.Result?.Snippets;
            if (snippets == null || snippets.Length == 0)
            {
                EditorContext.Status("no snippets.");
                return;
            }
            var chosen = PickSnippet(snippets, editor.HasSelection);
            if (chosen == null)
            {
                return;
            }
            var selected = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : "";
            var tab = EngineProcess.Shared.Request("tab",
                "\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.SelectionEnd + ",\"start\":" + editor.SelectionStart
                + ",\"snippet\":" + Json.Str(chosen) + ",\"env\":" + SnippetEnv(selected));
            var parsed = tab == null ? null : Json.Parse<TabReply>(tab);
            if (parsed?.Error != null)
            {
                EditorContext.Status(parsed.Error);
            }
            ApplyEdit(editor, parsed?.Result);
        });

        private static string PickSnippet(SnippetInfo[] snippets, bool surround)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var filter = new TextBox { Margin = new Thickness(0, 0, 0, 6) };
            var list = new ListBox { Height = 320, Width = 460, FontFamily = new System.Windows.Media.FontFamily("Consolas") };
            void refill()
            {
                var f = filter.Text.Trim();
                list.ItemsSource = snippets
                    .Where(s => f.Length == 0 || s.Prefix.IndexOf(f, StringComparison.OrdinalIgnoreCase) >= 0 || (s.Description ?? "").IndexOf(f, StringComparison.OrdinalIgnoreCase) >= 0)
                    .OrderBy(s => s.Prefix, StringComparer.OrdinalIgnoreCase)
                    .Select(s => s.Prefix.PadRight(10) + " " + s.Description)
                    .ToList();
                list.SelectedIndex = 0;
            }
            refill();
            var panel = new DockPanel { Margin = new Thickness(10) };
            DockPanel.SetDock(filter, Dock.Top);
            panel.Children.Add(filter);
            panel.Children.Add(list);
            var window = new Window
            {
                Title = surround ? "Surround With Snippet" : "Insert Snippet",
                Content = panel,
                SizeToContent = SizeToContent.WidthAndHeight,
                ResizeMode = ResizeMode.NoResize,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
            };
            EditorContext.SetOwner(window);
            string result = null;
            void accept()
            {
                if (list.SelectedItem is string row)
                {
                    result = row.Split(' ')[0];
                    window.DialogResult = true;
                }
            }
            filter.TextChanged += (s, e) => refill();
            filter.PreviewKeyDown += (s, e) =>
            {
                if (e.Key == Key.Down) { list.SelectedIndex = Math.Min(list.Items.Count - 1, list.SelectedIndex + 1); e.Handled = true; }
                else if (e.Key == Key.Up) { list.SelectedIndex = Math.Max(0, list.SelectedIndex - 1); e.Handled = true; }
                else if (e.Key == Key.Enter) { accept(); e.Handled = true; }
                else if (e.Key == Key.Escape) { window.DialogResult = false; e.Handled = true; }
                list.ScrollIntoView(list.SelectedItem);
            };
            list.MouseDoubleClick += (s, e) => accept();
            window.Loaded += (s, e) => filter.Focus();
            window.ShowDialog();
            return result;
        }

        public static void FixIssues() => Run("Fix code issues", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = RequireEditor();
            if (editor == null)
            {
                return;
            }
            var key = MetadataKey(out _);
            var reply = EngineProcess.Shared.Request("fix", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text));
            var parsed = reply == null ? null : Json.Parse<RefactorReply>(reply);
            if (parsed?.Result?.Text == null)
            {
                EditorContext.Status(parsed?.Error ?? "the engine did not answer.");
                return;
            }
            ApplyRewrite(editor, parsed.Result, "issues fixed");
        });

        public static void FindInvalidObjects(AsyncPackage package) => Run("Find invalid objects", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var target = ConnectionResolver.TryGetActive();
            if (target == null || !target.CanQuery)
            {
                EditorContext.Status("open a query window connected with Windows or SQL Server authentication first.");
                return;
            }
            EditorContext.Status("looking for invalid objects in " + target.Description + "...");
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                System.Collections.Generic.List<string> rows = null;
                string error = null;
                await TaskScheduler.Default;
                try
                {
                    rows = Database.InvalidObjects(target);
                }
                catch (Exception ex)
                {
                    error = ex.Message;
                }
                await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                var lines = error != null
                    ? new[] { "Could not check " + target.Description + ": " + error }
                    : new[] { rows.Count + " invalid object reference(s) in " + target.Description + ":" }.Concat(rows.Select(r => "    " + r)).ToArray();
                WriteOutput(lines);
                EditorContext.Status(error ?? rows.Count + " invalid object reference(s); see the Output window.");
            });
        });

        public static void WriteOutput(string[] lines)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!(Package.GetGlobalService(typeof(SVsOutputWindow)) is IVsOutputWindow output))
            {
                return;
            }
            var id = OutputPaneId;
            if (output.GetPane(ref id, out var pane) != VSConstants.S_OK || pane == null)
            {
                output.CreatePane(ref id, "QuillSQL", 1, 1);
                output.GetPane(ref id, out pane);
            }
            if (pane == null)
            {
                return;
            }
            pane.OutputStringThreadSafe(DateTime.Now.ToString("HH:mm:ss ") + string.Join(Environment.NewLine, lines) + Environment.NewLine);
            Log.Write("output: " + string.Join(" | ", lines));
            pane.Activate();
            // Show the Output window without taking the focus from the query window.
            var outputWindow = new Guid("34E76E81-EE4A-11D0-AE2E-00A0C90FFFC3");
            if (Package.GetGlobalService(typeof(SVsUIShell)) is IVsUIShell shell
                && shell.FindToolWindow((uint)__VSFINDTOOLWIN.FTW_fForceCreate, ref outputWindow, out var frame) == VSConstants.S_OK)
            {
                frame?.ShowNoActivate();
            }
        }
    }
}
