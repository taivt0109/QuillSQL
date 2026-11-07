using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>Tools > QuillSQL > Options: every setting, with a live formatting preview.</summary>
    internal sealed class OptionsWindow
    {
        private static readonly string StylePath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".quillsql.json");
        private static readonly string SnippetFolder = Path.Combine(Updater.Folder, "Snippets");

        private readonly AsyncPackage _package;
        private readonly Settings _settings = Settings.Current.Clone();
        private readonly AiSettings _ai = AiAssistant.LoadSettings();
        private readonly ObservableCollection<ColorRule> _colors = new ObservableCollection<ColorRule>(ConnectionColors.All() ?? new ColorRule[0]);
        private readonly List<Action> _apply = new List<Action>();
        private Window _window;

        // Formatting
        private bool _sectionedStyle;
        private CheckBox _layout, _leadingCommas, _upperKeywords, _semicolons, _unicode, _replaceSectioned;
        private ComboBox _indent, _brackets, _aliases;
        private TextBox _sample, _preview;
        private readonly DispatcherTimer _previewTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(250) };

        // Snippets
        private ListBox _snippetList;
        private TextBox _snippetPrefix, _snippetDescription, _snippetBody;

        private OptionsWindow(AsyncPackage package) => _package = package;

        public static void Show(AsyncPackage package) => Commands.Run("Options", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            new OptionsWindow(package).Open();
        });

        private void Open()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var pages = new List<Tuple<string, UIElement>>
            {
                Page("General", "Language, updates, history and where QuillSQL keeps its files.", General()),
                Page("Suggestions", "When the suggestion list opens, how a suggestion is inserted and what the list shows.", Suggestions()),
                Page("Typing", "Characters closed for you, and the helpers shown in the editor.", Typing()),
                Page("Formatting", "How Format T-SQL lays out code. The preview follows your changes.", Formatting()),
                Page("Code analysis", "The rules checked as you type and by Analyze Code.", Analysis()),
                Page("Running queries", "Questions asked before a query that changes or removes data runs.", Running()),
                Page("Tab colors", "Color query windows by server or database, so production stands out.", Colors()),
                Page("AI", "The assistant behind the AI commands.", Ai()),
                Page("Snippets", "Pieces of code inserted by typing a short word.", Snippets()),
                Page("Shortcuts", "Keyboard shortcuts of the QuillSQL commands.", Shortcuts()),
            };

            // Category list on the left, the selected page on the right.
            var nav = Theme.List(new Thickness(14, 7, 14, 7));
            nav.BorderThickness = new Thickness(0);
            nav.Background = Brushes.Transparent;
            nav.Margin = new Thickness(0, 12, 0, 12);
            nav.ItemsSource = pages.Select(p => p.Item1).ToList();
            var host = new ContentControl { Focusable = false };
            nav.SelectionChanged += (s, e) =>
            {
                if (nav.SelectedIndex >= 0)
                {
                    host.Content = pages[nav.SelectedIndex].Item2;
                }
            };
            nav.SelectedIndex = 0;
            var navPane = new Border { Background = Theme.Faint, Child = nav };

            var ok = Theme.DialogButton(Loc.T("OK"), isDefault: true);
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isCancel: true);
            var apply = Theme.DialogButton(Loc.T("Apply"));
            var footer = Theme.Footer(Theme.Dim("QuillSQL " + QuillSqlPackage.Version, 0.6), ok, cancel, apply);

            var root = new Grid();
            root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(210) });
            root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            var divider = Theme.Divider(vertical: true);
            Grid.SetColumn(divider, 1);
            Grid.SetColumn(host, 2);
            Grid.SetRow(footer, 1);
            Grid.SetColumnSpan(footer, 3);
            root.Children.Add(navPane);
            root.Children.Add(divider);
            root.Children.Add(host);
            root.Children.Add(footer);

            _window = Theme.Dialog(Loc.T("QuillSQL Options"), true);
            _window.Content = root;
            _window.Width = 980;
            _window.Height = 700;
            _window.MinWidth = 760;
            _window.MinHeight = 520;
            EditorContext.SetOwner(_window);
            ok.Click += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                if (Save())
                {
                    _window.Close();
                }
            };
            apply.Click += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                Save();
            };
            _previewTimer.Tick += (s, e) =>
            {
                _previewTimer.Stop();
                UpdatePreview();
            };
            _window.Closed += (s, e) => _previewTimer.Stop();
            _window.Loaded += (s, e) => (nav.ItemContainerGenerator.ContainerFromIndex(0) as ListBoxItem)?.Focus();
            UpdatePreview();
            _window.ShowDialog();
        }

        // ---- Building blocks ----

        /// <summary>A page: title, one-line description, then the settings, scrolling as a whole.</summary>
        private static Tuple<string, UIElement> Page(string title, string description, UIElement content)
        {
            var heading = new TextBlock { Text = Loc.T(title), FontSize = 18, FontWeight = FontWeights.SemiBold };
            var about = Theme.Dim(Loc.T(description));
            about.Margin = new Thickness(0, 4, 0, 0);
            var top = new Border { BorderThickness = new Thickness(0, 0, 0, 1), Padding = new Thickness(0, 0, 0, 14), Margin = new Thickness(0, 0, 0, 8), Child = Stack(heading, about) };
            top.SetResourceReference(Border.BorderBrushProperty, ThemedDialogColors.SectionDividerBrushKey);
            var page = new DockPanel { Margin = new Thickness(28, 22, 28, 24) };
            DockPanel.SetDock(top, Dock.Top);
            page.Children.Add(top);
            page.Children.Add(content);
            return Tuple.Create(Loc.T(title), (UIElement)new ScrollViewer
            {
                Content = page,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
                Focusable = false,
            });
        }

        private static TextBlock Heading(string text) => new TextBlock { Text = Loc.T(text), FontWeight = FontWeights.SemiBold, FontSize = 13.5, Margin = new Thickness(0, 16, 0, 6) };

        private static TextBlock Note(string text)
        {
            var note = Theme.Dim(Loc.T(text));
            note.Margin = new Thickness(0, 2, 0, 8);
            note.MaxWidth = 640;
            note.HorizontalAlignment = HorizontalAlignment.Left;
            return note;
        }

        /// <summary>A check box bound to a setting; the value is written back when the user saves.</summary>
        private CheckBox Check(string label, bool value, Action<bool> set, string hint = null)
        {
            var box = new CheckBox { Content = new TextBlock { Text = Loc.T(label) }, IsChecked = value, Margin = new Thickness(0, 4, 0, 4), ToolTip = hint == null ? null : Loc.T(hint), HorizontalAlignment = HorizontalAlignment.Left };
            System.Windows.Automation.AutomationProperties.SetName(box, Loc.T(label));
            _apply.Add(() => set(box.IsChecked == true));
            return box;
        }

        private static StackPanel Stack(params UIElement[] children)
        {
            var panel = new StackPanel();
            foreach (var c in children)
            {
                panel.Children.Add(c);
            }
            return panel;
        }

        private static WrapPanel Row(params UIElement[] children)
        {
            var panel = new WrapPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 4, 0, 4) };
            foreach (var c in children)
            {
                panel.Children.Add(c);
            }
            return panel;
        }

        private static TextBlock Label(string text) => new TextBlock { Text = Loc.T(text), VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 12, 0), Width = 190, TextWrapping = TextWrapping.Wrap };

        private static ComboBox Combo(object items, int selected, double width = 240) => new ComboBox { ItemsSource = (System.Collections.IEnumerable)items, SelectedIndex = selected, Width = width, MinHeight = 24, HorizontalAlignment = HorizontalAlignment.Left };

        private static Button ButtonFor(string text, RoutedEventHandler click)
        {
            var button = Theme.DialogButton(Loc.T(text));
            button.Margin = new Thickness(0, 6, 8, 6);
            button.HorizontalAlignment = HorizontalAlignment.Left;
            button.Click += click;
            return button;
        }

        /// <summary>A group of settings placed side by side, wrapping when the window is narrow.</summary>
        private static WrapPanel Inline(params UIElement[] children)
        {
            var panel = Row(children);
            foreach (FrameworkElement c in panel.Children)
            {
                c.Margin = new Thickness(0, 4, 24, 4);
            }
            return panel;
        }

        // ---- Pages ----

        private UIElement General()
        {
            var s = _settings;
            var languages = new[] { "auto", "en", "vi" };
            var language = Combo(new[] { Loc.T("Automatic (Windows language)"), "English", "Tiếng Việt" }, Math.Max(0, Array.IndexOf(languages, s.Language ?? "auto")));
            _apply.Add(() => s.Language = languages[Math.Max(0, language.SelectedIndex)]);
            return Stack(
                Row(Label("Language"), language),
                Note("The new language applies as soon as you save, including the menus."),
                Check("Enable QuillSQL", s.Enabled, v => s.Enabled = v, "Off turns off suggestions, underlines, cards and warnings; menu commands still work."),
                Heading("Updates"),
                Check("Check for updates once a day", s.AutoUpdate, v => s.AutoUpdate = v),
                ButtonFor("Check for updates now", (o, e) =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    Updater.Start(_package, true);
                }),
                Heading("History"),
                Check("Keep a history of executed queries and closed tabs", s.KeepHistory, v => s.KeepHistory = v),
                Heading("About"),
                Note("QuillSQL " + QuillSqlPackage.Version + ". Settings are stored in " + Settings.FilePath + "."),
                ButtonFor("Open the settings and log folder", (o, e) =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    TextCommands.OpenLogFolder();
                }));
        }

        private UIElement Suggestions()
        {
            var s = _settings;
            var prefixes = new TextBox { Text = s.IgnoredPrefixes ?? "", Width = 240, MinHeight = 24, VerticalContentAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            var ownAliases = new TextBox { Text = s.OwnAliases ?? "", AcceptsReturn = true, FontFamily = Theme.Mono, Height = 80, Width = 360, HorizontalAlignment = HorizontalAlignment.Left, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Margin = new Thickness(0, 4, 0, 0) };
            _apply.Add(() =>
            {
                s.IgnoredPrefixes = prefixes.Text.Trim();
                s.OwnAliases = ownAliases.Text.Replace("\r", "").Trim();
            });
            return Stack(
                Check("Show suggestions automatically as you type", s.AutoShowSuggestions, v => s.AutoShowSuggestions = v, "When off, press Ctrl+Space to open the list."),
                Heading("Insert the selected suggestion with"),
                Inline(Check("Tab", s.InsertWithTab, v => s.InsertWithTab = v),
                    Check("Enter", s.InsertWithEnter, v => s.InsertWithEnter = v),
                    Check("Space", s.InsertWithSpace, v => s.InsertWithSpace = v),
                    Check(". (dot)", s.InsertWithDot, v => s.InsertWithDot = v),
                    Check(", (comma)", s.InsertWithComma, v => s.InsertWithComma = v),
                    Check("( (parenthesis)", s.InsertWithParenthesis, v => s.InsertWithParenthesis = v)),
                Heading("Inserted code"),
                Check("Give tables and views an alias after FROM and JOIN (Customers AS c)", s.AssignAliases, v => s.AssignAliases = v),
                Check("Write AS before the alias", s.AliasWithAs, v => s.AliasWithAs = v),
                Check("Insert object names with their schema (dbo.Customers)", s.QualifyObjectNames, v => s.QualifyObjectNames = v),
                Check("Fill in the whole definition after ALTER PROCEDURE, VIEW or FUNCTION", s.GenerateAlter, v => s.GenerateAlter = v),
                Heading("Aliases"),
                Row(Label("Prefixes left out of aliases"), prefixes),
                Note("Separated by commas. With tbl, tblCustomer gets the alias c instead of tc."),
                Label("Your own aliases, one \"Table = alias\" a line"),
                ownAliases,
                Heading("List"),
                Check("Show the definition of the selected table, view or procedure next to the list", s.ShowDefinitionBox, v => s.ShowDefinitionBox = v),
                Check("Show the parameters of a function or procedure while typing its arguments", s.ParameterHints, v => s.ParameterHints = v),
                Check("List sys and INFORMATION_SCHEMA views before their schema is typed", s.ShowSystemObjects, v => s.ShowSystemObjects = v),
                ButtonFor("Refresh suggestions now", (o, e) =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    MetadataCache.Clear();
                    EditorContext.Status("suggestions will reload on next use.");
                }));
        }

        private UIElement Typing()
        {
            var s = _settings;
            return Stack(
                Heading("Close automatically"),
                Inline(Check("' (single quote)", s.CloseSingleQuote, v => s.CloseSingleQuote = v),
                    Check("[ (square bracket)", s.CloseSquareBracket, v => s.CloseSquareBracket = v),
                    Check("( (parenthesis)", s.CloseParenthesis, v => s.CloseParenthesis = v),
                    Check("/* (block comment)", s.CloseComment, v => s.CloseComment = v)),
                Check("Typing a closing character that was inserted for you moves over it", s.OvertypeClosing, v => s.OvertypeClosing = v),
                Note("Nothing is closed inside a comment or an open string, or when SSMS has already closed it."),
                Heading("Editor"),
                Check("Show a card with columns, indexes, partitions and script when the mouse rests on an object", s.ObjectCards, v => s.ObjectCards = v),
                Check("Highlight the other uses of the name under the caret", s.HighlightMatches, v => s.HighlightMatches = v));
        }

        private string FormattingJson()
        {
            string choice(ComboBox box, params string[] values) => values[Math.Max(0, box.SelectedIndex)];
            var brackets = choice(_brackets, null, "WithBrackets", "WithoutBrackets");
            var aliases = choice(_aliases, null, "AddAs", "RemoveAs");
            return "{\"layout\":" + Json.Bool(_layout.IsChecked == true)
                + ",\"indentSize\":" + (_indent.SelectedIndex + 1)
                + ",\"leadingCommas\":" + Json.Bool(_leadingCommas.IsChecked == true)
                + ",\"applyCasing\":" + Json.Bool(_upperKeywords.IsChecked == true)
                + ",\"insertSemicolons\":" + Json.Bool(_semicolons.IsChecked == true)
                + ",\"prefixUnicode\":" + Json.Bool(_unicode.IsChecked == true)
                + (brackets == null ? "" : ",\"encloseIdentifiers\":" + Json.Str(brackets))
                + (aliases == null ? "" : ",\"tableAliasStyle\":" + Json.Str(aliases)) + "}";
        }

        private UIElement Formatting()
        {
            var s = _settings;
            // Current options file: flat options are edited here; a sectioned style is kept unless replaced.
            string current = File.Exists(StylePath) ? File.ReadAllText(StylePath) : "{}";
            FlatStyle flat;
            try
            {
                _sectionedStyle = current.Contains("\"whitespace\"") || current.Contains("\"lists\"") || current.Contains("\"metadata\"");
                flat = _sectionedStyle ? new FlatStyle() : Json.Parse<FlatStyle>(current) ?? new FlatStyle();
            }
            catch (Exception)
            {
                flat = new FlatStyle();
            }
            void changed(object o, RoutedEventArgs e) => _previewTimer.Restart();
            CheckBox box(string text, bool value)
            {
                var c = new CheckBox { Content = new TextBlock { Text = Loc.T(text) }, IsChecked = value, Margin = new Thickness(0, 4, 0, 4), HorizontalAlignment = HorizontalAlignment.Left };
                System.Windows.Automation.AutomationProperties.SetName(c, Loc.T(text));
                c.Checked += changed;
                c.Unchecked += changed;
                return c;
            }
            ComboBox combo(int selected, params string[] items)
            {
                var c = Combo(items.Select(Loc.T).ToArray(), selected, 160);
                c.SelectionChanged += (o, e) => _previewTimer.Restart();
                return c;
            }
            _layout = box("Lay out code (line breaks and indentation)", flat.Layout ?? true);
            _indent = combo(Math.Max(0, Math.Min(7, (flat.IndentSize ?? 4) - 1)), "1", "2", "3", "4", "5", "6", "7", "8");
            _indent.Width = 80;
            _leadingCommas = box("Commas at the start of list lines", flat.LeadingCommas == true);
            _upperKeywords = box("UPPERCASE keywords", flat.ApplyCasing == true);
            _semicolons = box("Insert missing semicolons", flat.InsertSemicolons == true);
            _unicode = box("Add N to non-ASCII string literals", flat.PrefixUnicode == true);
            _brackets = combo(flat.EncloseIdentifiers == "WithBrackets" ? 1 : flat.EncloseIdentifiers == "WithoutBrackets" ? 2 : 0, "Leave as written", "Add [brackets]", "Remove unneeded [brackets]");
            _aliases = combo(flat.TableAliasStyle == "AddAs" ? 1 : flat.TableAliasStyle == "RemoveAs" ? 2 : 0, "Leave as written", "Add AS", "Remove AS");
            _replaceSectioned = new CheckBox { Content = new TextBlock { Text = Loc.T("Replace the sectioned style in " + StylePath + " with these options"), TextWrapping = TextWrapping.Wrap }, Margin = new Thickness(0, 4, 0, 4) };

            UIElement sectioned = new TextBlock();
            if (_sectionedStyle)
            {
                var warning = new DockPanel();
                var icon = Theme.Icon(Theme.InfoGlyph, 14, Theme.Frozen(Theme.Blue));
                icon.VerticalAlignment = VerticalAlignment.Top;
                icon.Margin = new Thickness(0, 1, 8, 0);
                DockPanel.SetDock(icon, Dock.Left);
                warning.Children.Add(icon);
                warning.Children.Add(Stack(new TextBlock { Text = Loc.T("A sectioned style file is in use, so the options below are not applied. Tick the box to switch to them."), TextWrapping = TextWrapping.Wrap }, _replaceSectioned));
                var pill = Theme.Pill(warning, Theme.Blue);
                pill.Padding = new Thickness(10, 8, 10, 8);
                pill.Margin = new Thickness(0, 0, 0, 8);
                pill.HorizontalAlignment = HorizontalAlignment.Stretch;
                sectioned = pill;
            }

            var labelWidth = 136.0;
            TextBlock label(string text)
            {
                var l = Label(text);
                l.Width = labelWidth;
                return l;
            }
            var controls = Stack(
                sectioned,
                _layout,
                Row(label("Indent size"), _indent),
                _leadingCommas,
                _upperKeywords,
                _semicolons,
                _unicode,
                Row(label("Square brackets"), _brackets),
                Row(label("AS before table aliases"), _aliases),
                Heading("When formatting, also"),
                Check("Expand SELECT * into column names", s.FormatExpandsWildcards, v => s.FormatExpandsWildcards = v),
                Check("Qualify object and column names", s.FormatQualifiesNames, v => s.FormatQualifiesNames = v),
                Note("Format: Ctrl+K, Ctrl+Y. Lines between -- quill-format-off and -- quill-format-on are kept as written."));

            _sample = new TextBox
            {
                Text = "select c.CustomerId,c.Name,sum(o.Total) total from dbo.Customers c join sales.Orders o on o.CustomerId=c.CustomerId\nwhere o.Total>100 and c.Name like N'A%' group by c.CustomerId,c.Name order by total desc",
                AcceptsReturn = true,
                FontFamily = Theme.Mono,
                FontSize = 12,
                Padding = new Thickness(6, 4, 6, 4),
                Height = 110,
                TextWrapping = TextWrapping.Wrap,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
            _sample.TextChanged += (o, e) => _previewTimer.Restart();
            _preview = new TextBox
            {
                IsReadOnly = true,
                FontFamily = Theme.Mono,
                FontSize = 12,
                Padding = new Thickness(6, 4, 6, 4),
                Height = 260,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
            var sampleHeading = Heading("Sample (edit it)");
            sampleHeading.Margin = new Thickness(0, 0, 0, 6);
            var preview = Stack(sampleHeading, _sample, Heading("Preview"), _preview);

            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star), MinWidth = 300 });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1.25, GridUnitType.Star) });
            Grid.SetColumn(preview, 1);
            preview.Margin = new Thickness(24, 0, 0, 0);
            grid.Children.Add(controls);
            grid.Children.Add(preview);
            return grid;
        }

        private void UpdatePreview()
        {
            if (_preview == null)
            {
                return;
            }
            var reply = EngineProcess.Shared.Request("format", "\"text\":" + Json.Str(_sample.Text) + ",\"options\":" + FormattingJson());
            var parsed = reply == null ? null : Json.Parse<TextReply>(reply);
            _preview.Text = parsed?.Result?.Text ?? ("(" + (parsed?.Error ?? "the engine did not answer") + ")");
        }

        private UIElement Analysis()
        {
            var s = _settings;
            var reply = EngineProcess.Shared.Request("rules", "");
            var rules = reply == null ? null : Json.Parse<RulesReply>(reply)?.Result?.Rules;
            var disabled = new HashSet<string>(s.DisabledRules ?? new string[0]);
            var boxes = new List<Tuple<string, CheckBox>>();
            var rows = new List<Tuple<string, Border>>();
            var list = new StackPanel();
            foreach (var rule in rules ?? new RuleInfo[0])
            {
                var warning = rule.Severity == "warning";
                var text = new StackPanel { Orientation = Orientation.Horizontal };
                text.Children.Add(new TextBlock { Text = rule.Code, FontFamily = Theme.Mono, Opacity = 0.65, Width = 64, VerticalAlignment = VerticalAlignment.Center });
                text.Children.Add(new TextBlock { Text = Loc.T(rule.Title), VerticalAlignment = VerticalAlignment.Center, TextWrapping = TextWrapping.Wrap });
                var box = new CheckBox { Content = text, IsChecked = !disabled.Contains(rule.Code), VerticalAlignment = VerticalAlignment.Center };
                var severity = Theme.Pill(new TextBlock { Text = Loc.T(warning ? "Warning" : "Suggestion"), FontSize = 11 }, warning ? Theme.Amber : Theme.Blue);
                severity.Padding = new Thickness(6, 0, 6, 1);
                severity.VerticalAlignment = VerticalAlignment.Center;
                severity.Margin = new Thickness(12, 0, 0, 0);
                var line = new DockPanel();
                DockPanel.SetDock(severity, Dock.Right);
                line.Children.Add(severity);
                line.Children.Add(box);
                var row = new Border { Padding = new Thickness(10, 6, 10, 6), Child = line };
                boxes.Add(Tuple.Create(rule.Code, box));
                rows.Add(Tuple.Create((rule.Code + " " + rule.Title + " " + Loc.T(rule.Title)).ToLowerInvariant(), row));
                list.Children.Add(row);
            }
            _apply.Add(() => s.DisabledRules = boxes.Where(b => b.Item2.IsChecked != true).Select(b => b.Item1).ToArray());

            var search = new TextBox { MinHeight = 26 };
            var count = new TextBlock { Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(12, 0, 0, 0) };
            void refresh()
            {
                var words = (search.Text ?? "").ToLowerInvariant().Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries);
                var shown = 0;
                foreach (var row in rows)
                {
                    var visible = words.All(w => row.Item1.Contains(w));
                    row.Item2.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
                    row.Item2.Background = visible && shown++ % 2 == 1 ? Theme.Faint : Brushes.Transparent;
                }
                count.Text = Loc.T(boxes.Count(b => b.Item2.IsChecked == true) + " of " + boxes.Count + " rules on");
            }
            search.TextChanged += (o, e) => refresh();
            foreach (var b in boxes)
            {
                b.Item2.Checked += (o, e) => refresh();
                b.Item2.Unchecked += (o, e) => refresh();
            }
            void setVisible(bool on)
            {
                for (var i = 0; i < rows.Count; i++)
                {
                    if (rows[i].Item2.Visibility == Visibility.Visible)
                    {
                        boxes[i].Item2.IsChecked = on;
                    }
                }
            }
            var searchBox = Theme.SearchBox(search, Loc.T("Filter by code or name"));
            searchBox.Width = 300;
            var allOn = Theme.FlatButton(Loc.T("Turn all on"));
            allOn.Margin = new Thickness(12, 0, 0, 0);
            allOn.Click += (o, e) => setVisible(true);
            var allOff = Theme.FlatButton(Loc.T("Turn all off"));
            allOff.Margin = new Thickness(6, 0, 0, 0);
            allOff.Click += (o, e) => setVisible(false);
            var toolbar = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 8) };
            toolbar.Children.Add(searchBox);
            toolbar.Children.Add(allOn);
            toolbar.Children.Add(allOff);
            toolbar.Children.Add(count);
            refresh();

            var frame = new Border { BorderThickness = new Thickness(1), Child = list };
            frame.SetResourceReference(Border.BorderBrushProperty, ThemedDialogColors.ListBoxBorderBrushKey);
            frame.SetResourceReference(Border.BackgroundProperty, ThemedDialogColors.ListBoxBrushKey);
            frame.SetResourceReference(TextBlock.ForegroundProperty, ThemedDialogColors.ListBoxTextBrushKey);
            return Stack(
                Check("Underline code analysis issues as you type", s.CodeAnalysis, v => s.CodeAnalysis = v),
                Heading("Rules"),
                Note("Untick a rule to stop reporting it, in the editor and in Analyze Code."),
                toolbar,
                frame);
        }

        private UIElement Running()
        {
            var s = _settings;
            return Stack(
                Heading("Ask before running"),
                Check("Ask before running UPDATE or DELETE without WHERE", s.ExecutionWarnings, v => s.ExecutionWarnings = v),
                Check("Also ask before TRUNCATE TABLE", s.WarnTruncate, v => s.WarnTruncate = v),
                Check("Also ask before DROP TABLE and DROP DATABASE", s.WarnDrop, v => s.WarnDrop = v),
                Note("The question's default answer is Cancel, so pressing Enter by mistake does not run the query."),
                Heading("Run part of a script"),
                Note("Execute Current Statement (Ctrl+B, Ctrl+E) runs the statement at the caret; Execute Current Batch (Ctrl+B, Ctrl+B) runs the batch between GO lines."));
        }

        private UIElement Colors()
        {
            var grid = new DataGrid
            {
                ItemsSource = _colors,
                AutoGenerateColumns = false,
                CanUserAddRows = true,
                CanUserDeleteRows = true,
                CanUserResizeRows = false,
                Height = 280,
                HeadersVisibility = DataGridHeadersVisibility.Column,
                SelectionUnit = DataGridSelectionUnit.FullRow,
            };
            Theme.Grid(grid);
            // A swatch of the row's color, refreshed when a row is committed.
            var swatch = new FrameworkElementFactory(typeof(Border));
            swatch.SetValue(FrameworkElement.WidthProperty, 16.0);
            swatch.SetValue(FrameworkElement.HeightProperty, 16.0);
            swatch.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            swatch.SetValue(Border.BorderThicknessProperty, new Thickness(1));
            swatch.SetValue(Border.BorderBrushProperty, Theme.Line);
            swatch.SetBinding(Border.BackgroundProperty, new System.Windows.Data.Binding("Color") { Converter = new FuncConverter(c => Theme.BrushFromText(c as string)) });
            grid.Columns.Add(new DataGridTemplateColumn { Header = "", CellTemplate = new DataTemplate { VisualTree = swatch }, Width = 36, IsReadOnly = true });
            grid.Columns.Add(new DataGridTextColumn { Header = Loc.T("Server or database contains"), Binding = new System.Windows.Data.Binding("Match"), Width = 270 });
            grid.Columns.Add(new DataGridTextColumn { Header = Loc.T("Color (#RRGGBB)"), Binding = new System.Windows.Data.Binding("Color"), Width = 150, FontFamily = Theme.Mono });
            grid.Columns.Add(new DataGridTextColumn { Header = Loc.T("Label"), Binding = new System.Windows.Data.Binding("Label"), Width = 220 });
            var refresh = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
            refresh.Tick += (o, e) =>
            {
                refresh.Stop();
                try
                {
                    grid.Items.Refresh();
                }
                catch (InvalidOperationException)
                {
                    // Another row is being edited; its swatch updates when that one is committed.
                }
            };
            grid.RowEditEnding += (o, e) => refresh.Start();
            // The usual environments, matched by words that commonly appear in server and database names.
            var presets = Theme.FlatButton(Loc.T("Add production, staging, development and test"), null);
            presets.HorizontalAlignment = HorizontalAlignment.Left;
            presets.Margin = new Thickness(0, 0, 0, 8);
            presets.Click += (o, e) =>
            {
                grid.CommitEdit(DataGridEditingUnit.Row, true);
                foreach (var rule in new[]
                {
                    new ColorRule { Match = "prod", Color = "#C62828", Label = "PRODUCTION" },
                    new ColorRule { Match = "staging", Color = "#EF6C00", Label = "STAGING" },
                    new ColorRule { Match = "uat", Color = "#F9A825", Label = "UAT" },
                    new ColorRule { Match = "dev", Color = "#1565C0", Label = "DEVELOPMENT" },
                    new ColorRule { Match = "test", Color = "#2E7D32", Label = "TEST" },
                })
                {
                    if (!_colors.Any(c => string.Equals(c.Match, rule.Match, StringComparison.OrdinalIgnoreCase)))
                    {
                        _colors.Add(rule);
                    }
                }
            };
            return Stack(
                Note("Query windows whose \"server / database\" contains the text get a colored document tab. The most specific match wins: a database before its server, a server before a word. Tip: right-click a server or database in Object Explorer to color it."),
                presets,
                grid,
                Note("Examples: prod #C62828 PRODUCTION · uat #EF6C00 UAT · test #2E7D32 TEST. Delete a row with the Delete key."));
        }

        private UIElement Ai()
        {
            var model = new ComboBox { IsEditable = true, ItemsSource = new[] { "", "sonnet", "opus", "haiku" }, Text = _ai.Model ?? "", Width = 240, MinHeight = 24, HorizontalAlignment = HorizontalAlignment.Left };
            var language = new TextBox { Text = _ai.Language ?? "", Width = 240, MinHeight = 24, VerticalContentAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            var timeout = new TextBox { Text = _ai.ToolTimeoutSeconds.ToString(System.Globalization.CultureInfo.InvariantCulture), Width = 80, MinHeight = 24, VerticalContentAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            _apply.Add(() =>
            {
                _ai.Model = model.Text.Trim();
                _ai.Language = language.Text.Trim();
                if (int.TryParse(timeout.Text.Trim(), out var seconds))
                {
                    _ai.ToolTimeout = Math.Max(10, Math.Min(600, seconds));
                }
            });
            var claude = AiAssistant.FindClaude();
            var status = new StackPanel { Orientation = Orientation.Horizontal };
            status.Children.Add(Theme.Icon(claude != null ? "" : Theme.WarningGlyph, 14, Theme.Frozen(claude != null ? Theme.Green : Theme.Amber)));
            var statusText = new TextBlock { Text = Loc.T(claude != null ? "Claude Code CLI: " + claude : "Claude Code CLI was not found. Install it from https://code.claude.com and sign in with `claude` to use the AI commands."), TextWrapping = TextWrapping.Wrap, MaxWidth = 600, Margin = new Thickness(8, 0, 0, 0) };
            status.Children.Add(statusText);
            var statusPill = Theme.Pill(status, claude != null ? Theme.Green : Theme.Amber);
            statusPill.Padding = new Thickness(10, 6, 10, 6);
            statusPill.Margin = new Thickness(0, 0, 0, 12);
            return Stack(
                statusPill,
                Row(Label("Model (empty: your default)"), model),
                Row(Label("Answer language"), language),
                Note("For example Vietnamese or English. Explanations and SQL comments are written in it."),
                Row(Label("Time limit of a database lookup (seconds)"), timeout),
                Note("How long one lookup of the AI (a search, a plan, Query Store...) may run, from 10 to 600. Raise it for large databases; keep it low on busy production servers. Measuring a query may take twice as long."));
        }

        private UIElement Snippets()
        {
            _snippetList = Theme.List(new Thickness(10, 5, 10, 5));
            _snippetList.Height = 440;
            _snippetList.Width = 250;
            _snippetList.VerticalAlignment = VerticalAlignment.Top;
            _snippetList.ItemTemplate = SnippetTemplate();
            _snippetPrefix = new TextBox { Width = 200, MinHeight = 24, FontFamily = Theme.Mono, VerticalContentAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            _snippetDescription = new TextBox { MinWidth = 300, MinHeight = 24, VerticalContentAlignment = VerticalAlignment.Center };
            _snippetBody = new TextBox { AcceptsReturn = true, AcceptsTab = true, FontFamily = Theme.Mono, FontSize = 12, Padding = new Thickness(6, 4, 6, 4), Height = 270, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Auto, Margin = new Thickness(0, 8, 0, 0) };
            ReloadSnippets(null);
            _snippetList.SelectionChanged += (o, e) =>
            {
                if (_snippetList.SelectedItem is SnippetInfo snippet)
                {
                    _snippetPrefix.Text = snippet.Prefix;
                    _snippetDescription.Text = snippet.Description;
                    _snippetBody.Text = (snippet.Body ?? "").Replace("\r\n", "\n").Replace("\n", "\r\n");
                }
            };
            var labelWidth = 110.0;
            TextBlock label(string text)
            {
                var l = Label(text);
                l.Width = labelWidth;
                return l;
            }
            var description = new DockPanel { Margin = new Thickness(0, 4, 0, 4) };
            var descriptionLabel = label("Description");
            DockPanel.SetDock(descriptionLabel, Dock.Left);
            description.Children.Add(descriptionLabel);
            description.Children.Add(_snippetDescription);
            var save = ButtonFor("Save snippet", (o, e) => SaveSnippet());
            var editor = Stack(
                Row(label("Shortcut"), _snippetPrefix),
                description,
                _snippetBody,
                Note("$CURSOR$ marks where the caret goes; $SELECTEDTEXT$ is the selection; $name$ is a placeholder selected after insertion. Also $DATE$, $TIME$, $USER$, $SERVER$, $DBNAME$, $PASTE$."),
                Row(ButtonFor("New", (o, e) =>
                {
                    _snippetList.SelectedItem = null;
                    _snippetPrefix.Text = "";
                    _snippetDescription.Text = "";
                    _snippetBody.Text = "$CURSOR$";
                    _snippetPrefix.Focus();
                }), save, ButtonFor("Delete", (o, e) => DeleteSnippet())));
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            Grid.SetColumn(editor, 1);
            editor.Margin = new Thickness(20, 0, 0, 0);
            grid.Children.Add(_snippetList);
            grid.Children.Add(editor);
            return grid;
        }

        /// <summary>A snippet row: its shortcut in a code font, then its description dimmed.</summary>
        private static DataTemplate SnippetTemplate()
        {
            var panel = new FrameworkElementFactory(typeof(StackPanel));
            var prefix = new FrameworkElementFactory(typeof(TextBlock));
            prefix.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(SnippetInfo.Prefix)));
            prefix.SetValue(TextBlock.FontFamilyProperty, Theme.Mono);
            prefix.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            panel.AppendChild(prefix);
            var description = new FrameworkElementFactory(typeof(TextBlock));
            description.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(SnippetInfo.Description)));
            description.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            description.SetValue(TextBlock.FontSizeProperty, 11.0);
            description.SetValue(UIElement.OpacityProperty, 0.7);
            panel.AppendChild(description);
            return new DataTemplate { VisualTree = panel };
        }

        private void ReloadSnippets(string select)
        {
            var reply = EngineProcess.Shared.Request("snippets", "");
            var all = (reply == null ? null : Json.Parse<SnippetsReply>(reply)?.Result?.Snippets) ?? new SnippetInfo[0];
            var sorted = all.OrderBy(x => x.Prefix, StringComparer.OrdinalIgnoreCase).ToList();
            _snippetList.ItemsSource = sorted;
            if (select != null)
            {
                _snippetList.SelectedItem = sorted.FirstOrDefault(x => string.Equals(x.Prefix, select, StringComparison.OrdinalIgnoreCase));
            }
        }

        private static string SnippetFile(string prefix)
        {
            if (Directory.Exists(SnippetFolder))
            {
                foreach (var file in Directory.GetFiles(SnippetFolder, "*.json"))
                {
                    try
                    {
                        if (string.Equals(Json.Parse<SnippetInfo>(File.ReadAllText(file))?.Prefix, prefix, StringComparison.OrdinalIgnoreCase))
                        {
                            return file;
                        }
                    }
                    catch (Exception)
                    {
                        // Not a snippet file.
                    }
                }
            }
            return null;
        }

        private void SaveSnippet()
        {
            var prefix = new string((_snippetPrefix.Text ?? "").Trim().Where(c => char.IsLetterOrDigit(c) || c == '_').ToArray());
            if (prefix.Length == 0)
            {
                MessageBox.Show(_window, Loc.T("Enter a shortcut made of letters and digits."), "QuillSQL");
                return;
            }
            Directory.CreateDirectory(SnippetFolder);
            var file = SnippetFile(prefix) ?? Path.Combine(SnippetFolder, prefix + ".json");
            File.WriteAllText(file, "{\n  \"prefix\": " + Json.Str(prefix) + ",\n  \"description\": " + Json.Str(_snippetDescription.Text ?? "")
                + ",\n  \"body\": " + Json.Str((_snippetBody.Text ?? "").Replace("\r\n", "\n")) + ",\n  \"placeholders\": []\n}\n");
            EngineProcess.Shared.Request("reload_snippets", "");
            ReloadSnippets(prefix);
        }

        private void DeleteSnippet()
        {
            var prefix = (_snippetPrefix.Text ?? "").Trim();
            var file = SnippetFile(prefix);
            if (file == null)
            {
                MessageBox.Show(_window, Loc.T("Only your own snippets can be deleted. To change a built-in one, edit it and save: your version replaces it."), "QuillSQL");
                return;
            }
            File.Delete(file);
            EngineProcess.Shared.Request("reload_snippets", "");
            ReloadSnippets(null);
        }

        private static readonly Regex KeyCombination = new Regex(@"(?:(?:Ctrl|Shift|Alt)\+)+(?:Space|F\d{1,2}|[A-Z0-9])\b|\bF\d{1,2}\b");

        /// <summary>Shortcut text with each key combination drawn as a keycap and the rest as plain text.</summary>
        private static WrapPanel Keys(string text)
        {
            var panel = new WrapPanel { VerticalAlignment = VerticalAlignment.Center };
            void words(string part)
            {
                part = part.Trim();
                if (part.Length > 0)
                {
                    panel.Children.Add(new TextBlock { Text = part, Opacity = 0.7, Margin = new Thickness(part == "," ? 0 : 2, 0, 6, 0), VerticalAlignment = VerticalAlignment.Center });
                }
            }
            var last = 0;
            foreach (Match m in KeyCombination.Matches(text))
            {
                words(text.Substring(last, m.Index - last));
                panel.Children.Add(Theme.Keycap(m.Value));
                last = m.Index + m.Length;
            }
            words(text.Substring(last));
            return panel;
        }

        private static UIElement Shortcuts()
        {
            var rows = new[]
            {
                new[] { "Format T-SQL", "Ctrl+K, Ctrl+Y" },
                new[] { "Script object at the caret (CREATE TABLE / ALTER)", "F12  or  Ctrl+B, Ctrl+D" },
                new[] { "Expand wildcards", "Ctrl+B, Ctrl+W  (or Tab after *)" },
                new[] { "Qualify object names", "Ctrl+B, Ctrl+Q" },
                new[] { "Smart rename", "Ctrl+B, Ctrl+R" },
                new[] { "Insert semicolons", "Ctrl+B, Ctrl+C" },
                new[] { "Execute current statement", "Ctrl+B, Ctrl+E" },
                new[] { "Execute current batch", "Ctrl+B, Ctrl+B" },
                new[] { "Summarize script", "Ctrl+B, Ctrl+T" },
                new[] { "Insert snippet", "Ctrl+B, Ctrl+S" },
                new[] { "Analyze code", "Ctrl+B, Ctrl+A" },
                new[] { "Fix code issues", "Ctrl+B, Ctrl+F" },
                new[] { "Query history", "Ctrl+B, Ctrl+H" },
                new[] { "AI Chat", "Ctrl+B, Ctrl+I" },
                new[] { "Ask AI", "Ctrl+B, Ctrl+G" },
                new[] { "Analyze plan with AI", "Ctrl+B, Ctrl+P" },
                new[] { "Suggestions", "Ctrl+Space" },
                new[] { "Object card", "rest the mouse on a name, or Ctrl+B, Ctrl+K" },
                new[] { "Options", "Ctrl+B, Ctrl+O" },
            };
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star), MinWidth = 260 });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            for (var i = 0; i < rows.Length; i++)
            {
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                var band = new Border { Background = i % 2 == 1 ? Theme.Faint : Brushes.Transparent };
                var name = new TextBlock { Text = Loc.T(rows[i][0]), Margin = new Thickness(12, 6, 12, 6), VerticalAlignment = VerticalAlignment.Center, TextWrapping = TextWrapping.Wrap };
                var keys = Keys(Loc.T(rows[i][1]));
                keys.Margin = new Thickness(0, 4, 12, 4);
                Grid.SetRow(band, i);
                Grid.SetColumnSpan(band, 2);
                Grid.SetRow(name, i);
                Grid.SetRow(keys, i);
                Grid.SetColumn(keys, 1);
                grid.Children.Add(band);
                grid.Children.Add(name);
                grid.Children.Add(keys);
            }
            var frame = new Border { BorderThickness = new Thickness(1), Child = grid, Margin = new Thickness(0, 4, 0, 8) };
            frame.SetResourceReference(Border.BorderBrushProperty, ThemedDialogColors.ListBoxBorderBrushKey);
            return Stack(frame, Note("Change any of them in Tools > Options > Environment > Keyboard: search for QuillSQL."));
        }

        // ---- Saving ----

        private bool Save()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            foreach (var apply in _apply)
            {
                apply();
            }
            try
            {
                if (!_sectionedStyle || _replaceSectioned.IsChecked == true)
                {
                    var json = FormattingJson();
                    var reply = EngineProcess.Shared.Request("set_options", "\"options\":" + json);
                    var error = reply == null ? "the engine did not answer" : Json.Parse<ErrorOnlyReply>(reply)?.Error;
                    if (error != null)
                    {
                        MessageBox.Show(_window, Loc.T("The formatting options were not saved: " + error), "QuillSQL");
                        return false;
                    }
                    File.WriteAllText(StylePath, json);
                    _sectionedStyle = false;
                }
                ConnectionColors.Save(_colors);
                _ai.Save();
                _settings.Save();
                EditorContext.Status("options saved.");
                return true;
            }
            catch (Exception ex)
            {
                Log.Error("saving options", ex);
                MessageBox.Show(_window, Loc.T("The options could not be saved: " + ex.Message), "QuillSQL");
                return false;
            }
        }
    }

    /// <summary>The flat formatting options file, %USERPROFILE%\.quillsql.json.</summary>
    [System.Runtime.Serialization.DataContract]
    internal sealed class FlatStyle
    {
        [System.Runtime.Serialization.DataMember(Name = "layout")] public bool? Layout { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "indentSize")] public int? IndentSize { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "leadingCommas")] public bool? LeadingCommas { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "applyCasing")] public bool? ApplyCasing { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "insertSemicolons")] public bool? InsertSemicolons { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "prefixUnicode")] public bool? PrefixUnicode { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "encloseIdentifiers")] public string EncloseIdentifiers { get; set; }
        [System.Runtime.Serialization.DataMember(Name = "tableAliasStyle")] public string TableAliasStyle { get; set; }
    }
}
