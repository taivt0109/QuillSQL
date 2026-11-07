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
            private string _group;
            public int Index { get; set; }
            public string Type { get; set; }
            public string Name { get; set; }
            public string Status { get; set; }
            /// <summary>Different, only in source, only in target, identical: the order of the groups.</summary>
            public int Rank { get; set; }
            public string Marker { get; set; }
            public Brush MarkerFill { get; set; }
            public Brush MarkerInk { get; set; }
            public string Kind { get; set; }
            public string Tip { get; set; }
            public JsonObject Data { get; set; }

            public bool Selected
            {
                get => _selected;
                set { _selected = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Selected))); }
            }

            /// <summary>The group the row is shown in, for the current Group by choice.</summary>
            public string Group
            {
                get => _group;
                set { _group = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Group))); }
            }

            public event PropertyChangedEventHandler PropertyChanged;
        }

        public enum Grouping { Difference, ObjectType, None }

        private readonly CompareSession _session;
        private readonly ObservableCollection<Row> _rows = new ObservableCollection<Row>();
        private readonly ListCollectionView _view;
        private readonly DataGrid _grid;
        private readonly RichTextBox _left = SideBox(), _right = SideBox();
        private readonly TextBlock _leftTitle, _rightTitle, _summary, _diffCount;
        private readonly TreeView _properties = new TreeView();
        private readonly CheckBox _showIdentical;
        private readonly TextBox _find = Ui.SearchBox("Group objects as you type", 240);
        private readonly ComboBox _groupBy = new ComboBox { Width = 170, VerticalAlignment = VerticalAlignment.Center };
        private readonly HashSet<int> _filtered = new HashSet<int>();
        private readonly Button _filterButton;
        private readonly FilterPane _filterPane;
        private readonly string _sourceName, _targetName;
        private readonly List<int> _diffLines = new List<int>();
        private int _diffAt = -1;

        public long Session { get; }

        /// <summary>The comparison's suggested table renames; raised when the user asks to review them.</summary>
        public event Action<List<JsonObject>> ReviewRenames;

        public SchemaResults(CompareSession session, JsonObject result)
        {
            _session = session;
            Session = result.Long("session");
            _sourceName = result.Str("sourceName");
            _targetName = result.Str("targetName");
            foreach (var d in result.List("differences").OfType<JsonObject>())
            {
                var kind = d.Str("kind");
                var row = new Row
                {
                    Index = (int)d.Long("index"),
                    Type = d.Str("type"),
                    Name = (string.IsNullOrEmpty(d.Str("schema")) ? "" : "[" + d.Str("schema") + "].") + "[" + d.Str("name") + "]" + (d.Str("targetName") != null ? " → " + d.Str("targetName") : ""),
                    Kind = kind,
                    Rank = kind == "Different" ? 0 : kind == "OnlyIn1" ? 1 : kind == "OnlyIn2" ? 2 : 3,
                    // What deployment does to the target: create, change or drop.
                    Marker = kind == "Equal" ? "=" : kind == "Different" ? "~" : kind == "OnlyIn1" ? "+" : "−",
                    MarkerFill = kind == "Equal" ? Brushes.Transparent : kind == "Different" ? CompareTheme.Changed : kind == "OnlyIn1" ? CompareTheme.Added : CompareTheme.Removed,
                    MarkerInk = kind == "Equal" ? CompareTheme.Muted : kind == "Different" ? CompareTheme.Medium : kind == "OnlyIn1" ? CompareTheme.Success : CompareTheme.High,
                    Status = kind == "Equal" ? "identical" : kind == "Different" ? "in both, but different" : kind == "OnlyIn1" ? "only in " + _sourceName : "only in " + _targetName,
                    Selected = d.Bool("selected"),
                    Data = d,
                };
                var action = kind == "OnlyIn1" ? "created in" : kind == "OnlyIn2" ? "dropped from" : "modified in";
                row.Tip = kind == "Equal" ? row.Name + " is identical in both databases."
                    : row.Name + ": this object will be " + action + " the target database" + (d.Bool("trivial") ? " (extended properties only)" : "") + ".";
                _rows.Add(row);
            }
            _view = (ListCollectionView)CollectionViewSource.GetDefaultView(_rows);
            _view.SortDescriptions.Add(new SortDescription("Rank", ListSortDirection.Ascending));
            _view.SortDescriptions.Add(new SortDescription("Type", ListSortDirection.Ascending));
            _view.SortDescriptions.Add(new SortDescription("Name", ListSortDirection.Ascending));
            _showIdentical = Ui.Check("Show identical objects", false, _ => Refresh());
            _view.Filter = o =>
            {
                var r = (Row)o;
                if (_filtered.Contains(r.Index)) return false;
                return r.Kind != "Equal" || _showIdentical.IsChecked == true;
            };
            _find.TextChanged += (s, e) => { _groupBy.IsEnabled = _find.Text.Length == 0; Regroup(); };

            _grid = new DataGrid
            {
                ItemsSource = _view,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
                IsReadOnly = false,
                SelectionMode = DataGridSelectionMode.Extended,
            };
            var deployColumn = Ui.CheckColumn("Deploy", "Selected", 64);
            deployColumn.Header = HeaderCheck();
            _grid.Columns.Add(deployColumn);
            _grid.Columns.Add(new DataGridTemplateColumn { Header = "", Width = 40, CellTemplate = MarkerTemplate() });
            _grid.Columns.Add(new DataGridTextColumn { Header = "Object", Binding = new Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            _grid.Columns.Add(new DataGridTextColumn { Header = "Type", Binding = new Binding("Type"), IsReadOnly = true, Width = 150 });
            var rowStyle = new Style(typeof(DataGridRow), CompareTheme.Style(typeof(DataGridRow)));
            rowStyle.Setters.Add(new Setter(ToolTipProperty, new Binding("Tip")));
            _grid.RowStyle = rowStyle;
            _grid.GroupStyle.Add(GroupHeader());
            _grid.SelectionChanged += async (s, e) => await ShowSelected();
            _grid.ContextMenu = RowMenu();

            var counts = _rows.GroupBy(r => r.Kind).ToDictionary(g => g.Key, g => g.Count());
            int n(string k) => counts.TryGetValue(k, out var c) ? c : 0;
            var badges = Ui.Row(
                Ui.Badge(n("Different") + " different", CompareTheme.Changed, CompareTheme.Medium),
                Ui.Badge(n("OnlyIn1") + " only in " + _sourceName, CompareTheme.Added, CompareTheme.Success),
                Ui.Badge(n("OnlyIn2") + " only in " + _targetName, CompareTheme.Removed, CompareTheme.High),
                Ui.Badge(n("Equal") + " identical", (Brush)CompareTheme.Resources["Q.Subtle"], CompareTheme.Muted));
            _summary = Ui.Text("", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            var autoMapped = result.List("autoMapped").OfType<JsonObject>().ToList();
            if (autoMapped.Count > 0)
            {
                _summary.Text = "Columns paired by similarity: " + string.Join(", ", autoMapped.Select(a => a.Str("table") + ".[" + a.Str("source") + "] → [" + a.Str("target") + "]"));
                _summary.Margin = new Thickness(0, 6, 0, 0);
            }
            foreach (var label in new[] { "Type of difference", "Type of object", "No groups" }) _groupBy.Items.Add(label);
            _groupBy.SelectedIndex = 0;
            _groupBy.ToolTip = "Change how the database objects are presented";
            _groupBy.SelectionChanged += (s, e) => Regroup();
            _filterButton = Ui.Command(Glyph.Filter, "0 objects filtered", ToggleFilterPane, "Apply a custom filter to the results");
            var groupLabel = Ui.Text("Group by", brush: CompareTheme.Muted);
            groupLabel.Margin = new Thickness(12, 0, 8, 0);
            var tools = Ui.Row(_showIdentical, _filterButton, groupLabel, _groupBy, new Border { Width = 8 }, _find);
            var toolbar = new DockPanel();
            DockPanel.SetDock(tools, Dock.Right);
            toolbar.Children.Add(tools);
            toolbar.Children.Add(badges);
            var top = Ui.Column(toolbar, _summary);
            var renames = result.List("renames").OfType<JsonObject>().ToList();
            if (renames.Count > 0)
            {
                var text = Ui.Text(Ui.Plural(renames.Count, "table looks", "tables look") + " renamed: " + string.Join(", ", renames.Take(3).Select(r => QName(r.Obj("source")) + " → " + QName(r.Obj("target")))) + (renames.Count > 3 ? ", …" : "")
                    + ". Map them to compare them as one table.", wrap: TextWrapping.Wrap);
                var review = Ui.Button("Review renames…", () => ReviewRenames?.Invoke(renames), minWidth: 120);
                var bar = new DockPanel();
                var icon = Ui.Icon(Glyph.Info, 15, CompareTheme.Accent);
                icon.Margin = new Thickness(0, 0, 10, 0);
                DockPanel.SetDock(icon, Dock.Left);
                DockPanel.SetDock(review, Dock.Right);
                bar.Children.Add(icon);
                bar.Children.Add(review);
                bar.Children.Add(text);
                var card = Ui.Card(bar, 10);
                card.Margin = new Thickness(0, 8, 0, 0);
                top.Children.Add(card);
            }
            // Problems reading scripts folders: statements the parser skipped or could not read.
            var warnings = result.Strings("warnings");
            if (warnings.Count > 0)
            {
                var text = Ui.Text(Ui.Plural(warnings.Count, "warning", "warnings") + " while reading the data sources: " + warnings[0] + (warnings.Count > 1 ? " …" : ""), wrap: TextWrapping.Wrap);
                var show = Ui.Button("Show warnings…", () => ShowWarnings(warnings), minWidth: 120);
                var bar = new DockPanel();
                var icon = Ui.Icon(Glyph.Warning, 15, CompareTheme.Medium);
                icon.Margin = new Thickness(0, 0, 10, 0);
                DockPanel.SetDock(icon, Dock.Left);
                DockPanel.SetDock(show, Dock.Right);
                bar.Children.Add(icon);
                bar.Children.Add(show);
                bar.Children.Add(text);
                var card = Ui.Card(bar, 10);
                card.Margin = new Thickness(0, 8, 0, 0);
                top.Children.Add(card);
            }
            top.Margin = new Thickness(28, 6, 28, 10);

            _leftTitle = Ui.Text("Source: " + _sourceName, bold: true);
            _rightTitle = Ui.Text("Target: " + _targetName, bold: true);
            var sides = new Grid();
            sides.ColumnDefinitions.Add(new ColumnDefinition());
            sides.ColumnDefinitions.Add(new ColumnDefinition());
            sides.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            sides.RowDefinitions.Add(new RowDefinition());
            _diffCount = Ui.Text("", brush: CompareTheme.Muted);
            var nav = Ui.Row(
                Ui.IconButton(Glyph.Up, "Go to the previous line containing a difference", () => GoToDifference(-1)),
                Ui.IconButton(Glyph.Down, "Go to the next line containing a difference", () => GoToDifference(1)),
                _diffCount);
            nav.Margin = new Thickness(0, 2, 8, 0);
            nav.HorizontalAlignment = HorizontalAlignment.Right;
            var leftHead = new DockPanel();
            leftHead.Children.Add(_leftTitle);
            var rightHead = new DockPanel();
            DockPanel.SetDock(nav, Dock.Right);
            rightHead.Children.Add(nav);
            rightHead.Children.Add(_rightTitle);
            Grid.SetColumn(rightHead, 1);
            Grid.SetRow(_left, 1);
            Grid.SetRow(_right, 1);
            Grid.SetColumn(_right, 1);
            _leftTitle.Margin = _rightTitle.Margin = new Thickness(16, 10, 16, 4);
            sides.Children.Add(leftHead);
            sides.Children.Add(rightHead);
            sides.Children.Add(_left);
            sides.Children.Add(_right);
            SyncScroll(_left, _right);

            var bottomTabs = new TabControl();
            bottomTabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Code, "SQL side by side"), Content = sides });
            bottomTabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Property differences"), Content = _properties });

            var split = new Grid { Margin = new Thickness(16, 0, 16, 16) };
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            split.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.2, GridUnitType.Star) });
            var splitter = new GridSplitter { Height = 12, HorizontalAlignment = HorizontalAlignment.Stretch, ResizeBehavior = GridResizeBehavior.PreviousAndNext };
            // Shown instead of an empty list.
            var emptyIcon = Ui.Icon(Glyph.Check, 28, CompareTheme.Success);
            emptyIcon.HorizontalAlignment = HorizontalAlignment.Center;
            var emptyTitle = Ui.Title("The schemas are identical", 16);
            emptyTitle.HorizontalAlignment = HorizontalAlignment.Center;
            emptyTitle.Margin = new Thickness(0, 10, 0, 4);
            _emptyText = Ui.Text("", brush: CompareTheme.Muted);
            _emptyText.HorizontalAlignment = HorizontalAlignment.Center;
            _empty = Ui.Column(emptyIcon, emptyTitle, _emptyText);
            _empty.VerticalAlignment = VerticalAlignment.Center;
            var listArea = new Grid();
            listArea.Children.Add(_grid);
            listArea.Children.Add(_empty);
            foreach (var i in result.List("filtered")) _filtered.Add((int)(double)i);
            _filterPane = new FilterPane(session, _rows.Select(r => r.Type).Distinct().OrderBy(t => t)) { Visibility = _filtered.Count > 0 ? Visibility.Visible : Visibility.Collapsed };
            _filterPane.Changed += async f => await ApplyFilter(f);
            var projectFilter = session.Project.Str("filter");
            if (projectFilter != null)
            {
                Loaded += async (s, e) =>
                {
                    var model = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("filter_load", new JsonObject { ["xml"] = projectFilter }));
                    if (model != null) _filterPane.SetFilter(model.Obj("filter"));
                };
            }
            var withFilter = new DockPanel();
            DockPanel.SetDock(_filterPane, Dock.Left);
            withFilter.Children.Add(_filterPane);
            withFilter.Children.Add(listArea);
            var gridCard = Ui.Card(withFilter, 4);
            _identical = n("Equal");
            Regroup();
            var bottomCard = Ui.Card(bottomTabs, 0);
            Grid.SetRow(splitter, 1);
            Grid.SetRow(bottomCard, 2);
            split.Children.Add(gridCard);
            split.Children.Add(splitter);
            split.Children.Add(bottomCard);

            SetDock(top, Dock.Top);
            Children.Add(top);
            Children.Add(split);
        }

        private readonly StackPanel _empty;
        private readonly TextBlock _emptyText;
        private readonly int _identical;

        /// <summary>Include or exclude the selected objects, or show the script of one.</summary>
        private ContextMenu RowMenu()
        {
            var menu = new ContextMenu();
            CompareTheme.Attach(menu);
            void tick(bool on)
            {
                foreach (var r in _grid.SelectedItems.OfType<Row>().Where(r => r.Kind != "Equal")) r.Selected = on;
                UpdateHeaderCheck();
            }
            menu.Items.Add(Ui.MenuItem("Include selected objects", () => tick(true), Glyph.Check));
            menu.Items.Add(Ui.MenuItem("Exclude selected objects", () => tick(false)));
            menu.Items.Add(new Separator());
            menu.Items.Add(Ui.MenuItem("View object deployment script", async () => await ObjectScript(), Glyph.Code));
            Dialogs.RegisterMenu(menu);
            return menu;
        }

        /// <summary>The deployment script of the selected object on its own (with its dependencies, as the options say).</summary>
        private async System.Threading.Tasks.Task ObjectScript()
        {
            if (!(_grid.SelectedItem is Row row) || row.Kind == "Equal") return;
            var script = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("schema_script", new JsonObject
            {
                ["session"] = Session,
                ["selected"] = new List<object> { row.Index },
                ["project"] = _session.Project,
                ["isolationLevel"] = CompareSettings.Load().IsolationLevel,
            }));
            if (script != null) Dialogs.ShowScript(_session.Host, "Deployment script for " + row.Name, script.Str("sql"));
        }

        /// <summary>The parser warnings, one per line, to read and copy.</summary>
        private void ShowWarnings(List<string> warnings)
        {
            var w = Ui.Dialog(_session.Host.Owner, "Parser warnings", null, 820, 520);
            var code = Ui.Code();
            code.Text = string.Join(Environment.NewLine, warnings);
            var heading = Ui.Title("Parser warnings", 18);
            var lead = Ui.Text("The scripts folders were read with these warnings; the objects concerned may be compared incompletely.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            lead.Margin = new Thickness(0, 4, 0, 12);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            var head = Ui.Column(heading, lead);
            DockPanel.SetDock(head, Dock.Top);
            body.Children.Add(head);
            body.Children.Add(Ui.Card(code, 0));
            var copy = Ui.Button("Copy", () => Clipboard.SetText(code.Text), glyph: Glyph.Copy);
            var close = Ui.Button("Close", () => w.Close(), primary: true);
            close.IsCancel = true;
            var buttons = Ui.Row(copy, close);
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            w.Content = Ui.WithFooter(body, buttons);
            w.ShowDialog();
        }

        private static string QName(JsonObject n) => n == null ? "" : "[" + n.Str("schema") + "].[" + n.Str("name") + "]";

        /// <summary>Groups by the Group by choice, or, while the find box has text, by whether objects match it.</summary>
        private void Regroup()
        {
            var find = _find.Text.Trim();
            var mode = (Grouping)Math.Max(0, _groupBy.SelectedIndex);
            foreach (var r in _rows)
            {
                if (find.Length > 0)
                {
                    var match = (r.Name + " " + r.Data.Str("schema")).IndexOf(find, StringComparison.OrdinalIgnoreCase) >= 0;
                    r.Group = (match ? "0" : "1") + find;
                }
                else
                {
                    r.Group = mode == Grouping.Difference ? r.Rank + r.Status : mode == Grouping.ObjectType ? r.Type : null;
                }
            }
            using (_view.DeferRefresh())
            {
                _view.GroupDescriptions.Clear();
                _view.SortDescriptions.Clear();
                if (find.Length > 0 || mode != Grouping.None)
                {
                    _view.GroupDescriptions.Add(new PropertyGroupDescription("Group"));
                    _view.SortDescriptions.Add(new SortDescription("Group", ListSortDirection.Ascending));
                }
                _view.SortDescriptions.Add(new SortDescription("Rank", ListSortDirection.Ascending));
                _view.SortDescriptions.Add(new SortDescription("Type", ListSortDirection.Ascending));
                _view.SortDescriptions.Add(new SortDescription("Name", ListSortDirection.Ascending));
            }
            Refresh();
        }

        private void Refresh()
        {
            _view.Refresh();
            var differences = _rows.Count(r => r.Kind != "Equal");
            var empty = _view.IsEmpty;
            _grid.Visibility = empty ? Visibility.Hidden : Visibility.Visible;
            _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
            ((TextBlock)_empty.Children[1]).Text = differences == 0 ? "The schemas are identical" : "No objects are shown";
            _emptyText.Text = differences == 0 ? Ui.Plural(_identical, "object compared", "objects compared") + "; nothing to deploy." : "Show identical objects or include more object types in the filter.";
            var filtered = _rows.Count(r => _filtered.Contains(r.Index) && (r.Kind != "Equal" || _showIdentical.IsChecked == true));
            _filterPane.ShowExcluded(_rows.Count(r => _filtered.Contains(r.Index)));
            ((TextBlock)((StackPanel)_filterButton.Content).Children[1]).Text = Ui.Plural(filtered, "object filtered", "objects filtered");
            UpdateHeaderCheck();
        }

        // ---- Group headers: a bold count, then what the group holds ----

        private GroupStyle GroupHeader()
        {
            var groupStyle = new GroupStyle();
            var header = new FrameworkElementFactory(typeof(StackPanel));
            header.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);
            header.SetValue(FrameworkElement.MarginProperty, new Thickness(10, 14, 0, 6));
            var groupCount = new FrameworkElementFactory(typeof(TextBlock));
            groupCount.SetBinding(TextBlock.TextProperty, new Binding("ItemCount"));
            groupCount.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            var groupName = new FrameworkElementFactory(typeof(TextBlock));
            groupName.SetBinding(TextBlock.TextProperty, new Binding { Converter = new GroupLabel(this) });
            groupName.SetValue(TextBlock.ForegroundProperty, CompareTheme.Muted);
            groupName.SetValue(FrameworkElement.MarginProperty, new Thickness(6, 0, 0, 0));
            header.AppendChild(groupCount);
            header.AppendChild(groupName);
            groupStyle.HeaderTemplate = new DataTemplate { VisualTree = header };
            return groupStyle;
        }

        private sealed class GroupLabel : IValueConverter
        {
            private readonly SchemaResults _owner;
            public GroupLabel(SchemaResults owner) => _owner = owner;

            public object Convert(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture)
            {
                if (!(value is CollectionViewGroup g)) return "";
                var key = g.Name as string ?? "";
                var one = g.ItemCount == 1;
                if (_owner._find.Text.Trim().Length > 0)
                {
                    var term = "'" + key.Substring(1) + "'";
                    return key.StartsWith("0", StringComparison.Ordinal) ? (one ? "object matches " : "objects match ") + term : (one ? "object does not match " : "objects do not match ") + term;
                }
                if ((Grouping)_owner._groupBy.SelectedIndex == Grouping.Difference) return key.Substring(1);
                var type = key.ToLowerInvariant();
                return one ? type : type.EndsWith("y", StringComparison.Ordinal) ? type.Substring(0, type.Length - 1) + "ies" : type + "s";
            }

            public object ConvertBack(object value, Type targetType, object parameter, System.Globalization.CultureInfo culture) => Binding.DoNothing;
        }

        // ---- Deploy check boxes ----

        private CheckBox _headerCheck;

        /// <summary>The Deploy column header: ticked when every shown difference is, mixed when some are.</summary>
        private CheckBox HeaderCheck()
        {
            _headerCheck = new CheckBox { ToolTip = "Include in deployment", IsThreeState = false, HorizontalAlignment = HorizontalAlignment.Center };
            _headerCheck.Click += (s, e) =>
            {
                var shown = Shown().Where(r => r.Kind != "Equal").ToList();
                var all = shown.Count > 0 && shown.All(r => r.Selected);
                foreach (var r in shown) r.Selected = !all;
                UpdateHeaderCheck();
            };
            return _headerCheck;
        }

        private void UpdateHeaderCheck()
        {
            if (_headerCheck == null) return;
            var shown = Shown().Where(r => r.Kind != "Equal").ToList();
            _headerCheck.IsEnabled = shown.Count > 0;
            _headerCheck.IsChecked = shown.Count > 0 && shown.All(r => r.Selected) ? true : shown.Any(r => r.Selected) ? (bool?)null : false;
        }

        private IEnumerable<Row> Shown() => _rows.Where(r => !_filtered.Contains(r.Index));

        public void SelectAll(bool on)
        {
            foreach (var r in Shown().Where(r => r.Kind != "Equal")) r.Selected = on;
            UpdateHeaderCheck();
        }

        /// <summary>Indexes of the differences ticked for deployment; objects the filter hides are not deployed.</summary>
        public List<object> Selected => Shown().Where(r => r.Selected && r.Kind != "Equal").Select(r => (object)r.Index).ToList();

        public int DifferenceCount => _rows.Count(r => r.Kind != "Equal");

        public JsonObject Difference(int index) => _rows.FirstOrDefault(r => r.Index == index)?.Data;

        /// <summary>Objects unticked, for the project file.</summary>
        public List<object> Deselected => _rows.Where(r => !r.Selected && r.Kind != "Equal")
            .Select(r => (object)new JsonObject { ["type"] = r.Type, ["schema"] = r.Data.Str("schema"), ["name"] = r.Data.Str("name") }).ToList();

        // ---- Filter ----

        /// <summary>Applies the pane's filter in the engine; what it leaves out is hidden and not deployed.</summary>
        private async System.Threading.Tasks.Task ApplyFilter(JsonObject filter)
        {
            var reply = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("schema_filter", new JsonObject { ["session"] = Session, ["filter"] = filter }));
            if (reply == null) return;
            _filtered.Clear();
            foreach (var i in reply.List("filtered")) _filtered.Add((int)(double)i);
            if (reply.Str("xml") != null) _session.Project["filter"] = reply.Str("xml");
            else _session.Project.Remove("filter");
            FilterChanged?.Invoke();
            Refresh();
        }

        /// <summary>Raised when the filter changes (the project changed).</summary>
        public event Action FilterChanged;

        private void ToggleFilterPane() => _filterPane.Visibility = _filterPane.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
        // ---- SQL side by side ----

        /// <summary>A rounded square with +, −, ~ or =, colored by what deployment does.</summary>
        private static DataTemplate MarkerTemplate()
        {
            var box = new FrameworkElementFactory(typeof(Border));
            box.SetValue(FrameworkElement.WidthProperty, 20.0);
            box.SetValue(FrameworkElement.HeightProperty, 20.0);
            box.SetValue(Border.CornerRadiusProperty, new CornerRadius(4));
            box.SetBinding(Border.BackgroundProperty, new Binding("MarkerFill"));
            var mark = new FrameworkElementFactory(typeof(TextBlock));
            mark.SetBinding(TextBlock.TextProperty, new Binding("Marker"));
            mark.SetBinding(TextBlock.ForegroundProperty, new Binding("MarkerInk"));
            mark.SetValue(TextBlock.FontWeightProperty, FontWeights.Bold);
            mark.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            mark.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            box.AppendChild(mark);
            return new DataTemplate { VisualTree = box };
        }

        private const double LineHeight = 18;

        private static RichTextBox SideBox()
        {
            var box = new RichTextBox
            {
                IsReadOnly = true,
                FontFamily = CompareTheme.Code,
                FontSize = 12.5,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
            box.Document = new FlowDocument(new Paragraph(new Run("Select an object to see its SQL.") { Foreground = CompareTheme.Muted, FontFamily = CompareTheme.Font })) { PagePadding = new Thickness(12, 8, 12, 8) };
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
            // The first line of each run of differing lines, for the up and down buttons.
            _diffLines.Clear();
            for (var i = 0; i < lines.Count; i++)
            {
                if (lines[i].Kind != LineDiff.Kind.Same && (i == 0 || lines[i - 1].Kind == LineDiff.Kind.Same)) _diffLines.Add(i);
            }
            _diffAt = -1;
            _diffCount.Text = row.Kind == "Different" ? Ui.Plural(_diffLines.Count, "difference", "differences") : "";
            if (row.Kind == "Different" && _diffLines.Count > 0) GoToDifference(1);
        }

        /// <summary>Scrolls both sides to the next (1) or previous (-1) run of differing lines.</summary>
        private void GoToDifference(int step)
        {
            if (_diffLines.Count == 0) return;
            var next = _diffAt + step;
            if (next < 0 || next >= _diffLines.Count) return;
            _diffAt = next;
            _left.ScrollToVerticalOffset(Math.Max(0, _diffLines[next] * LineHeight - 3 * LineHeight));
            _diffCount.Text = (next + 1) + " of " + _diffLines.Count;
        }

        private static void Fill(RichTextBox box, List<LineDiff.Line> lines, bool left)
        {
            var doc = new FlowDocument { PageWidth = 4000, FontFamily = CompareTheme.Code, FontSize = 12.5, PagePadding = new Thickness(12, 4, 12, 4) };
            var p = new Paragraph { Margin = new Thickness(0), LineHeight = LineHeight, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
            var missing = (Brush)CompareTheme.Resources["Q.Subtle"];
            foreach (var line in lines)
            {
                var text = left ? line.Left : line.Right;
                switch (line.Kind)
                {
                    case LineDiff.Kind.Changed:
                        // Changed lines are tinted, and what changed inside them is marked more strongly.
                        InlineDiff.Compare(line.Left, line.Right, out var a, out var b);
                        foreach (var piece in left ? a : b)
                        {
                            p.Inlines.Add(new Run(piece.Text) { Background = piece.Different ? CompareTheme.ChangedStrong : CompareTheme.Changed });
                        }
                        p.Inlines.Add(new Run("\n") { Background = CompareTheme.Changed });
                        continue;
                    case LineDiff.Kind.OnlyLeft:
                        p.Inlines.Add(new Run((text ?? "") + "\n") { Background = left ? CompareTheme.Added : missing });
                        continue;
                    case LineDiff.Kind.OnlyRight:
                        p.Inlines.Add(new Run((text ?? "") + "\n") { Background = left ? missing : CompareTheme.Removed });
                        continue;
                    default:
                        p.Inlines.Add(new Run((text ?? "") + "\n"));
                        continue;
                }
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
}
