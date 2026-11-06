using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Documents;
using System.Windows.Media;

namespace QuillSql.Compare
{
    /// <summary>Schema comparison results: the objects that differ, and the SQL of the selected one side by side.</summary>
    internal sealed class SchemaResults : DockPanel
    {
        public sealed class Row : INotifyPropertyChanged
        {
            private bool _selected;
            public int Index { get; set; }
            public string Type { get; set; }
            public string Name { get; set; }
            public string Status { get; set; }
            public string Marker { get; set; }
            public string Kind { get; set; }
            public JsonObject Data { get; set; }

            public bool Selected
            {
                get => _selected;
                set { _selected = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Selected))); }
            }

            public event PropertyChangedEventHandler PropertyChanged;
        }

        private readonly CompareSession _session;
        private readonly ObservableCollection<Row> _rows = new ObservableCollection<Row>();
        private readonly ICollectionView _view;
        private readonly DataGrid _grid;
        private readonly RichTextBox _left = SideBox(), _right = SideBox();
        private readonly TextBlock _leftTitle, _rightTitle, _summary;
        private readonly TreeView _properties = new TreeView { BorderThickness = new Thickness(0) };
        private readonly CheckBox _showIdentical;
        private readonly TextBox _filter = new TextBox { Width = 220 };

        public long Session { get; }

        public SchemaResults(CompareSession session, JsonObject result)
        {
            _session = session;
            Session = result.Long("session");
            Background = CompareTheme.Background;
            foreach (var d in result.List("differences").OfType<JsonObject>())
            {
                var kind = d.Str("kind");
                _rows.Add(new Row
                {
                    Index = (int)d.Long("index"),
                    Type = d.Str("type"),
                    Name = (string.IsNullOrEmpty(d.Str("schema")) ? "" : "[" + d.Str("schema") + "].") + "[" + d.Str("name") + "]" + (d.Str("targetName") != null ? " → " + d.Str("targetName") : ""),
                    Kind = kind,
                    Marker = kind == "Equal" ? "==" : kind == "Different" ? "<>" : kind == "OnlyIn1" ? ">>" : "<<",
                    Status = kind == "Equal" ? "Identical" : kind == "Different" ? (d.Bool("trivial") ? "Different (extended properties only)" : "Different") : kind == "OnlyIn1" ? "Only in " + result.Str("sourceName") : "Only in " + result.Str("targetName"),
                    Selected = d.Bool("selected"),
                    Data = d,
                });
            }
            _view = CollectionViewSource.GetDefaultView(_rows);
            _view.GroupDescriptions.Add(new PropertyGroupDescription("Status"));
            _showIdentical = Ui.Check("Show identical objects", false, _ => _view.Refresh());
            _view.Filter = o =>
            {
                var r = (Row)o;
                if (r.Kind == "Equal" && _showIdentical.IsChecked != true) return false;
                return string.IsNullOrWhiteSpace(_filter.Text) || (r.Name + " " + r.Type).IndexOf(_filter.Text, StringComparison.OrdinalIgnoreCase) >= 0;
            };
            _filter.TextChanged += (s, e) => _view.Refresh();

            _grid = new DataGrid
            {
                ItemsSource = _view,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
                IsReadOnly = false,
                SelectionMode = DataGridSelectionMode.Single,
                HeadersVisibility = DataGridHeadersVisibility.Column,
                GridLinesVisibility = DataGridGridLinesVisibility.None,
                Background = CompareTheme.Background,
            };
            _grid.Columns.Add(new DataGridCheckBoxColumn { Header = "Deploy", Binding = new Binding("Selected") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged }, Width = 60 });
            _grid.Columns.Add(new DataGridTextColumn { Header = "Type", Binding = new Binding("Type"), IsReadOnly = true, Width = 130 });
            _grid.Columns.Add(new DataGridTextColumn { Header = "", Binding = new Binding("Marker"), IsReadOnly = true, Width = 34 });
            _grid.Columns.Add(new DataGridTextColumn { Header = "Object", Binding = new Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            var groupStyle = new GroupStyle();
            var header = new FrameworkElementFactory(typeof(TextBlock));
            header.SetBinding(TextBlock.TextProperty, new Binding("Name"));
            header.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            header.SetValue(FrameworkElement.MarginProperty, new Thickness(4, 8, 0, 2));
            groupStyle.HeaderTemplate = new DataTemplate { VisualTree = header };
            _grid.GroupStyle.Add(groupStyle);
            _grid.SelectionChanged += async (s, e) => await ShowSelected();

            var counts = _rows.GroupBy(r => r.Kind).ToDictionary(g => g.Key, g => g.Count());
            int n(string k) => counts.TryGetValue(k, out var c) ? c : 0;
            _summary = Ui.Text($"{n("Different")} different, {n("OnlyIn1")} only in {result.Str("sourceName")}, {n("OnlyIn2")} only in {result.Str("targetName")}, {n("Equal")} identical",
                brush: CompareTheme.Muted);
            var autoMapped = result.List("autoMapped").OfType<JsonObject>().ToList();
            if (autoMapped.Count > 0)
            {
                _summary.Text += ". Columns paired by similarity: " + string.Join(", ", autoMapped.Select(a => a.Str("table") + ".[" + a.Str("source") + "] → [" + a.Str("target") + "]"));
            }
            var toolbar = Ui.Row(
                Ui.Text("Filter:"), _filter, new Border { Width = 10 }, _showIdentical,
                Ui.Button("Select all", () => { foreach (var r in _rows.Where(r => r.Kind != "Equal")) r.Selected = true; }),
                Ui.Button("Select none", () => { foreach (var r in _rows) r.Selected = false; }));
            var top = Ui.Column(toolbar, _summary);
            top.Margin = new Thickness(8, 4, 8, 4);

            _leftTitle = Ui.Text(result.Str("sourceName"), bold: true);
            _rightTitle = Ui.Text(result.Str("targetName"), bold: true);
            var sides = new Grid();
            sides.ColumnDefinitions.Add(new ColumnDefinition());
            sides.ColumnDefinitions.Add(new ColumnDefinition());
            sides.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            sides.RowDefinitions.Add(new RowDefinition());
            Grid.SetColumn(_rightTitle, 1);
            Grid.SetRow(_left, 1);
            Grid.SetRow(_right, 1);
            Grid.SetColumn(_right, 1);
            _leftTitle.Margin = _rightTitle.Margin = new Thickness(4, 2, 4, 2);
            sides.Children.Add(_leftTitle);
            sides.Children.Add(_rightTitle);
            sides.Children.Add(_left);
            sides.Children.Add(_right);
            SyncScroll(_left, _right);

            var bottomTabs = new TabControl();
            bottomTabs.Items.Add(new TabItem { Header = "SQL side by side", Content = sides });
            bottomTabs.Items.Add(new TabItem { Header = "Property differences", Content = _properties });

            var split = new Grid();
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            split.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.2, GridUnitType.Star) });
            var splitter = new GridSplitter { Height = 5, HorizontalAlignment = HorizontalAlignment.Stretch, Background = CompareTheme.Border };
            Grid.SetRow(splitter, 1);
            Grid.SetRow(bottomTabs, 2);
            split.Children.Add(_grid);
            split.Children.Add(splitter);
            split.Children.Add(bottomTabs);

            SetDock(top, Dock.Top);
            Children.Add(top);
            Children.Add(split);
        }

        /// <summary>Indexes of the differences ticked for deployment.</summary>
        public List<object> Selected => _rows.Where(r => r.Selected && r.Kind != "Equal").Select(r => (object)r.Index).ToList();

        /// <summary>Objects unticked, for the project file.</summary>
        public List<object> Deselected => _rows.Where(r => !r.Selected && r.Kind != "Equal")
            .Select(r => (object)new JsonObject { ["type"] = r.Type, ["schema"] = r.Data.Str("schema"), ["name"] = r.Data.Str("name") }).ToList();

        private static RichTextBox SideBox()
        {
            var box = new RichTextBox
            {
                IsReadOnly = true,
                FontFamily = CompareTheme.Code,
                FontSize = 12,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                Background = CompareTheme.Background,
                Foreground = CompareTheme.Foreground,
            };
            box.Document.PageWidth = 4000;
            return box;
        }

        private static void SyncScroll(RichTextBox a, RichTextBox b)
        {
            var busy = false;
            void link(RichTextBox from, RichTextBox to)
            {
                from.AddHandler(ScrollViewer.ScrollChangedEvent, new ScrollChangedEventHandler((s, e) =>
                {
                    if (busy) return;
                    busy = true;
                    to.ScrollToVerticalOffset(e.VerticalOffset);
                    to.ScrollToHorizontalOffset(e.HorizontalOffset);
                    busy = false;
                }));
            }
            link(a, b);
            link(b, a);
        }

        private async System.Threading.Tasks.Task ShowSelected()
        {
            if (!(_grid.SelectedItem is Row row)) return;
            ShowProperties(row.Data);
            var sql = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("schema_object_sql", new JsonObject { ["session"] = Session, ["index"] = row.Index }));
            if (sql == null || !ReferenceEquals(_grid.SelectedItem, row)) return;
            var lines = LineDiff.Align(sql.Str("source"), sql.Str("target"));
            Fill(_left, lines, true);
            Fill(_right, lines, false);
        }

        private static void Fill(RichTextBox box, List<LineDiff.Line> lines, bool left)
        {
            var doc = new FlowDocument { PageWidth = 4000, FontFamily = CompareTheme.Code, FontSize = 12 };
            var p = new Paragraph { Margin = new Thickness(0), LineHeight = 16 };
            foreach (var line in lines)
            {
                var text = left ? line.Left : line.Right;
                var run = new Run((text ?? "") + "\n");
                switch (line.Kind)
                {
                    case LineDiff.Kind.Changed: run.Background = CompareTheme.Changed; break;
                    case LineDiff.Kind.OnlyLeft: run.Background = left ? CompareTheme.Added : CompareTheme.Panel; break;
                    case LineDiff.Kind.OnlyRight: run.Background = left ? CompareTheme.Panel : CompareTheme.Removed; break;
                }
                p.Inlines.Add(run);
            }
            doc.Blocks.Add(p);
            box.Document = doc;
        }

        private void ShowProperties(JsonObject d)
        {
            _properties.Items.Clear();
            var nodes = new Dictionary<string, TreeViewItem>();
            foreach (var p in d.List("properties").OfType<JsonObject>())
            {
                ItemsControl parent = _properties;
                var path = "";
                foreach (var f in p.List("location").OfType<JsonObject>())
                {
                    var label = f.Str("name") + " " + (f.Str("source") == f.Str("target") ? f.Str("source") : (f.Str("source") ?? "(none)") + " / " + (f.Str("target") ?? "(none)"));
                    path += "/" + label;
                    if (!nodes.TryGetValue(path, out var node))
                    {
                        node = new TreeViewItem { Header = label, IsExpanded = true };
                        parent.Items.Add(node);
                        nodes[path] = node;
                    }
                    parent = node;
                }
                var s = p.Str("source");
                var t = p.Str("target");
                var text = s != null && t == null ? p.Str("name") + " " + s + ": only in source"
                    : s == null && t != null ? p.Str("name") + " " + t + ": only in target"
                    : p.Str("name") + ": " + s + "  →  " + t;
                parent.Items.Add(new TreeViewItem { Header = text });
            }
            if (_properties.Items.Count == 0)
            {
                _properties.Items.Add(new TreeViewItem { Header = d.Str("kind") == "Equal" ? "The object is identical." : "The whole object exists on one side only." });
            }
        }
    }

    /// <summary>Data comparison results: one row per table, and the differing rows of the selected table.</summary>
    internal sealed class DataResults : DockPanel
    {
        public sealed class TableRow : INotifyPropertyChanged
        {
            private bool _selected = true;
            public int Index { get; set; }
            public string Name { get; set; }
            public long Different { get; set; }
            public long OnlyInSource { get; set; }
            public long OnlyInTarget { get; set; }
            public long Identical { get; set; }
            public string Status { get; set; }
            public List<string> Columns { get; set; }
            public HashSet<int> Key { get; set; }

            public bool Selected
            {
                get => _selected;
                set { _selected = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Selected))); }
            }

            public event PropertyChangedEventHandler PropertyChanged;
        }

        private const int PageSize = 200;
        private readonly CompareSession _session;
        private readonly ObservableCollection<TableRow> _tables = new ObservableCollection<TableRow>();
        private readonly DataGrid _tableGrid;
        private readonly TabControl _kinds = new TabControl();
        private readonly string[] _kindNames;
        private readonly int[] _page = new int[4];

        public long Session { get; }

        public DataResults(CompareSession session, JsonObject result)
        {
            _session = session;
            Session = result.Long("session");
            Background = CompareTheme.Background;
            _kindNames = new[] { "Identical", "Different", "Only in " + result.Str("sourceName"), "Only in " + result.Str("targetName") };
            foreach (var t in result.List("tables").OfType<JsonObject>())
            {
                var counts = t.List("counts").Select(c => c is double d ? (long)d : 0).ToList();
                while (counts.Count < 4) counts.Add(0);
                var status = t.Str("error") ?? (t.Bool("byChecksum") ? "identical (checksum)" : t.Bool("repeatedKeys") ? "key values repeat" : counts[1] + counts[2] + counts[3] == 0 ? "identical" : "");
                _tables.Add(new TableRow
                {
                    Index = (int)t.Long("index"),
                    Name = t.Str("source") + (t.Str("source") != t.Str("target") ? " → " + t.Str("target") : ""),
                    Identical = counts[0],
                    Different = counts[1],
                    OnlyInSource = counts[2],
                    OnlyInTarget = counts[3],
                    Status = status,
                    Columns = t.Strings("columns"),
                    Key = new HashSet<int>(t.List("key").Select(k => k is double d ? (int)d : -1)),
                    Selected = t.Str("error") == null && counts[1] + counts[2] + counts[3] > 0,
                });
            }
            _tableGrid = new DataGrid
            {
                ItemsSource = _tables,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
                HeadersVisibility = DataGridHeadersVisibility.Column,
                SelectionMode = DataGridSelectionMode.Single,
                GridLinesVisibility = DataGridGridLinesVisibility.None,
            };
            _tableGrid.Columns.Add(new DataGridCheckBoxColumn { Header = "Deploy", Binding = new Binding("Selected") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged }, Width = 60 });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Table", Binding = new Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Different", Binding = new Binding("Different"), IsReadOnly = true, Width = 90 });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = _kindNames[2], Binding = new Binding("OnlyInSource"), IsReadOnly = true, Width = 120 });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = _kindNames[3], Binding = new Binding("OnlyInTarget"), IsReadOnly = true, Width = 120 });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Identical", Binding = new Binding("Identical"), IsReadOnly = true, Width = 90 });
            _tableGrid.Columns.Add(new DataGridTextColumn { Header = "Status", Binding = new Binding("Status"), IsReadOnly = true, Width = 200 });
            _tableGrid.SelectionChanged += async (s, e) => { Array.Clear(_page, 0, 4); await ShowRows(); };
            for (var k = 1; k <= 4; k++)
            {
                var kind = k % 4; // Different, only source, only target, identical
                _kinds.Items.Add(new TabItem { Header = _kindNames[kind], Tag = kind });
            }
            _kinds.SelectionChanged += async (s, e) => { if (e.Source == _kinds) await ShowRows(); };

            var summary = Ui.Text(
                Ui.Plural(_tables.Count, "table", "tables") + " compared"
                    + (result.Strings("onlyIn1").Count > 0 ? "; only in " + result.Str("sourceName") + ": " + string.Join(", ", result.Strings("onlyIn1")) : "")
                    + (result.Strings("onlyIn2").Count > 0 ? "; only in " + result.Str("targetName") + ": " + string.Join(", ", result.Strings("onlyIn2")) : ""),
                brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            summary.Margin = new Thickness(8, 4, 8, 4);
            var split = new Grid();
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            split.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.4, GridUnitType.Star) });
            var splitter = new GridSplitter { Height = 5, HorizontalAlignment = HorizontalAlignment.Stretch, Background = CompareTheme.Border };
            Grid.SetRow(splitter, 1);
            Grid.SetRow(_kinds, 2);
            split.Children.Add(_tableGrid);
            split.Children.Add(splitter);
            split.Children.Add(_kinds);
            SetDock(summary, Dock.Top);
            Children.Add(summary);
            Children.Add(split);
        }

        public List<object> SelectedTables => _tables.Where(t => t.Selected).Select(t => (object)t.Index).ToList();

        private async System.Threading.Tasks.Task ShowRows()
        {
            if (!(_tableGrid.SelectedItem is TableRow table) || !(_kinds.SelectedItem is TabItem tab)) return;
            var kind = (int)tab.Tag;
            var start = _page[kind] * PageSize;
            var reply = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("data_rows", new JsonObject
            {
                ["session"] = Session, ["table"] = table.Index, ["kind"] = kind, ["start"] = start, ["count"] = PageSize,
            }));
            if (reply == null || !ReferenceEquals(_tableGrid.SelectedItem, table)) return;
            var total = reply.Long("total");
            var grid = new DataGrid { AutoGenerateColumns = false, IsReadOnly = true, HeadersVisibility = DataGridHeadersVisibility.Column, CanUserSortColumns = false };
            var rows = new List<Dictionary<string, string>>();
            var differing = new List<HashSet<int>>();
            var paired = kind == 1 || kind == 0;
            for (var c = 0; c < table.Columns.Count; c++)
            {
                var name = table.Columns[c];
                if (paired && !table.Key.Contains(c))
                {
                    grid.Columns.Add(Column(name + " (source)", "s" + c, c));
                    grid.Columns.Add(Column(name + " (target)", "t" + c, c));
                }
                else
                {
                    grid.Columns.Add(Column(name, (kind == 3 ? "t" : "s") + c, c));
                }
            }
            foreach (var r in reply.List("rows").OfType<JsonObject>())
            {
                var src = r.Strings("source");
                var tgt = r.Strings("target");
                var row = new Dictionary<string, string>();
                for (var c = 0; c < table.Columns.Count; c++)
                {
                    row["s" + c] = c < src.Count ? src[c] : "";
                    row["t" + c] = c < tgt.Count ? tgt[c] : "";
                }
                var diffs = r.List("differs").Select((d, i) => (d is bool b && b) ? i : -1).Where(i => i >= 0);
                row["__differs"] = string.Join(",", diffs);
                rows.Add(row);
            }
            grid.ItemsSource = rows;
            var pager = Ui.Row(
                Ui.Button("◀ Previous", async () => { if (_page[kind] > 0) { _page[kind]--; await ShowRows(); } }),
                Ui.Button("Next ▶", async () => { if ((_page[kind] + 1) * PageSize < total) { _page[kind]++; await ShowRows(); } }),
                Ui.Text("  Rows " + (total == 0 ? 0 : start + 1) + "–" + Math.Min(start + PageSize, total) + " of " + total
                    + (kind == 0 && total == 0 && table.Identical > 0 ? " (identical rows are counted but not kept; turn on 'Show identical values in results')" : "")));
            var panel = new DockPanel();
            DockPanel.SetDock(pager, Dock.Bottom);
            panel.Children.Add(pager);
            panel.Children.Add(grid);
            tab.Content = panel;
        }

        private static DataGridTextColumn Column(string header, string key, int index)
        {
            var col = new DataGridTextColumn { Header = header, Binding = new Binding("[" + key + "]"), MaxWidth = 400 };
            // Cells that differ are highlighted.
            var style = new Style(typeof(DataGridCell));
            var trigger = new DataTrigger
            {
                Binding = new Binding("[__differs]") { Converter = new ContainsConverter(index) },
                Value = true,
            };
            trigger.Setters.Add(new Setter(Control.BackgroundProperty, CompareTheme.Changed));
            style.Triggers.Add(trigger);
            col.CellStyle = style;
            return col;
        }

        private sealed class ContainsConverter : IValueConverter
        {
            private readonly string _index;
            public ContainsConverter(int index) => _index = index.ToString(System.Globalization.CultureInfo.InvariantCulture);

            public object Convert(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture)
                => value is string s && s.Split(',').Contains(_index);

            public object ConvertBack(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture) => Binding.DoNothing;
        }
    }
}
