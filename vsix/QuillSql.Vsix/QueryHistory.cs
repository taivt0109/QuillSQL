using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.Serialization;
using System.Runtime.Serialization.Json;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using CommandEvents = EnvDTE.CommandEvents;
using Document = EnvDTE.Document;
using DocumentEvents = EnvDTE.DocumentEvents;
using DTE = EnvDTE.DTE;
using TextDocument = EnvDTE.TextDocument;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class HistoryEntry
    {
        [DataMember(Name = "time")] public string Time { get; set; }
        /// <summary>"executed" or "closed".</summary>
        [DataMember(Name = "kind")] public string Kind { get; set; }
        [DataMember(Name = "connection")] public string Connection { get; set; }
        [DataMember(Name = "title")] public string Title { get; set; }
        [DataMember(Name = "text")] public string Text { get; set; }

        public override string ToString()
        {
            var first = (Text ?? "").Split('\n').Select(l => l.Trim()).FirstOrDefault(l => l.Length > 0) ?? "";
            if (first.Length > 90)
            {
                first = first.Substring(0, 90) + "...";
            }
            return Time + "  " + (Kind == "closed" ? "closed  " : "executed") + "  " + (Connection ?? "") + "  " + first;
        }
    }

    /// <summary>
    /// Keeps every executed query and every closed query window in
    /// %LOCALAPPDATA%\QuillSQL\history.jsonl, one JSON object per line.
    /// </summary>
    internal static class QueryHistory
    {
        private const int MaxEntries = 5000;
        private static readonly string FilePath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "history.jsonl");
        private static readonly object Gate = new object();
        // DTE drops event handlers whose event objects are collected, so keep them.
        private static CommandEvents _execute;
        private static DocumentEvents _documents;
        private static string _lastText;

        public static void Start(DTE dte)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var command = dte.Commands.Item("Query.Execute");
                _execute = dte.Events.CommandEvents[command.Guid, command.ID];
                _execute.BeforeExecute += OnExecute;
            }
            catch (Exception ex)
            {
                Log.Error("watching Query.Execute", ex);
            }
            _documents = dte.Events.DocumentEvents;
            _documents.DocumentClosing += OnClosing;
            Trim();
        }

        private static void OnExecute(string guid, int id, object customIn, object customOut, ref bool cancel)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var editor = EditorContext.Active();
                if (editor == null)
                {
                    return;
                }
                var text = editor.HasSelection ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : editor.Text;
                var settings = Settings.Current;
                if (settings.Enabled && settings.ExecutionWarnings && !AllowRun(text, settings))
                {
                    cancel = true;
                    EditorContext.Status("the query was not run.");
                    return;
                }
                if (settings.KeepHistory)
                {
                    Add("executed", ConnectionResolver.TryGetActive()?.Description, EditorContext.Dte?.ActiveDocument?.Name, text);
                }
            }
            catch (Exception ex)
            {
                Log.Error("recording executed query", ex);
            }
        }

        /// <summary>Asks before running statements that change every row or drop data.</summary>
        private static bool AllowRun(string text, Settings settings)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var reply = EngineProcess.Shared.Request("execution_warnings",
                "\"text\":" + Json.Str(text) + ",\"truncate\":" + Json.Bool(settings.WarnTruncate) + ",\"drop\":" + Json.Bool(settings.WarnDrop));
            var warnings = reply == null ? null : Json.Parse<WarningsReply>(reply)?.Result?.Warnings;
            if (warnings == null || warnings.Length == 0)
            {
                return true;
            }
            var server = ConnectionResolver.TryGetActive()?.Description ?? "this server";
            Log.Write("execution warning: " + string.Join(" | ", warnings));
            return EditorContext.Confirm("QuillSQL: Run this query?",
                Loc.T("On " + server + ", this script will:") + "\n\n  • " + string.Join("\n  • ", warnings.Select(Loc.T)) + "\n\n" + Loc.T("Run it anyway?"), "Run anyway", safeDefault: true);
        }

        private static void OnClosing(Document document)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (!Settings.Current.KeepHistory || !document.Name.EndsWith(".sql", StringComparison.OrdinalIgnoreCase) || !(document.Object("TextDocument") is TextDocument text))
                {
                    return;
                }
                Add("closed", null, document.Name, text.StartPoint.CreateEditPoint().GetText(text.EndPoint));
            }
            catch (Exception ex)
            {
                Log.Error("recording closed query window", ex);
            }
        }

        private static void Add(string kind, string connection, string title, string text)
        {
            if (string.IsNullOrWhiteSpace(text) || text == _lastText)
            {
                return;
            }
            _lastText = text;
            var entry = new HistoryEntry { Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss"), Kind = kind, Connection = connection, Title = title, Text = text };
            var serializer = new DataContractJsonSerializer(typeof(HistoryEntry));
            using (var stream = new MemoryStream())
            {
                serializer.WriteObject(stream, entry);
                var line = Encoding.UTF8.GetString(stream.ToArray());
                lock (Gate)
                {
                    Directory.CreateDirectory(Path.GetDirectoryName(FilePath));
                    File.AppendAllText(FilePath, line + "\n", new UTF8Encoding(false));
                }
            }
        }

        /// <summary>Newest first.</summary>
        public static List<HistoryEntry> Load()
        {
            var entries = new List<HistoryEntry>();
            string[] lines;
            lock (Gate)
            {
                lines = File.Exists(FilePath) ? File.ReadAllLines(FilePath, Encoding.UTF8) : new string[0];
            }
            for (var i = lines.Length - 1; i >= 0; i--)
            {
                try
                {
                    var entry = Json.Parse<HistoryEntry>(lines[i]);
                    if (entry != null)
                    {
                        entries.Add(entry);
                    }
                }
                catch (Exception)
                {
                    // A line cut short by a crash; skip it.
                }
            }
            return entries;
        }

        private static void Trim()
        {
            try
            {
                lock (Gate)
                {
                    if (!File.Exists(FilePath))
                    {
                        return;
                    }
                    var lines = File.ReadAllLines(FilePath, Encoding.UTF8);
                    if (lines.Length > MaxEntries)
                    {
                        File.WriteAllLines(FilePath, lines.Skip(lines.Length - MaxEntries), new UTF8Encoding(false));
                    }
                }
            }
            catch (Exception ex)
            {
                Log.Error("trimming query history", ex);
            }
        }

        public static void ShowWindow() => Commands.Run("Query history", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var all = Load();
            var search = new TextBox { MinHeight = 28 };
            var list = Theme.List(new Thickness(10, 6, 10, 6));
            list.ItemTemplate = EntryTemplate();
            var preview = new TextBox
            {
                IsReadOnly = true,
                FontFamily = Theme.Mono,
                FontSize = 12,
                Padding = new Thickness(6),
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                AcceptsReturn = true,
            };
            var open = Theme.DialogButton(Loc.T("Open in New Query"), isDefault: true);
            var copy = Theme.DialogButton(Loc.T("Copy"));
            var close = Theme.DialogButton(Loc.T("Close"), isCancel: true);
            var count = new TextBlock { Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center };

            var splitter = new GridSplitter { Height = 8, HorizontalAlignment = HorizontalAlignment.Stretch, VerticalAlignment = VerticalAlignment.Center, Background = System.Windows.Media.Brushes.Transparent, ResizeBehavior = GridResizeBehavior.PreviousAndNext };
            var grid = new Grid { Margin = new Thickness(16, 16, 16, 12) };
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.2, GridUnitType.Star), MinHeight = 80 });
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star), MinHeight = 60 });
            var searchBox = Theme.SearchBox(search, Loc.T("Search query text and connections"));
            searchBox.Margin = new Thickness(0, 0, 0, 10);
            Grid.SetRow(searchBox, 0);
            Grid.SetRow(list, 1);
            Grid.SetRow(splitter, 2);
            Grid.SetRow(preview, 3);
            grid.Children.Add(searchBox);
            grid.Children.Add(list);
            grid.Children.Add(splitter);
            grid.Children.Add(preview);
            var root = new DockPanel();
            var footer = Theme.Footer(count, open, copy, close);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(grid);

            void refill()
            {
                var words = search.Text.Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries);
                var shown = all.Where(e => words.All(w => (e.Text ?? "").IndexOf(w, StringComparison.OrdinalIgnoreCase) >= 0 || (e.Connection ?? "").IndexOf(w, StringComparison.OrdinalIgnoreCase) >= 0)).Take(1000).ToList();
                list.ItemsSource = shown;
                list.SelectedIndex = 0;
                count.Text = shown.Count + " / " + all.Count;
            }
            refill();
            search.TextChanged += (s, e) => refill();
            list.SelectionChanged += (s, e) => preview.Text = (list.SelectedItem as HistoryEntry)?.Text ?? "";

            var window = Theme.Dialog(Loc.T("QuillSQL Query History (" + all.Count + ")"), true);
            window.Content = root;
            window.Width = 920;
            window.Height = 660;
            window.MinWidth = 560;
            window.MinHeight = 420;
            EditorContext.SetOwner(window);
            void openSelected()
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                if (list.SelectedItem is HistoryEntry entry)
                {
                    window.Close();
                    EditorContext.OpenInNewQuery(entry.Text);
                }
            }
            open.Click += (s, e) => openSelected();
            list.MouseDoubleClick += (s, e) => openSelected();
            copy.Click += (s, e) =>
            {
                if (list.SelectedItem is HistoryEntry entry)
                {
                    Clipboard.SetText(entry.Text);
                }
            };
            close.Click += (s, e) => window.Close();
            search.PreviewKeyDown += (s, e) =>
            {
                if (e.Key == System.Windows.Input.Key.Down && list.Items.Count > 0)
                {
                    (list.ItemContainerGenerator.ContainerFromIndex(Math.Max(0, list.SelectedIndex)) as ListBoxItem)?.Focus();
                    e.Handled = true;
                }
            };
            window.Loaded += (s, e) => search.Focus();
            window.Show();
        });

        /// <summary>A history row: kind tag, time and connection, then the first line of the query.</summary>
        private static DataTemplate EntryTemplate()
        {
            var executed = Theme.Frozen(Theme.Tint(Theme.Green, 0xC0));
            var closed = Theme.Frozen(Theme.Tint(Theme.Slate, 0xC0));
            var panel = new FrameworkElementFactory(typeof(StackPanel));

            var top = new FrameworkElementFactory(typeof(StackPanel));
            top.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);
            var tag = new FrameworkElementFactory(typeof(Border));
            tag.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            tag.SetValue(Border.PaddingProperty, new Thickness(5, 0, 5, 1));
            tag.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 8, 0));
            tag.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            tag.SetBinding(Border.BackgroundProperty, new System.Windows.Data.Binding(nameof(HistoryEntry.Kind)) { Converter = new FuncConverter(k => (string)k == "closed" ? closed : executed) });
            var kind = new FrameworkElementFactory(typeof(TextBlock));
            kind.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(HistoryEntry.Kind)) { Converter = new FuncConverter(k => Loc.T((string)k == "closed" ? "closed" : "executed")) });
            kind.SetValue(TextBlock.ForegroundProperty, System.Windows.Media.Brushes.White);
            kind.SetValue(TextBlock.FontSizeProperty, 10.0);
            kind.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            tag.AppendChild(kind);
            top.AppendChild(tag);
            foreach (var field in new[] { nameof(HistoryEntry.Time), nameof(HistoryEntry.Connection) })
            {
                var text = new FrameworkElementFactory(typeof(TextBlock));
                text.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(field));
                text.SetValue(UIElement.OpacityProperty, 0.7);
                text.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 12, 0));
                text.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
                top.AppendChild(text);
            }
            panel.AppendChild(top);

            var query = new FrameworkElementFactory(typeof(TextBlock));
            query.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(HistoryEntry.Text))
            {
                Converter = new FuncConverter(t => ((string)t ?? "").Split('\n').Select(l => l.Trim()).FirstOrDefault(l => l.Length > 0) ?? ""),
            });
            query.SetValue(TextBlock.FontFamilyProperty, Theme.Mono);
            query.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            query.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 4, 0, 0));
            panel.AppendChild(query);
            return new DataTemplate { VisualTree = panel };
        }
    }
}
