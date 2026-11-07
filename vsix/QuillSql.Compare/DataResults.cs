using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Input;
using System.Windows.Media;

namespace QuillSql.Compare
{
    /// <summary>
    /// Data comparison results: one row per table with what is deployed of each kind of row, and
    /// below it the rows of the selected table, each with its own deploy check box.
    /// </summary>
    internal sealed class DataResults : DockPanel
    {
        // Row kinds as the engine numbers them.
        public const int Identical = 0, Different = 1, OnlyInSource = 2, OnlyInTarget = 3, AllRows = 4;

        /// <summary>Which rows of one kind are deployed: all (Included) or none, with Toggled the other way round.</summary>
        public sealed class KindSelection
        {
            public bool Included = true;
            public readonly HashSet<long> Toggled = new HashSet<long>();
        }

        public sealed class TableRow : INotifyPropertyChanged
        {
            public int Index { get; set; }
            public string Name { get; set; }
            public long[] Counts { get; set; }
            public string Status { get; set; }
            public bool Comparable { get; set; }
            public string Group { get; set; }
            public int Rank { get; set; }
            public List<string> Columns { get; set; }
            public List<string> TargetColumns { get; set; }
            public HashSet<int> Key { get; set; }
            public List<long> ColumnDifferences { get; set; }
            public readonly KindSelection[] Selection = { new KindSelection(), new KindSelection(), new KindSelection(), new KindSelection() };

            public long Different => Counts[DataResults.Different];
            public long OnlyInSource => Counts[DataResults.OnlyInSource];
            public long OnlyInTarget => Counts[DataResults.OnlyInTarget];
            public long Identical => Counts[DataResults.Identical];
            public long AllDifferent => Different + OnlyInSource + OnlyInTarget;
            public bool HasDifferent => Comparable && Different > 0;
            public bool HasOnlyInSource => Comparable && OnlyInSource > 0;
            public bool HasOnlyInTarget => Comparable && OnlyInTarget > 0;
            public bool HasAny => Comparable && AllDifferent > 0;

            public long SelectedCount(int kind)
            {
                var s = Selection[kind];
                return s.Included ? Counts[kind] - s.Toggled.Count : s.Toggled.Count;
            }

            public long SelectedTotal => SelectedCount(DataResults.Different) + SelectedCount(DataResults.OnlyInSource) + SelectedCount(DataResults.OnlyInTarget);

            /// <summary>Ticked, cleared, or mixed (null) when only some rows are deployed.</summary>
            public bool? State(int kind)
            {
                var n = SelectedCount(kind);
                return n == Counts[kind] ? true : n == 0 ? false : (bool?)null;
            }

            public void SetKind(int kind, bool on)
            {
                Selection[kind].Included = on;
                Selection[kind].Toggled.Clear();
                Changed();
            }

            public bool? DifferentState { get => State(DataResults.Different); set => SetKind(DataResults.Different, value != false); }
            public bool? OnlyInSourceState { get => State(DataResults.OnlyInSource); set => SetKind(DataResults.OnlyInSource, value != false); }
            public bool? OnlyInTargetState { get => State(DataResults.OnlyInTarget); set => SetKind(DataResults.OnlyInTarget, value != false); }

            /// <summary>Every kind together, as the All different column shows it.</summary>
            public bool? AllState
            {
                get
                {
                    var states = new[] { DataResults.Different, DataResults.OnlyInSource, DataResults.OnlyInTarget }.Where(k => Counts[k] > 0).Select(State).Distinct().ToList();
                    return states.Count == 0 ? false : states.Count == 1 ? states[0] : null;
                }
                set
                {
                    foreach (var k in new[] { DataResults.Different, DataResults.OnlyInSource, DataResults.OnlyInTarget }) SetKind(k, value != false);
                }
            }

            public bool IsRowSelected(int kind, long row) => Selection[kind].Included != Selection[kind].Toggled.Contains(row);

            public void SetRow(int kind, long row, bool on)
            {
                var s = Selection[kind];
                if (on == s.Included) s.Toggled.Remove(row);
                else s.Toggled.Add(row);
                Changed();
            }

            public void Changed()
            {
                foreach (var p in new[] { nameof(DifferentState), nameof(OnlyInSourceState), nameof(OnlyInTargetState), nameof(AllState) })
                {
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(p));
                }
                SelectionChanged?.Invoke();
            }

            public event Action SelectionChanged;
            public event PropertyChangedEventHandler PropertyChanged;
        }

        /// <summary>One stored row in the rows grid.</summary>
        public sealed class RowItem : INotifyPropertyChanged
        {
            private readonly TableRow _table;
            private readonly Dictionary<string, string> _values = new Dictionary<string, string>();
            public int Kind { get; }
            public long Row { get; }
            public HashSet<int> Differs { get; } = new HashSet<int>();
            public bool HasSource { get; set; }
            public bool HasTarget { get; set; }

            public RowItem(TableRow table, int kind, long row)
            {
                _table = table;
                Kind = kind;
                Row = row;
            }

            public string this[string key]
            {
                get => _values.TryGetValue(key, out var v) ? v : "";
                set => _values[key] = value;
            }

            public bool CanSelect => Kind != Identical;
            public string Marker => Kind == Different ? "~" : Kind == OnlyInSource ? "+" : Kind == OnlyInTarget ? "−" : "=";

