using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using Microsoft.VisualStudio.Shell;
using Document = EnvDTE.Document;
using TextDocument = EnvDTE.TextDocument;
using vsEPReplaceTextOptions = EnvDTE.vsEPReplaceTextOptions;
using vsSaveChanges = EnvDTE.vsSaveChanges;

namespace QuillSql.Vsix
{
    /// <summary>Editing commands: text rewrites, running part of a script, tabs and snippets.</summary>
    internal static class TextCommands
    {
        private static EditorContext Editor()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
            }
            return editor;
        }

        /// <summary>Runs an engine tool on the selection, or on the whole script when nothing is selected.</summary>
        public static void Rewrite(string action, string done) => Commands.Run(done, () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null)
            {
                return;
            }
            var start = editor.HasSelection ? editor.SelectionStart : 0;
            var end = editor.HasSelection ? editor.SelectionEnd : editor.Text.Length;
            var text = editor.Text.Substring(start, end - start);
            var reply = EngineProcess.Shared.Request("tool", "\"action\":" + Json.Str(action) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + (editor.Caret - start));
            var parsed = reply == null ? null : Json.Parse<TextReply>(reply);
            if (parsed?.Result?.Text == null)
            {
                EditorContext.Status(parsed?.Error ?? "the engine did not answer.");
                return;
            }
            if (parsed.Result.Text == text)
            {
                EditorContext.Status("nothing to change.");
                return;
            }
            editor.Replace(start, end, parsed.Result.Text);
            if (editor.HasSelection)
            {
                editor.Select(start, start + parsed.Result.Text.Length);
            }
            EditorContext.Status(done + ".");
        });

        /// <summary>Selects the statement or batch at the caret and runs it.</summary>
        public static void ExecuteAtCaret(bool batch) => Commands.Run(batch ? "Execute current batch" : "Execute current statement", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null)
            {
                return;
            }
            var reply = EngineProcess.Shared.Request("tool", "\"action\":" + Json.Str(batch ? "batch_range" : "statement_range") + ",\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret);
            var range = reply == null ? null : Json.Parse<RangeReply>(reply)?.Result;
            if (range == null)
            {
                EditorContext.Status("no statement at the caret.");
                return;
            }
            editor.Select(range.Start, range.End);
            EditorContext.Dte?.ExecuteCommand("Query.Execute");
        });

        /// <summary>Formats every open query window, each as one Undo step.</summary>
        public static void FormatAllTabs() => Commands.Run("Format all tabs", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var dte = EditorContext.Dte;
            int formatted = 0, failed = 0;
            foreach (Document document in dte.Documents)
            {
                if (!document.Name.EndsWith(".sql", StringComparison.OrdinalIgnoreCase) || !(document.Object("TextDocument") is TextDocument text))
                {
                    continue;
                }
                var start = text.StartPoint.CreateEditPoint();
                var original = start.GetText(text.EndPoint);
                var reply = EngineProcess.Shared.Request("format", "\"text\":" + Json.Str(original));
                var result = reply == null ? null : Json.Parse<TextReply>(reply)?.Result?.Text;
                if (result == null)
                {
                    failed++;
                    continue;
                }
                if (result != original)
                {
                    start.ReplaceText(text.EndPoint, result, (int)vsEPReplaceTextOptions.vsEPReplaceTextKeepMarkers);
                    formatted++;
                }
            }
            EditorContext.Status(formatted + " tab(s) formatted" + (failed > 0 ? ", " + failed + " could not be formatted (see each tab)." : "."));
        });

        public static void DuplicateTab() => Commands.Run("Duplicate tab", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor != null)
            {
                EditorContext.OpenInNewQuery(editor.Text);
            }
        });

        public static void CloseUnmodifiedTabs() => Commands.Run("Close unmodified tabs", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var dte = EditorContext.Dte;
            var unmodified = new System.Collections.Generic.List<Document>();
            foreach (Document d in dte.Documents)
            {
                if (d.Saved && d.Name.EndsWith(".sql", StringComparison.OrdinalIgnoreCase))
                {
                    unmodified.Add(d);
                }
            }
            foreach (var document in unmodified)
            {
                document.Close(vsSaveChanges.vsSaveChangesNo);
            }
            EditorContext.Status(unmodified.Count + " unmodified tab(s) closed.");
        });

        /// <summary>The statements of the script in a window; double-click one to go to it.</summary>
        public static void SummarizeScript() => Commands.Run("Summarize script", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null)
            {
                return;
            }
            var reply = EngineProcess.Shared.Request("outline", "\"text\":" + Json.Str(editor.Text));
            var entries = reply == null ? null : Json.Parse<OutlineReply>(reply)?.Result?.Entries;
            if (entries == null || entries.Length == 0)
            {
                EditorContext.Status("the script has no statements.");
                return;
            }
            var list = Theme.List(new Thickness(8, 4, 8, 4));
            list.ItemsSource = entries;
            list.ItemTemplate = OutlineTemplate();
            list.BorderThickness = new Thickness(0);
            var hint = Theme.Dim(Loc.T("Double-click a statement, or select it and press Enter, to go to it in the editor."));
            hint.Margin = new Thickness(16, 10, 16, 10);
            var root = new DockPanel();
            DockPanel.SetDock(hint, Dock.Bottom);
            var divider = Theme.Divider();
            DockPanel.SetDock(divider, Dock.Bottom);
            root.Children.Add(hint);
            root.Children.Add(divider);
            root.Children.Add(list);
            var window = Theme.Dialog(Loc.T("QuillSQL Script Summary (" + entries.Length + " statements)"), true);
            window.Content = root;
            window.Width = 820;
            window.Height = 520;
            window.MinWidth = 420;
            window.MinHeight = 240;
            EditorContext.SetOwner(window);
            var view = editor.View;
            void go()
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                if (list.SelectedItem is OutlineEntry entry && EditorContext.For(view) is EditorContext current)
                {
                    view.SendExplicitFocus();
                    current.Select(entry.Start, entry.Start);
                }
            }
            list.MouseDoubleClick += (s, e) => go();
            list.KeyDown += (s, e) =>
            {
                if (e.Key == Key.Enter)
                {
                    go();
                }
            };
            window.Loaded += (s, e) => (list.ItemContainerGenerator.ContainerFromIndex(0) as ListBoxItem)?.Focus();
            window.Show();
        });

        /// <summary>An outline row: line number, statement kind tag, then the statement.</summary>
        private static DataTemplate OutlineTemplate()
        {
            var panel = new FrameworkElementFactory(typeof(DockPanel));
            var line = new FrameworkElementFactory(typeof(TextBlock));
            line.SetValue(DockPanel.DockProperty, Dock.Left);
            line.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(OutlineEntry.Line)));
            line.SetValue(FrameworkElement.WidthProperty, 44.0);
            line.SetValue(TextBlock.TextAlignmentProperty, TextAlignment.Right);
            line.SetValue(TextBlock.FontFamilyProperty, Theme.Mono);
            line.SetValue(UIElement.OpacityProperty, 0.55);
            line.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 12, 0));
            line.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            panel.AppendChild(line);

            var tag = new FrameworkElementFactory(typeof(Border));
            tag.SetValue(DockPanel.DockProperty, Dock.Left);
            tag.SetValue(FrameworkElement.WidthProperty, 132.0);
            tag.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 12, 0));
            tag.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            var pill = new FrameworkElementFactory(typeof(Border));
            pill.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Left);
            pill.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            pill.SetValue(Border.PaddingProperty, new Thickness(6, 0, 6, 1));
            pill.SetValue(Border.BackgroundProperty, Theme.Subtle);
            var kind = new FrameworkElementFactory(typeof(TextBlock));
            kind.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(OutlineEntry.Kind)));
            kind.SetValue(TextBlock.FontSizeProperty, 11.0);
            kind.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            kind.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            pill.AppendChild(kind);
            tag.AppendChild(pill);
            panel.AppendChild(tag);

            var summary = new FrameworkElementFactory(typeof(TextBlock));
            summary.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(OutlineEntry.Summary)));
            summary.SetValue(TextBlock.FontFamilyProperty, Theme.Mono);
            summary.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            summary.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            panel.AppendChild(summary);
            return new DataTemplate { VisualTree = panel };
        }

        /// <summary>Saves the selection as a snippet in %LOCALAPPDATA%\QuillSQL\Snippets.</summary>
        public static void CreateSnippet() => Commands.Run("Create snippet", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null || !editor.HasSelection)
            {
                EditorContext.Status("select the code for the snippet first.");
                return;
            }
            var prefix = EditorContext.Prompt("Create Snippet", "Shortcut (letters and digits, for example sel):", "");
            if (string.IsNullOrWhiteSpace(prefix))
            {
                return;
            }
            prefix = new string(prefix.Trim().Where(c => char.IsLetterOrDigit(c) || c == '_').ToArray());
            var description = EditorContext.Prompt("Create Snippet", "Description:", "") ?? "";
            var body = editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart).Replace("\r\n", "\n");
            var folder = Path.Combine(Updater.Folder, "Snippets");
            Directory.CreateDirectory(folder);
            File.WriteAllText(Path.Combine(folder, prefix + ".json"),
                "{\n  \"prefix\": " + Json.Str(prefix) + ",\n  \"description\": " + Json.Str(description) + ",\n  \"body\": " + Json.Str(body) + ",\n  \"placeholders\": []\n}\n");
            EngineProcess.Shared.Request("reload_snippets", "");
            EditorContext.Status("snippet " + prefix + " saved; type it and press Tab.");
        });

        /// <summary>Joins the selected lines into a comma list, optionally quoting each one.</summary>
        public static void AddCommas(bool quotes) => Commands.Run(quotes ? "Add quotes and commas" : "Add commas", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null || !editor.HasSelection)
            {
                EditorContext.Status("select the lines first.");
                return;
            }
            var text = editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart);
            var nl = text.Contains("\r\n") ? "\r\n" : "\n";
            var lines = text.Split(new[] { "\r\n", "\n" }, StringSplitOptions.None);
            var values = lines.Select(l => l.Trim()).Where(l => l.Length > 0).Select(l => quotes ? "'" + l.Replace("'", "''") + "'" : l.TrimEnd(',')).ToList();
            var result = string.Join("," + nl, values);
            editor.Replace(editor.SelectionStart, editor.SelectionEnd, result);
            editor.Select(editor.SelectionStart, editor.SelectionStart + result.Length);
        });

        /// <summary>Wraps the selection in format-off and format-on markers.</summary>
        public static void DisableFormatting() => Commands.Run("Disable formatting", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = Editor();
            if (editor == null || !editor.HasSelection)
            {
                EditorContext.Status("select the code to keep as it is.");
                return;
            }
            var lineStart = editor.SelectionStart == 0 ? 0 : editor.Text.LastIndexOf('\n', editor.SelectionStart - 1) + 1;
            var selected = editor.Text.Substring(lineStart, editor.SelectionEnd - lineStart);
            var nl = editor.Text.Contains("\r\n") ? "\r\n" : "\n";
            var wrapped = "-- quill-format-off" + nl + selected.TrimEnd('\r', '\n') + nl + "-- quill-format-on" + nl;
            editor.Replace(lineStart, editor.SelectionEnd, wrapped);
            EditorContext.Status("formatting turned off for the selection.");
        });

        public static void ToggleEnabled() => Commands.Run("Enable or disable QuillSQL", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var settings = Settings.Current.Clone();
            settings.Enabled = !settings.Enabled;
            settings.Save();
            EditorContext.Status(settings.Enabled ? "enabled." : "disabled; suggestions, underlines and cards are off until you enable it again.");
        });

        public static void OpenLogFolder() => Commands.Run("Open log folder", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Directory.CreateDirectory(Updater.Folder);
            Process.Start(new ProcessStartInfo(Updater.Folder) { UseShellExecute = true });
        });
    }
}
