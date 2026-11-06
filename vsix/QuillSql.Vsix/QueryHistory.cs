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
                Add("executed", ConnectionResolver.TryGetActive()?.Description, EditorContext.Dte?.ActiveDocument?.Name, text);
            }
            catch (Exception ex)
            {
                Log.Error("recording executed query", ex);
            }
        }

        private static void OnClosing(Document document)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (!document.Name.EndsWith(".sql", StringComparison.OrdinalIgnoreCase) || !(document.Object("TextDocument") is TextDocument text))
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
            var search = new TextBox { Margin = new Thickness(0, 0, 0, 6) };
            var list = new ListBox { FontFamily = new System.Windows.Media.FontFamily("Consolas"), FontSize = 12 };
            var preview = new TextBox
            {
                IsReadOnly = true,
                FontFamily = new System.Windows.Media.FontFamily("Consolas"),
                FontSize = 12,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                AcceptsReturn = true,
            };
            var open = new Button { Content = "Open in New Query", MinWidth = 130, Margin = new Thickness(0, 0, 8, 0) };
            var copy = new Button { Content = "Copy", MinWidth = 80, Margin = new Thickness(0, 0, 8, 0) };
            var close = new Button { Content = "Close", MinWidth = 80, IsCancel = true };
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 8, 0, 0) };
            buttons.Children.Add(open);
            buttons.Children.Add(copy);
            buttons.Children.Add(close);

            var grid = new Grid { Margin = new Thickness(10) };
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            Grid.SetRow(search, 0);
            Grid.SetRow(list, 1);
            Grid.SetRow(preview, 2);
            Grid.SetRow(buttons, 3);
            preview.Margin = new Thickness(0, 6, 0, 0);
            grid.Children.Add(search);
            grid.Children.Add(list);
            grid.Children.Add(preview);
            grid.Children.Add(buttons);

            void refill()
            {
                var words = search.Text.Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries);
                list.ItemsSource = all.Where(e => words.All(w => (e.Text ?? "").IndexOf(w, StringComparison.OrdinalIgnoreCase) >= 0 || (e.Connection ?? "").IndexOf(w, StringComparison.OrdinalIgnoreCase) >= 0)).Take(1000).ToList();
                list.SelectedIndex = 0;
            }
            refill();
            search.TextChanged += (s, e) => refill();
            list.SelectionChanged += (s, e) => preview.Text = (list.SelectedItem as HistoryEntry)?.Text ?? "";

            var window = new Window
            {
                Title = "QuillSQL Query History (" + all.Count + ")",
                Content = grid,
                Width = 900,
                Height = 640,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
            };
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
            window.Loaded += (s, e) => search.Focus();
            window.Show();
        });
    }
}