            public bool Selected
            {
                get => CanSelect && _table.IsRowSelected(Kind, Row);
                set
                {
                    if (!CanSelect) return;
                    _table.SetRow(Kind, Row, value);
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Selected)));
                }
            }

            public void Refresh() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Selected)));

            /// <summary>Whether column c's cell on a side differs (rows only on one side differ everywhere).</summary>
            public bool CellDiffers(int c) => Kind == OnlyInSource || Kind == OnlyInTarget || (Kind == Different && Differs.Contains(c));

            public event PropertyChangedEventHandler PropertyChanged;
        }

        private const int PageSize = 200;
        private readonly CompareSession _session;
        private readonly ObservableCollection<TableRow> _tables = new ObservableCollection<TableRow>();
        private readonly ListCollectionView _tableView;
        private readonly DataGrid _tableGrid;
        private readonly TabControl _kinds = new TabControl();
        private readonly string _sourceName, _targetName;
        private readonly TextBlock _selectedText = Ui.Text("", brush: CompareTheme.Muted);
        private readonly TextBox _findTables = Ui.SearchBox("Group objects as you type", 220);
        private readonly ComboBox _groupBy = new ComboBox { Width = 170, VerticalAlignment = VerticalAlignment.Center };

        // The rows pane, shared by the tabs.
        private readonly DockPanel _rowsPanel = new DockPanel();
        private readonly DataGrid _rows = new DataGrid();
        private readonly TextBlock _range = Ui.Text("", brush: CompareTheme.Muted);
        private readonly TextBlock _rowsNote = Ui.Text("", brush: CompareTheme.Muted);
        private readonly TextBox _find = Ui.SearchBox("Find in rows", 200);
        private readonly CheckBox _showIdenticalColumns;
        private readonly Dictionary<DataGridColumn, (int Column, string Side)> _columnOf = new Dictionary<DataGridColumn, (int, string)>();
        private TableRow _shownTable;
        private int _shownKind = -1;
        private int _page;
        private long _total;
        private JsonObject _sort;
        private DataGridColumn _sortColumn;

        public long Session { get; }

        public DataResults(CompareSession session, JsonObject result)
        {
            _session = session;
            Session = result.Long("session");
            _sourceName = result.Str("sourceName");
            _targetName = result.Str("targetName");
            foreach (var t in result.List("tables").OfType<JsonObject>())
            {
                var counts = t.List("counts").Select(c => c is double d ? (long)d : 0).ToList();
                while (counts.Count < 4) counts.Add(0);
                var comparable = t.Str("error") == null;
                var differing = counts[Different] + counts[OnlyInSource] + counts[OnlyInTarget];
                var status = t.Str("error") ?? (t.Bool("byChecksum") ? "identical (checksum)" : t.Bool("repeatedKeys") ? "key values repeat" : differing == 0 ? "identical" : "");
                var row = new TableRow
                {
                    Index = (int)t.Long("index"),
                    Name = t.Str("source") + (t.Str("source") != t.Str("target") ? " → " + t.Str("target") : ""),
                    Counts = counts.ToArray(),
                    Status = status,
                    Comparable = comparable,
                    Rank = !comparable ? 2 : differing > 0 ? 0 : 1,
                    Columns = t.Strings("columns"),
                    TargetColumns = t.Strings("targetColumns"),
                    Key = new HashSet<int>(t.List("key").Select(k => k is double d ? (int)d : -1)),
                    ColumnDifferences = t.List("columnDifferences").Select(c => c is double d ? (long)d : 0).ToList(),
                };
                if (!comparable) foreach (var k in new[] { Different, OnlyInSource, OnlyInTarget }) row.Selection[k].Included = false;
                row.SelectionChanged += UpdateSelectedText;
                row.SelectionChanged += () => { if (row == _shownTable) foreach (var r in _rows.Items.OfType<RowItem>()) r.Refresh(); };
                _tables.Add(row);
            }
            _tableView = (ListCollectionView)CollectionViewSource.GetDefaultView(_tables);
            _tableGrid = new DataGrid
            {
                ItemsSource = _tableView,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
                HeadersVisibility = DataGridHeadersVisibility.Column,
                SelectionMode = DataGridSelectionMode.Extended,
            };
            _tableGrid.Columns.Add(new DataGridTemplateColumn { Header = "All different", Width = 120, CellTemplate = KindCell("AllState", "AllDifferent", "HasAny") });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Table", Binding = new Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            _tableGrid.Columns.Add(new DataGridTemplateColumn { Header = new TextBlock { Text = "Only in " + _sourceName }, Width = 150, CellTemplate = KindCell("OnlyInSourceState", "OnlyInSource", "HasOnlyInSource") });
            _tableGrid.Columns.Add(new DataGridTemplateColumn { Header = "Different", Width = 120, CellTemplate = KindCell("DifferentState", "Different", "HasDifferent") });
            _tableGrid.Columns.Add(new DataGridTemplateColumn { Header = new TextBlock { Text = "Only in " + _targetName }, Width = 150, CellTemplate = KindCell("OnlyInTargetState", "OnlyInTarget", "HasOnlyInTarget") });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Identical", Binding = new Binding("Identical") { StringFormat = "N0" }, IsReadOnly = true, Width = 100, ElementStyle = Ui.RightAligned() });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Status", Binding = new Binding("Status"), IsReadOnly = true, Width = 180 });
            _tableGrid.GroupStyle.Add(GroupHeader());
            _tableGrid.SelectionChanged += async (s, e) => { if (_tableGrid.SelectedItem is TableRow t && t != _shownTable) await ShowRows(t, _shownKind < 0 ? Different : _shownKind, true); };
            _tableGrid.ContextMenu = TableMenu();
            foreach (var label in new[] { "Type of difference", "No groups" }) _groupBy.Items.Add(label);
            _groupBy.SelectedIndex = 0;
            _groupBy.SelectionChanged += (s, e) => Regroup();
            _findTables.TextChanged += (s, e) => { _groupBy.IsEnabled = _findTables.Text.Length == 0; Regroup(); };
            Regroup();

            long total(Func<TableRow, long> f) => _tables.Sum(f);
            var badges = Ui.Row(
                Ui.Badge(Ui.Plural(_tables.Count, "table", "tables"), (Brush)CompareTheme.Resources["Q.Subtle"], CompareTheme.Muted),
                Ui.Badge(total(t => t.Different).ToString("N0") + " different rows", CompareTheme.Changed, CompareTheme.Medium),
                Ui.Badge(total(t => t.OnlyInSource).ToString("N0") + " only in " + _sourceName, CompareTheme.Added, CompareTheme.Success),
                Ui.Badge(total(t => t.OnlyInTarget).ToString("N0") + " only in " + _targetName, CompareTheme.Removed, CompareTheme.High),
                _selectedText);
            var groupLabel = Ui.Text("Group by", brush: CompareTheme.Muted);
            groupLabel.Margin = new Thickness(12, 0, 8, 0);
            var tools = Ui.Row(
                Ui.Command(Glyph.Check, "Include all", () => SetAll(true), "Deploy every differing row (Ctrl+I)"),
                Ui.Command(Glyph.Delete, "Exclude all", () => SetAll(false), "Deploy nothing (Ctrl+Shift+I)"),
                groupLabel, _groupBy, new Border { Width = 8 }, _findTables);
            var toolbar = new DockPanel();
            DockPanel.SetDock(tools, Dock.Right);
            toolbar.Children.Add(tools);
            toolbar.Children.Add(badges);
            var missing = (result.Strings("onlyIn1").Count > 0 ? "Tables only in " + _sourceName + ": " + string.Join(", ", result.Strings("onlyIn1")) + ". " : "")
                + (result.Strings("onlyIn2").Count > 0 ? "Tables only in " + _targetName + ": " + string.Join(", ", result.Strings("onlyIn2")) + "." : "");
            var summary = Ui.Column(toolbar);
            if (missing.Length > 0) summary.Children.Add(Ui.Text(missing, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            summary.Margin = new Thickness(28, 6, 28, 10);

            // Tabs, left to right as SQL Data Compare has them; the grid below is shared.
            foreach (var (label, kind, tip) in new[]
            {
                ("All rows", AllRows, "Every row that differs, in comparison order"),
                ("Only in " + _sourceName, OnlyInSource, "Rows only in " + _sourceName),
                ("Different", Different, "Rows that exist in both databases but differ"),
                ("Only in " + _targetName, OnlyInTarget, "Rows only in " + _targetName),
                ("Identical", Identical, "Rows that are the same in both databases"),
            })
            {
                _kinds.Items.Add(new TabItem { Header = new TextBlock { Text = label }, Tag = kind, ToolTip = tip });
            }
            _kinds.SelectedIndex = 2;
            _kinds.SelectionChanged += async (s, e) =>
            {
                if (e.Source != _kinds || !(_kinds.SelectedItem is TabItem tab)) return;
                foreach (TabItem other in _kinds.Items) if (other != tab) other.Content = null;
                tab.Content = _rowsPanel;
                if (_shownTable != null) await ShowRows(_shownTable, (int)tab.Tag, true);
            };
            ((TabItem)_kinds.Items[2]).Content = _rowsPanel;

            _showIdenticalColumns = Ui.Check("Show identical columns", true, _ => { if (_shownTable != null) BuildColumns(); });
            _rows.AutoGenerateColumns = false;
            _rows.CanUserAddRows = false;
            _rows.SelectionUnit = DataGridSelectionUnit.CellOrRowHeader;
            _rows.SelectionMode = DataGridSelectionMode.Extended;
            _rows.Sorting += async (s, e) => { e.Handled = true; await SortBy(e.Column); };
            _rows.MouseDoubleClick += async (s, e) => { if (_rows.CurrentCell.Column != null && _columnOf.ContainsKey(_rows.CurrentCell.Column)) await ShowValue(); };
            _rows.PreviewKeyDown += (s, e) =>
            {
                // Space ticks or clears the selected rows.
                if (e.Key != Key.Space || e.OriginalSource is CheckBox) return;
                var items = _rows.SelectedCells.Select(c => c.Item).OfType<RowItem>().Distinct().Where(r => r.CanSelect).ToList();
                if (items.Count == 0) return;
                var on = !items.All(r => r.Selected);
                foreach (var r in items) r.Selected = on;
                e.Handled = true;
            };
            _find.KeyDown += async (s, e) => { if (e.Key == Key.Enter) await Find((Keyboard.Modifiers & ModifierKeys.Shift) == 0); };
            var nav = Ui.Row(
                Ui.IconButton(Glyph.Back, "Previous difference", async () => await NextCell(-1)),
                Ui.IconButton(Glyph.Up, "Previous record with a difference", async () => await NextRecord(-1)),
                Ui.IconButton(Glyph.Down, "Next record with a difference", async () => await NextRecord(1)),
                Ui.IconButton(Glyph.Forward, "Next difference", async () => await NextCell(1)),
                new Border { Width = 12 },
                _find,
                Ui.IconButton(Glyph.Search, "Find next (Enter; Shift+Enter finds the previous one)", async () => await Find(true)),
                new Border { Width = 8 },
                _showIdenticalColumns,
                Ui.Command(Glyph.View, "Show value…", async () => await ShowValue(), "The whole value of the selected cell (double-click a cell)"));
            nav.Margin = new Thickness(8, 6, 8, 2);
            _range.Margin = new Thickness(8, 0, 0, 0);
            var pager = Ui.Row(
                Ui.IconButton(Glyph.Previous, "Previous page", async () => { if (_page > 0) await LoadPage(_page - 1); }),
                Ui.IconButton(Glyph.Next, "Next page", async () => { if ((_page + 1) * PageSize < _total) await LoadPage(_page + 1); }),
                _range, new Border { Width = 12 }, _rowsNote);
            pager.Margin = new Thickness(8, 4, 8, 6);
            DockPanel.SetDock(nav, Dock.Top);
            DockPanel.SetDock(pager, Dock.Bottom);
            _rowsPanel.Children.Add(nav);
            _rowsPanel.Children.Add(pager);
            _rowsPanel.Children.Add(_rows);

            var split = new Grid { Margin = new Thickness(16, 0, 16, 16) };
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            split.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.4, GridUnitType.Star) });
            var splitter = new GridSplitter { Height = 12, HorizontalAlignment = HorizontalAlignment.Stretch, ResizeBehavior = GridResizeBehavior.PreviousAndNext };
            var tablesCard = Ui.Card(_tableGrid, 4);
            var rowsCard = Ui.Card(_kinds, 0);
            Grid.SetRow(splitter, 1);
            Grid.SetRow(rowsCard, 2);
            split.Children.Add(tablesCard);
            split.Children.Add(splitter);
            split.Children.Add(rowsCard);
            SetDock(summary, Dock.Top);
            Children.Add(summary);
            Children.Add(split);
            InputBindings.Add(new KeyBinding(new Relay(() => SetAll(true)), Key.I, ModifierKeys.Control));
            InputBindings.Add(new KeyBinding(new Relay(() => SetAll(false)), Key.I, ModifierKeys.Control | ModifierKeys.Shift));
            InputBindings.Add(new KeyBinding(new Relay(() => _find.Focus()), Key.F, ModifierKeys.Control));
            UpdateSelectedText();
        }

        private sealed class Relay : ICommand
        {
            private readonly Action _run;
            public Relay(Action run) => _run = run;
            public event EventHandler CanExecuteChanged { add { } remove { } }
            public bool CanExecute(object parameter) => true;
            public void Execute(object parameter) => _run();
        }

        public string SourceName => _sourceName;
        public string TargetName => _targetName;
        public IEnumerable<TableRow> Tables => _tables;

        /// <summary>Rows chosen for deployment, in every table.</summary>
        public long SelectedRows => _tables.Sum(t => t.SelectedTotal);

        private void UpdateSelectedText()
        {
            var all = _tables.Sum(t => t.HasAny ? t.AllDifferent : 0);
            _selectedText.Text = all == 0 ? "All objects identical" : "Selected " + SelectedRows.ToString("N0") + " of " + all.ToString("N0") + " rows";
            _selectedText.Margin = new Thickness(6, 0, 0, 0);
        }

        public void SetAll(bool on)
        {
            foreach (var t in _tables.Where(t => t.Comparable)) t.AllState = on;
            foreach (var r in _rows.Items.OfType<RowItem>()) r.Refresh();
        }

        // ---- Tables grid ----

        /// <summary>A check box (when there are rows of the kind) and the count.</summary>
        private static DataTemplate KindCell(string state, string count, string has)
        {
            var panel = new FrameworkElementFactory(typeof(StackPanel));
            panel.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);
            panel.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Right);
            var box = new FrameworkElementFactory(typeof(CheckBox));
            box.SetBinding(ToggleButton.IsCheckedProperty, new Binding(state) { Mode = BindingMode.TwoWay, UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged });
            box.SetBinding(VisibilityProperty, new Binding(has) { Converter = new BooleanToVisibilityConverter() });
            box.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 8, 0));
            box.SetValue(FrameworkElement.ToolTipProperty, "Include in deployment");
            var text = new FrameworkElementFactory(typeof(TextBlock));
            text.SetBinding(TextBlock.TextProperty, new Binding(count) { StringFormat = "N0" });
            text.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            panel.AppendChild(box);
            panel.AppendChild(text);
            return new DataTemplate { VisualTree = panel };
        }

        private GroupStyle GroupHeader()
        {
            var groupStyle = new GroupStyle();
            var header = new FrameworkElementFactory(typeof(StackPanel));
            header.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);
            header.SetValue(FrameworkElement.MarginProperty, new Thickness(10, 12, 0, 4));
            var groupCount = new FrameworkElementFactory(typeof(TextBlock));
            groupCount.SetBinding(TextBlock.TextProperty, new Binding("ItemCount"));
            groupCount.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            var groupName = new FrameworkElementFactory(typeof(TextBlock));
            groupName.SetBinding(TextBlock.TextProperty, new Binding("Name") { Converter = new GroupLabel(this) });
            groupName.SetValue(TextBlock.ForegroundProperty, CompareTheme.Muted);
            groupName.SetValue(FrameworkElement.MarginProperty, new Thickness(6, 0, 0, 0));
            header.AppendChild(groupCount);
            header.AppendChild(groupName);
            groupStyle.HeaderTemplate = new DataTemplate { VisualTree = header };
            return groupStyle;
        }

        private sealed class GroupLabel : IValueConverter
        {
            private readonly DataResults _owner;
            public GroupLabel(DataResults owner) => _owner = owner;

            public object Convert(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture)
            {
                var key = value as string ?? "";
                var find = _owner._findTables.Text.Trim();
                if (find.Length > 0) return (key.StartsWith("0", StringComparison.Ordinal) ? "match '" : "do not match '") + find + "'";
                return key.Length > 1 ? key.Substring(1) : key;
            }

            public object ConvertBack(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture) => Binding.DoNothing;
        }

        private void Regroup()
        {
            var find = _findTables.Text.Trim();
            foreach (var t in _tables)
            {
                t.Group = find.Length > 0 ? (t.Name.IndexOf(find, StringComparison.OrdinalIgnoreCase) >= 0 ? "0" : "1")
                    : t.Rank == 0 ? "0different" : t.Rank == 1 ? "1identical" : "2that could not be compared";
            }
            using (_tableView.DeferRefresh())
            {
                _tableView.GroupDescriptions.Clear();
                _tableView.SortDescriptions.Clear();
                if (find.Length > 0 || _groupBy.SelectedIndex == 0)
                {
                    _tableView.GroupDescriptions.Add(new PropertyGroupDescription("Group"));
                    _tableView.SortDescriptions.Add(new SortDescription("Group", ListSortDirection.Ascending));
                }
                _tableView.SortDescriptions.Add(new SortDescription("Name", ListSortDirection.Ascending));
            }
        }

        /// <summary>Text for menus and check boxes, where an underscore would mark an access key.</summary>
        public static string Plain(string text) => (text ?? "").Replace("_", "__");

        private ContextMenu TableMenu()
        {
            var menu = new ContextMenu();
            CompareTheme.Attach(menu);
            IEnumerable<TableRow> chosen() => _tableGrid.SelectedItems.OfType<TableRow>().Where(t => t.Comparable);
            void set(int kind, bool on)
            {
                foreach (var t in chosen())
                {
                    if (kind < 0) t.AllState = on;
                    else t.SetKind(kind, on);
                }
                foreach (var r in _rows.Items.OfType<RowItem>()) r.Refresh();
            }
            menu.Items.Add(Ui.MenuItem("Include all", () => set(-1, true), Glyph.Check));
            menu.Items.Add(Ui.MenuItem("Exclude all", () => set(-1, false)));
            menu.Items.Add(new Separator());
            menu.Items.Add(Ui.MenuItem("Include all in " + Plain(_sourceName), () => set(OnlyInSource, true)));
            menu.Items.Add(Ui.MenuItem("Exclude all in " + Plain(_sourceName), () => set(OnlyInSource, false)));
            menu.Items.Add(Ui.MenuItem("Include all different", () => set(Different, true)));
            menu.Items.Add(Ui.MenuItem("Exclude all different", () => set(Different, false)));
            menu.Items.Add(Ui.MenuItem("Include all in " + Plain(_targetName), () => set(OnlyInTarget, true)));
            menu.Items.Add(Ui.MenuItem("Exclude all in " + Plain(_targetName), () => set(OnlyInTarget, false)));
            menu.Items.Add(new Separator());
            menu.Items.Add(Ui.MenuItem("Show object deployment script…", async () => await TableScript(), Glyph.Code));
            menu.Items.Add(Ui.MenuItem("Remap table or view…", () => { if (_tableGrid.SelectedItem is TableRow t) EditTable?.Invoke(SourceOf(t), "Table mapping"); }, Glyph.List));
            menu.Items.Add(Ui.MenuItem("Set comparison key…", () => { if (_tableGrid.SelectedItem is TableRow t) EditTable?.Invoke(SourceOf(t), "Tables & views"); }, Glyph.Edit));
            Dialogs.RegisterMenu(menu);
            return menu;
        }

        /// <summary>Raised to edit one table in the project: (source table, tab of the project editor).</summary>
        public event Action<string, string> EditTable;

        private static string SourceOf(TableRow t)
        {
            var at = t.Name.IndexOf(" → ", StringComparison.Ordinal);
            return at < 0 ? t.Name : t.Name.Substring(0, at);
        }

        /// <summary>The deployment script of the selected table's chosen rows on its own.</summary>
        private async Task TableScript()
        {
            if (!(_tableGrid.SelectedItem is TableRow t) || !t.Comparable) return;
            var all = Selections().OfType<JsonObject>().Where(x => x.Long("table") == t.Index).Cast<object>().ToList();
            var script = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("data_script", new JsonObject
            {
                ["session"] = Session,
                ["tables"] = new List<object> { t.Index },
                ["kinds"] = new List<object> { false, true, true, true },
                ["selections"] = all,
                ["isolationLevel"] = CompareSettings.Load().IsolationLevel,
            }));
            if (script != null) Dialogs.ShowScript(_session.Host, "Deployment script for " + t.Name, script.Str("sql"), _session.LiveTarget());
        }

        // ---- Deployment selection for data_script ----

        /// <summary>Tables with rows to deploy.</summary>
        public List<object> SelectedTables => _tables.Where(t => t.Comparable && t.SelectedTotal > 0).Select(t => (object)t.Index).ToList();

        /// <summary>The `selections` of `data_script`: per table, the kinds and the rows toggled in each.</summary>
        public List<object> Selections()
        {
            var list = new List<object>();
            foreach (var t in _tables.Where(t => t.Comparable && t.HasAny))
            {
                JsonObject kind(int k) => new JsonObject { ["included"] = t.Selection[k].Included, ["toggled"] = t.Selection[k].Toggled.OrderBy(x => x).Select(x => (object)x).ToList() };
                list.Add(new JsonObject
                {
                    ["table"] = t.Index,
                    ["kinds"] = new List<object> { false, t.SelectedCount(Different) > 0, t.SelectedCount(OnlyInSource) > 0, t.SelectedCount(OnlyInTarget) > 0 },
                    ["rows"] = new JsonObject { ["different"] = kind(Different), ["onlyIn1"] = kind(OnlyInSource), ["onlyIn2"] = kind(OnlyInTarget) },
                });
            }
            return list;
        }

        // ---- Rows pane ----

        private async Task ShowRows(TableRow table, int kind, bool resetSort)
        {
            _shownTable = table;
            _shownKind = kind;
            if (resetSort)
            {
                _sort = null;
                _sortColumn = null;
            }
            BuildColumns();
            await LoadPage(0);
        }

        /// <summary>Key columns once, every other column as a source and target pair; columns without differences can be hidden.</summary>
        private void BuildColumns()
        {
            var table = _shownTable;
            var kind = _shownKind;
            _rows.Columns.Clear();
            _columnOf.Clear();
            if (kind != Identical)
            {
                var box = new FrameworkElementFactory(typeof(CheckBox));
                box.SetBinding(ToggleButton.IsCheckedProperty, new Binding("Selected") { Mode = BindingMode.TwoWay, UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged });
                box.SetBinding(VisibilityProperty, new Binding("CanSelect") { Converter = new BooleanToVisibilityConverter() });
                box.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
                _rows.Columns.Add(new DataGridTemplateColumn { Header = "Include", Width = 64, CellTemplate = new DataTemplate { VisualTree = box }, CanUserSort = false });
            }
            if (kind == AllRows)
            {
                var marker = new DataGridTextColumn { Header = "", Binding = new Binding("Marker"), Width = 32, IsReadOnly = true, CanUserSort = false };
                marker.ElementStyle = new Style(typeof(TextBlock)) { Setters = { new Setter(TextBlock.FontWeightProperty, FontWeights.Bold), new Setter(HorizontalAlignmentProperty, HorizontalAlignment.Center) } };
                _rows.Columns.Add(marker);
            }
            var onlyIn = kind == OnlyInSource || kind == OnlyInTarget;
            for (var c = 0; c < table.Columns.Count; c++)
            {
                var differs = c < table.ColumnDifferences.Count && table.ColumnDifferences[c] > 0;
                if (table.Key.Contains(c))
                {
                    AddColumn(table.Columns[c], null, "k" + c, c, "key", false);
                    continue;
                }
                if (_showIdenticalColumns.IsChecked != true && !onlyIn && !differs) continue;
                var targetName = c < table.TargetColumns.Count ? table.TargetColumns[c] : table.Columns[c];
                AddColumn(table.Columns[c], _sourceName, "s" + c, c, "source", differs && !onlyIn);
                AddColumn(targetName, _targetName, "t" + c, c, "target", differs && !onlyIn);
            }
            if (_sortColumn != null)
            {
                var again = _rows.Columns.FirstOrDefault(col => _columnOf.TryGetValue(col, out var at) && at.Column == (int)_sort.Long("column") && at.Side == _sort.Str("side"));
                if (again != null) again.SortDirection = _sort.Bool("descending") ? ListSortDirection.Descending : ListSortDirection.Ascending;
                _sortColumn = again;
            }
        }

        private void AddColumn(string name, string side, string key, int index, string sideKey, bool differs)
        {
            var header = new StackPanel();
            header.Children.Add(new TextBlock { Text = name, FontWeight = differs ? FontWeights.SemiBold : FontWeights.Normal });
            if (side != null) header.Children.Add(new TextBlock { Text = side, FontSize = 11, Foreground = CompareTheme.Muted });
            var col = new DataGridTextColumn { Header = header, Binding = new Binding("[" + key + "]"), MaxWidth = 420, MinWidth = 50, IsReadOnly = true, SortMemberPath = key };
            var hs = new Style(typeof(DataGridColumnHeader), CompareTheme.Style(typeof(DataGridColumnHeader)));
            hs.Setters.Add(new Setter(System.Windows.Automation.AutomationProperties.NameProperty, side == null ? name : name + " (" + side + ")"));
            if (differs)
            {
                // Columns with differences have a highlighted header.
                hs.Setters.Add(new Setter(Control.BackgroundProperty, CompareTheme.Resources["Q.Selected"]));
                hs.Setters.Add(new Setter(Control.ForegroundProperty, CompareTheme.Foreground));
            }
            col.HeaderStyle = hs;
            var style = new Style(typeof(DataGridCell), CompareTheme.Style(typeof(DataGridCell)));
            var absent = new DataTrigger { Binding = new Binding(sideKey == "target" ? "HasTarget" : "HasSource"), Value = false };
            if (sideKey != "key") absent.Setters.Add(new Setter(Control.BackgroundProperty, CompareTheme.Resources["Q.Subtle"]));
            var different = new DataTrigger { Binding = new Binding { Converter = new CellDiffers(index) }, Value = true };
            different.Setters.Add(new Setter(Control.BackgroundProperty, CompareTheme.Changed));
            var selected = new Trigger { Property = DataGridCell.IsSelectedProperty, Value = true };
            selected.Setters.Add(new Setter(Control.BackgroundProperty, CompareTheme.Resources["Q.Selected"]));
            selected.Setters.Add(new Setter(Control.BorderBrushProperty, CompareTheme.Accent));
            if (sideKey != "key") style.Triggers.Add(different);
            style.Triggers.Add(absent);
            style.Triggers.Add(selected);
            col.CellStyle = style;
            _columnOf[col] = (index, sideKey);
            _rows.Columns.Add(col);
        }

        private sealed class CellDiffers : IValueConverter
        {
            private readonly int _index;
            public CellDiffers(int index) => _index = index;
            public object Convert(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture) => value is RowItem r && r.CellDiffers(_index);
            public object ConvertBack(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture) => Binding.DoNothing;
        }

        private async Task SortBy(DataGridColumn column)
        {
            if (!_columnOf.TryGetValue(column, out var at) || _shownTable == null) return;
            var descending = _sortColumn == column && _sort != null && !_sort.Bool("descending");
            foreach (var c in _rows.Columns) c.SortDirection = null;
            column.SortDirection = descending ? ListSortDirection.Descending : ListSortDirection.Ascending;
            _sortColumn = column;
            _sort = new JsonObject { ["column"] = at.Column, ["side"] = at.Side, ["descending"] = descending };
            _rowsNote.Text = "Sorting…";
            await LoadPage(0);
        }

        private async Task<bool> LoadPage(int page)
        {
            var table = _shownTable;
            var kind = _shownKind;
            if (table == null) return false;
            var request = new JsonObject { ["session"] = Session, ["table"] = table.Index, ["kind"] = kind, ["start"] = page * PageSize, ["count"] = PageSize };
            if (_sort != null) request["sort"] = _sort;
            var reply = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("data_rows", request));
            if (reply == null || table != _shownTable || kind != _shownKind) return false;
            _page = page;
            _total = reply.Long("total");
            var items = new List<RowItem>();
            foreach (var r in reply.List("rows").OfType<JsonObject>())
            {
                var item = new RowItem(table, (int)r.Long("kind"), r.Long("row"));
                var src = r.Strings("source");
                var tgt = r.Strings("target");
                item.HasSource = src.Count > 0;
                item.HasTarget = tgt.Count > 0 || item.Kind == Identical;
                for (var c = 0; c < table.Columns.Count; c++)
                {
                    var s = c < src.Count ? src[c] : null;
                    var t = c < tgt.Count ? tgt[c] : null;
                    item["k" + c] = s ?? t ?? "";
                    item["s" + c] = s ?? "";
                    item["t" + c] = item.Kind == Identical ? s ?? t ?? "" : t ?? "";
                }
                var i = 0;
                foreach (var d in r.List("differs"))
                {
                    if (d is bool b && b) item.Differs.Add(i);
                    i++;
                }
                items.Add(item);
            }
            _rows.ItemsSource = items;
            // A new item source clears the sort arrows.
            if (_sortColumn != null) _sortColumn.SortDirection = _sort.Bool("descending") ? ListSortDirection.Descending : ListSortDirection.Ascending;
            var start = page * PageSize;
            _range.Text = "Rows " + (_total == 0 ? 0 : start + 1).ToString("N0") + "–" + Math.Min(start + PageSize, _total).ToString("N0") + " of " + _total.ToString("N0");
            _rowsNote.Text = kind == Identical && _total == 0 && table.Identical > 0
                ? "Identical rows are counted but not kept; turn on 'Show identical values in results'."
                : kind == AllRows && table.Identical > 0 && _total < table.AllDifferent + table.Identical ? "Identical rows were excluded from the project." : "";
            return true;
        }

        private RowItem CurrentRow => _rows.CurrentCell.Item as RowItem ?? _rows.SelectedCells.Select(c => c.Item).OfType<RowItem>().FirstOrDefault();

        private int CurrentPosition()
        {
            var row = CurrentRow;
            var i = row == null ? -1 : _rows.Items.IndexOf(row);
            return i < 0 ? -1 : _page * PageSize + i;
        }

        /// <summary>Selects one cell (or the first data cell of a row) and scrolls to it.</summary>
        private void Select(int indexInPage, DataGridColumn column = null)
        {
            if (indexInPage < 0 || indexInPage >= _rows.Items.Count) return;
            var item = _rows.Items[indexInPage];
            column = column ?? _rows.Columns.FirstOrDefault(c => _columnOf.ContainsKey(c));
            if (column == null) return;
            _rows.SelectedCells.Clear();
            var cell = new DataGridCellInfo(item, column);
            _rows.CurrentCell = cell;
            _rows.SelectedCells.Add(cell);
            _rows.ScrollIntoView(item, column);
            _rows.Focus();
        }

        private async Task<bool> GoTo(long position, DataGridColumn column = null)
        {
            if (position < 0 || position >= _total) return false;
            var page = (int)(position / PageSize);
            if (page != _page && !await LoadPage(page)) return false;
            Select((int)(position % PageSize), column);
            return true;
        }

        /// <summary>The previous or next row with a difference (identical rows are skipped).</summary>
        private async Task NextRecord(int step)
        {
            var pos = CurrentPosition();
            for (var p = pos + step; p >= 0 && p < _total; p += step)
            {
                if ((int)(p / PageSize) != _page && !await LoadPage((int)(p / PageSize))) return;
                if (_rows.Items[(int)(p % PageSize)] is RowItem r && r.Kind != Identical)
                {
                    Select((int)(p % PageSize));
                    return;
                }
            }
        }

        /// <summary>The previous or next differing cell, row after row.</summary>
        private async Task NextCell(int step)
        {
            var columns = _rows.Columns.Where(c => _columnOf.TryGetValue(c, out var at) && at.Side != "key").ToList();
            if (columns.Count == 0) return;
            var pos = Math.Max(CurrentPosition(), step > 0 ? 0 : -1);
            var current = _rows.CurrentCell.Column;
            var col = current != null && columns.Contains(current) ? columns.IndexOf(current) : (step > 0 ? -1 : columns.Count);
            for (var p = pos; p >= 0 && p < _total; p += step)
            {
                if ((int)(p / PageSize) != _page && !await LoadPage((int)(p / PageSize))) return;
                if (!(_rows.Items[(int)(p % PageSize)] is RowItem r)) return;
                for (var c = col + step; c >= 0 && c < columns.Count; c += step)
                {
                    if (r.CellDiffers(_columnOf[columns[c]].Column))
                    {
                        Select((int)(p % PageSize), columns[c]);
                        return;
                    }
                }
                col = step > 0 ? -1 : columns.Count;
            }
        }

        private async Task Find(bool forward)
        {
            var text = _find.Text;
            if (string.IsNullOrEmpty(text) || _shownTable == null) return;
            var pos = CurrentPosition();
            var start = forward ? pos + 1 : Math.Max(pos, 0);
            for (var attempt = 0; attempt < 2; attempt++)
            {
                var request = new JsonObject { ["session"] = Session, ["table"] = _shownTable.Index, ["kind"] = _shownKind, ["text"] = text, ["start"] = start, ["backward"] = !forward };
                if (_sort != null) request["sort"] = _sort;
                var found = await Ui.Try(_session.Host.Owner, async () => new JsonObject { ["at"] = await _session.Engine.Request("data_find", request) });
                if (found == null) return;
                if (found["at"] is double at)
                {
                    await GoTo((long)at);
                    return;
                }
                if (attempt == 1 || !Ui.Confirm(_session.Host.Owner, "Could not find any instances of the search string. Search again from the " + (forward ? "beginning" : "end") + " of the table?")) return;
                start = forward ? 0 : (int)_total;
            }
        }

        /// <summary>
        /// "Value details": the whole value of the selected cell's column on both sides (the grid
        /// cuts values at 256 characters), as text line against line, as bytes, or as an image.
        /// </summary>
        private async Task ShowValue()
        {
            var cell = _rows.CurrentCell.Column == null && _rows.SelectedCells.Count > 0 ? _rows.SelectedCells[0] : _rows.CurrentCell;
            if (!(cell.Item is RowItem row) || cell.Column == null || !_columnOf.TryGetValue(cell.Column, out var at))
            {
                Ui.Error(_session.Host.Owner, "Select a cell first.");
                return;
            }
            async Task<JsonObject> side(string name) => await Ui.Try(_session.Host.Owner, async () => new JsonObject { ["v"] = await _session.Engine.Request("data_value", new JsonObject
            {
                ["session"] = Session, ["table"] = _shownTable.Index, ["kind"] = row.Kind, ["row"] = row.Row, ["column"] = at.Column, ["side"] = name,
            }) });
            var one = row.HasSource ? await side("source") : new JsonObject();
            if (one == null) return;
            var two = row.HasTarget && row.Kind != Identical ? await side("target") : row.Kind == Identical ? one : new JsonObject();
            if (two == null) return;
            var column = _shownTable.Columns[at.Column];
            ValueViewer.Show(_session.Host.Owner, _shownTable.Name + ".[" + column + "]", _sourceName, row.HasSource ? one.Str("v") : null, row.HasSource, _targetName, two.Str("v"), row.HasTarget || row.Kind == Identical);
        }
    }
}
