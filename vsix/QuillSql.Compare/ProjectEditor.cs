using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>
    /// The project's settings: Data sources, Table mapping (schema) or Tables &amp; views (data),
    /// Owner mapping and Options. Edits go straight into the project object.
    /// </summary>
    internal sealed class ProjectEditor : DockPanel
    {
        private readonly CompareSession _session;
        private readonly JsonObject _project;
        private readonly bool _data;
        private SourceEditor _source, _target;
        private readonly ObservableCollection<TableMappingRow> _tableMappings = new ObservableCollection<TableMappingRow>();
        private readonly ObservableCollection<OwnerMappingRow> _ownerMappings = new ObservableCollection<OwnerMappingRow>();
        private readonly ObservableCollection<DataTableRow> _dataTables = new ObservableCollection<DataTableRow>();
        private readonly Dictionary<string, CheckBox> _optionBoxes = new Dictionary<string, CheckBox>();
        /// <summary>Columns of each target table, from Load tables.</summary>
        private readonly Dictionary<string, List<string>> _targetColumns = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
        private TextBlock _optionHelp, _dataStatus;

        public ProjectEditor(CompareSession session, Action compareNow, Action cancel)
        {
            _session = session;
            _project = session.Project;
            _data = session.IsData;

            var buttons = Ui.Row(
                Ui.Button("Compare now", () => { Collect(); compareNow(); }, primary: true, minWidth: 130, glyph: Glyph.Play),
                Ui.Button("Cancel", cancel, minWidth: 100));
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            buttons.Margin = new Thickness(0);
            var footer = new Border { Child = buttons, Padding = new Thickness(24, 12, 24, 12), BorderThickness = new Thickness(0, 1, 0, 0) };
            footer.SetResourceReference(Border.BackgroundProperty, "Q.Layer");
            footer.SetResourceReference(Border.BorderBrushProperty, "Q.Divider");
            SetDock(footer, Dock.Bottom);
            Children.Add(footer);

            var tabs = _tabs = new TabControl { Style = CompareTheme.Style("Q.NavTabs"), Margin = new Thickness(4, 0, 0, 0) };
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Connect, "Data sources", 12), Content = DataSources() });
            if (_data)
            {
                tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Tables & views", 12), Content = TablesAndViews() });
            }
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Table mapping", 12), Content = TableMapping() });
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.People, "Owner mapping", 12), Content = OwnerMapping() });
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Settings, "Options", 12), Content = Options() });
            Children.Add(tabs);
        }

        private readonly TabControl _tabs;
        private DataGrid _dataGrid;
        private CheckBox _includeNew;
        private bool _tablesLoaded;

        /// <summary>Shows a tab by its label: "Data sources", "Tables & views", "Table mapping", "Owner mapping", "Options".</summary>
        public void ShowTab(string label)
        {
            foreach (TabItem tab in _tabs.Items)
            {
                if (tab.Header is StackPanel p && p.Children.OfType<TextBlock>().Any(t => t.Text == label)) _tabs.SelectedItem = tab;
            }
        }

        /// <summary>Selects a table on Tables &amp; views (data), to set its key or filters.</summary>
        public void ShowTable(string source)
        {
            ShowTab("Tables & views");
            var row = _dataTables.FirstOrDefault(r => string.Equals(r.Source, source, StringComparison.OrdinalIgnoreCase));
            if (row == null && !string.IsNullOrEmpty(source)) _dataTables.Add(row = new DataTableRow { Source = source, Target = source });
            if (row == null || _dataGrid == null) return;
            _dataGrid.SelectedItem = row;
            _dataGrid.ScrollIntoView(row);
        }

        /// <summary>A section page: a title, a line of help, then the content.</summary>
        private static DockPanel Page(string title, string help, UIElement content, UIElement actions = null)
        {
            var head = new DockPanel { Margin = new Thickness(0, 0, 0, 14) };
            if (actions != null)
            {
                DockPanel.SetDock(actions, Dock.Right);
                head.Children.Add(actions);
            }
            var text = Ui.Column(Ui.Title(title, 18));
            if (help != null)
            {
                var h = Ui.Text(help, wrap: TextWrapping.Wrap, brush: CompareTheme.Muted);
                h.Margin = new Thickness(0, 4, 0, 0);
                text.Children.Add(h);
            }
            head.Children.Add(text);
            var page = new DockPanel { Margin = new Thickness(24, 20, 24, 20) };
            DockPanel.SetDock(head, Dock.Top);
            page.Children.Add(head);
            page.Children.Add(content);
            return page;
        }

        // ---- Data sources ----

        private UIElement DataSources()
        {
            _source = new SourceEditor(_session, "source", "Source");
            _target = new SourceEditor(_session, "target", "Target");
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var middle = Ui.Column(
                SmallButton(Glyph.Forward, "Copy the connection settings to the target", () => Copy(_source, _target)),
                SmallButton(Glyph.Switch, "Switch source and target", Switch),
                SmallButton(Glyph.Back, "Copy the connection settings to the source", () => Copy(_target, _source)));
            middle.VerticalAlignment = VerticalAlignment.Center;
            middle.Margin = new Thickness(8, 0, 8, 0);
            var source = Ui.Card(_source, 20);
            var target = Ui.Card(_target, 20);
            Grid.SetColumn(middle, 1);
            Grid.SetColumn(target, 2);
            grid.Children.Add(source);
            grid.Children.Add(middle);
            grid.Children.Add(target);
            var page = Page("Data sources", "The source is what the target should become: deployment changes the target.", grid);
            return new ScrollViewer { Content = page, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        }

        private static Button SmallButton(string glyph, string tip, Action click)
        {
            var b = Ui.IconButton(glyph, tip, click, subtle: false);
            b.Margin = new Thickness(0, 4, 0, 4);
            return b;
        }

        private void Copy(SourceEditor from, SourceEditor to)
        {
            from.Save();
            var copy = from.Side.Clone();
            to.Load(copy, from.Password);
        }

        private void Switch()
        {
            _source.Save();
            _target.Save();
            var s = _source.Side.Clone();
            var sp = _source.Password;
            _source.Load(_target.Side.Clone(), _target.Password);
            _target.Load(s, sp);
        }

        // ---- Table mapping (schema) ----

        public sealed class TableMappingRow
        {
            public string Source { get; set; }
            public string Target { get; set; }
            /// <summary>`old=new, a=b`.</summary>
            public string Columns { get; set; }
        }

        public sealed class OwnerMappingRow
        {
            public string Source { get; set; }
            public string Target { get; set; }
        }

        private static string QName(object o) => o is JsonObject n ? "[" + n.Str("schema") + "].[" + n.Str("name") + "]" : "";

        private static JsonObject ParseName(string text)
        {
            var parts = (text ?? "").Trim().Split(new[] { "].[", "." }, StringSplitOptions.None);
            string clean(string s) => s.Trim().Trim('[', ']');
            return parts.Length >= 2
                ? new JsonObject { ["schema"] = clean(parts[0]), ["name"] = clean(string.Join(".", parts.Skip(1))) }
                : new JsonObject { ["schema"] = "dbo", ["name"] = clean(parts[0]) };
        }

        private UIElement TableMapping()
        {
            if (!_data)
            {
                var mappings = _project.Obj("schemaMappings") ?? new JsonObject();
                foreach (var t in mappings.List("tables").OfType<JsonObject>())
                {
                    var cols = t.List("columns").OfType<List<object>>().Select(p => p.Count == 2 ? p[0] + "=" + p[1] : "");
                    var target = t.Obj("target");
                    _tableMappings.Add(new TableMappingRow { Source = QName(t.Obj("source")), Target = string.IsNullOrEmpty(target?.Str("name")) ? "" : QName(target), Columns = string.Join(", ", cols) });
                }
            }
            _tablePane = new MappingPane(true, Preview, MapTable, UnmapTable, () => { if (_data) foreach (var r in _dataTables) r.Target = r.Source; else _tableMappings.Clear(); }, EditColumns);
            const string help = "Tables pair by owner and name. Map a table with a differently named one, or unmap a pair to compare them as two separate tables; similar columns pair automatically unless that option is off. Double-click a mapped table to map its columns.";
            return Page("Table mapping", help, _tablePane);
        }

        private MappingPane _tablePane, _ownerPane;
        private JsonObject _lastPreview;

        /// <summary>The pairs as the comparison will make them, from the project as edited so far.</summary>
        private async System.Threading.Tasks.Task<JsonObject> Preview(bool refresh)
        {
            Collect();
            // Nothing to read until both sides are chosen.
            bool incomplete(JsonObject s) => s == null || (s.Str("type") == "empty" ? false : s.Str("type") == "database" || string.IsNullOrEmpty(s.Str("type")) ? string.IsNullOrEmpty(s.Str("server")) || string.IsNullOrEmpty(s.Str("database")) : string.IsNullOrEmpty(s.Str("path")));
            if (incomplete(_project.Obj("source")) || incomplete(_project.Obj("target"))) return null;
            _lastPreview = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("mapping_preview", new JsonObject { ["project"] = _session.WithPasswords(), ["refresh"] = refresh }));
            return _lastPreview;
        }

        private DataTableRow DataRow(string source)
        {
            var row = _dataTables.FirstOrDefault(r => string.Equals(r.Source, source, StringComparison.OrdinalIgnoreCase));
            if (row == null)
            {
                row = new DataTableRow { Source = source, Target = source };
                _dataTables.Add(row);
            }
            return row;
        }

        private void MapTable(string source, string target)
        {
            if (_data)
            {
                // The data engine lets a user mapping displace the default pairs of both tables.
                DataRow(source).Target = target;
                return;
            }
            var columns = _tableMappings.FirstOrDefault(m => string.Equals(m.Source, source, StringComparison.OrdinalIgnoreCase))?.Columns;
            foreach (var m in _tableMappings.Where(m => string.Equals(m.Source, source, StringComparison.OrdinalIgnoreCase)).ToList()) _tableMappings.Remove(m);
            _tableMappings.Add(new TableMappingRow { Source = source, Target = target, Columns = columns });
            // The source table that pairs with the target by name is left unmapped.
            foreach (var p in (_lastPreview?.Obj("tables")?.List("mapped") ?? new List<object>()).OfType<JsonObject>())
            {
                var other = QName(p.Obj("source"));
                if (string.Equals(QName(p.Obj("target")), target, StringComparison.OrdinalIgnoreCase) && !string.Equals(other, source, StringComparison.OrdinalIgnoreCase)) UnmapTable(other);
            }
        }

        /// <summary>An empty target leaves the table unpaired, even with a table of the same name.</summary>
        private void UnmapTable(string source)
        {
            if (_data)
            {
                DataRow(source).Target = "";
                return;
            }
            foreach (var m in _tableMappings.Where(m => string.Equals(m.Source, source, StringComparison.OrdinalIgnoreCase)).ToList()) _tableMappings.Remove(m);
            _tableMappings.Add(new TableMappingRow { Source = source, Target = "" });
        }

        /// <summary>Maps the columns of a mapped table (old=new pairs).</summary>
        private async void EditColumns(JsonObject pair)
        {
            var source = QName(pair.Obj("source"));
            var target = QName(pair.Obj("target"));
            var current = string.Join(", ", pair.List("columns").OfType<JsonObject>().Where(c => !c.Bool("auto")).Select(c => c.Str("source") + "=" + c.Str("target")));
            var result = Dialogs.MapColumns(_session.Host.Owner, source, pair.Strings("sourceColumns"), pair.Strings("targetColumns"), current);
            if (result == null) return;
            if (_data)
            {
                var row = DataRow(source);
                row.ColumnMap = result;
                if (string.IsNullOrEmpty(row.Target)) row.Target = target;
            }
            else
            {
                var row = _tableMappings.FirstOrDefault(m => string.Equals(m.Source, source, StringComparison.OrdinalIgnoreCase));
                if (row == null) _tableMappings.Add(row = new TableMappingRow { Source = source, Target = target });
                row.Columns = result;
            }
            await _tablePane.Reload();
        }
        private UIElement OwnerMapping()
        {
            var mappings = _project.Obj(_data ? "dataMappings" : "schemaMappings") ?? new JsonObject();
            foreach (var pair in mappings.List("schemas").OfType<List<object>>())
            {
                if (pair.Count == 2) _ownerMappings.Add(new OwnerMappingRow { Source = pair[0] as string, Target = pair[1] as string });
            }
            void map(string source, string target)
            {
                foreach (var m in _ownerMappings.Where(m => m.Source == source || m.Target == target).ToList()) _ownerMappings.Remove(m);
                _ownerMappings.Add(new OwnerMappingRow { Source = source, Target = target });
            }
            void unmap(string source)
            {
                foreach (var m in _ownerMappings.Where(m => m.Source == source).ToList()) _ownerMappings.Remove(m);
                _ownerMappings.Add(new OwnerMappingRow { Source = source, Target = "" });
            }
            _ownerPane = new MappingPane(false, Preview, map, unmap, () => _ownerMappings.Clear(), null);
            const string help = "Objects pair with objects of the same owner (schema). Map an owner of the source with a different owner of the target here.";
            return Page("Owner mapping", help, _ownerPane);
        }
        // ---- Tables & views (data) ----

        public sealed class DataTableRow
        {
            public bool Include { get; set; } = true;
            public string Source { get; set; }
            public string Target { get; set; }
            public string Key { get; set; }
            public List<string> KeyChoices { get; set; } = new List<string>();
            public string Where1 { get; set; }
            public string Where2 { get; set; }
            public string Problem { get; set; }
            public string Columns { get; set; }
            public string ExcludedColumns { get; set; }
            /// <summary>`old=new, …`: source columns paired with differently named target columns.</summary>
            public string ColumnMap { get; set; }
            public List<string> SourceColumns { get; set; } = new List<string>();
            public List<string> TargetColumns { get; set; } = new List<string>();
        }

        private UIElement TablesAndViews()
        {
            _dataStatus = Ui.Text("Load the tables to choose which are compared, their keys and filters.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            _dataStatus.Margin = new Thickness(0, 0, 0, 10);
            var grid = _dataGrid = new DataGrid
            {
                ItemsSource = _dataTables,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
            };
            grid.Columns.Add(Ui.CheckColumn("Compare", "Include", 76));
            grid.Columns.Add(new DataGridTextColumn { Header = "Source", Binding = new Binding("Source"), IsReadOnly = true, Width = 200 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Target", Binding = new Binding("Target") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged }, Width = 200 });
            var key = new DataGridComboBoxColumn
            {
                Header = "Comparison key (index, or col, col)",
                Width = 200,
                TextBinding = new Binding("Key") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged },
            };
            key.ElementStyle = new Style(typeof(ComboBox)) { Setters = { new Setter(ItemsControl.ItemsSourceProperty, new Binding("KeyChoices")) } };
            key.EditingElementStyle = new Style(typeof(ComboBox))
            {
                Setters = { new Setter(ItemsControl.ItemsSourceProperty, new Binding("KeyChoices")), new Setter(ComboBox.IsEditableProperty, true) },
            };
            grid.Columns.Add(key);
            grid.Columns.Add(new DataGridTextColumn { Header = "WHERE (source)", Binding = new Binding("Where1"), Width = 160 });
            grid.Columns.Add(new DataGridTextColumn { Header = "WHERE (target)", Binding = new Binding("Where2"), Width = 160 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Columns left out", Binding = new Binding("ExcludedColumns"), Width = 160 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Column mapping (old=new)", Binding = new Binding("ColumnMap"), Width = 180 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Problem", Binding = new Binding("Problem"), IsReadOnly = true, Width = 260 });

            var all = Ui.Button("All", () => { foreach (var r in _dataTables) r.Include = true; grid.Items.Refresh(); }, minWidth: 60);
            var none = Ui.Button("None", () => { foreach (var r in _dataTables) r.Include = false; grid.Items.Refresh(); }, minWidth: 60);
            var load = Ui.Button("Load tables", async () => await LoadTables(), primary: true, minWidth: 110, glyph: Glyph.Refresh);
            var map = Ui.Button("Columns…", () =>
            {
                if (!(grid.SelectedItem is DataTableRow r)) return;
                grid.CommitEdit(DataGridEditingUnit.Row, true);
                if (r.SourceColumns.Count == 0)
                {
                    Ui.Error(_session.Host.Owner, "Load the tables first, to see their columns.");
                    return;
                }
                var targetColumns = _targetColumns.TryGetValue(r.Target ?? "", out var tc) ? tc : r.TargetColumns;
                var result = Dialogs.MapColumns(_session.Host.Owner, r.Source, r.SourceColumns, targetColumns, r.ColumnMap);
                if (result == null) return;
                r.ColumnMap = result;
                grid.Items.Refresh();
            }, minWidth: 90, glyph: Glyph.Edit);
            map.ToolTip = "Pair the selected table's columns with differently named target columns";
            _includeNew = Ui.Check("Include new tables and views", _project.Bool("includeNewTables") || !_project.ContainsKey("includeNewTables"));
            _includeNew.ToolTip = "Tables added to the databases later are compared too. Clear it to compare only the tables ticked now.";
            var content = new DockPanel();
            DockPanel.SetDock(_dataStatus, Dock.Top);
            DockPanel.SetDock(_includeNew, Dock.Bottom);
            _includeNew.Margin = new Thickness(0, 10, 0, 0);
            content.Children.Add(_dataStatus);
            content.Children.Add(_includeNew);
            content.Children.Add(grid);
            var panel = Page("Tables & views", "Choose the tables to compare, the key that pairs their rows, and filters. Type another target table to compare a table with a differently named one.", content, Ui.Row(load, map, all, none));
            // Rows the project already describes are shown before the tables are loaded.
            var mappings = _project.Obj("dataMappings") ?? new JsonObject();
            foreach (var t in mappings.List("tables").OfType<JsonObject>())
            {
                _dataTables.Add(FromOverride(t));
            }
            return panel;
        }

        private static DataTableRow FromOverride(JsonObject t) => new DataTableRow
        {
            Source = QName(t.Obj("source")),
            Target = t.Obj("target") != null ? QName(t.Obj("target")) : QName(t.Obj("source")),
            Include = !t.Bool("excluded"),
            Key = KeyText(t.Strings("key")),
            Where1 = t.Str("where1"),
            Where2 = t.Str("where2"),
            ExcludedColumns = string.Join(", ", t.Strings("excludedColumns")),
            ColumnMap = string.Join(", ", t.List("columns").OfType<List<object>>().Where(p => p.Count == 2).Select(p => p[0] + "=" + p[1])),
        };

        private static string KeyText(List<string> key)
        {
            if (key.Count == 1 && key[0].StartsWith("index:", StringComparison.Ordinal)) return key[0].Substring(6);
            return key.Count == 0 ? "" : "(" + string.Join(", ", key) + ")";
        }

        private async System.Threading.Tasks.Task LoadTables()
        {
            Collect();
            _dataStatus.Text = "Loading tables…";
            var result = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("data_tables", new JsonObject { ["project"] = _session.WithPasswords() }));
            if (result == null)
            {
                _dataStatus.Text = "Could not load the tables.";
                return;
            }
            var previous = _dataTables.GroupBy(r => r.Source, StringComparer.OrdinalIgnoreCase).ToDictionary(g => g.Key, g => g.First(), StringComparer.OrdinalIgnoreCase);
            _targetColumns.Clear();
            if (result.Obj("targetColumns") is JsonObject targets)
            {
                foreach (var kv in targets) _targetColumns[kv.Key] = (kv.Value as List<object> ?? new List<object>()).OfType<string>().ToList();
            }
            var sourceColumns = result.Obj("sourceColumns") ?? new JsonObject();
            _dataTables.Clear();
            foreach (var t in result.List("tables").OfType<JsonObject>())
            {
                var source = t.Str("source");
                previous.TryGetValue(source, out var old);
                bool named(string key) => _project.List(key).OfType<JsonObject>().Any(d => "[" + d.Str("schema") + "].[" + d.Str("name") + "]" == source);
                // With new tables left out, a table the project does not name is not compared.
                var deselected = _project.ContainsKey("includeNewTables") && !_project.Bool("includeNewTables") ? !named("selected") : named("deselected");
                var row = new DataTableRow
                {
                    Source = source,
                    Target = t.Str("target"),
                    Include = old?.Include ?? !deselected,
                    Key = old != null && !string.IsNullOrEmpty(old.Key) ? old.Key : t.Str("keyName") ?? "",
                    Where1 = old?.Where1 ?? t.Str("where1"),
                    Where2 = old?.Where2 ?? t.Str("where2"),
                    ExcludedColumns = old?.ExcludedColumns,
                    ColumnMap = old?.ColumnMap,
                    Problem = t.Str("problem"),
                    Columns = string.Join(", ", t.List("columns").OfType<JsonObject>().Select(c => c.Str("source"))),
                    SourceColumns = (sourceColumns[source] as List<object> ?? new List<object>()).OfType<string>().ToList(),
                    TargetColumns = t.List("columns").OfType<JsonObject>().Select(c => c.Str("target")).ToList(),
                };
                if (old != null && !string.IsNullOrWhiteSpace(old.Target) && !string.Equals(old.Target, t.Str("target"), StringComparison.OrdinalIgnoreCase)) row.Target = old.Target;
                row.KeyChoices.AddRange(t.Strings("keyCandidates"));
                _dataTables.Add(row);
            }
            _tablesLoaded = true;
            var only1 = result.Strings("onlyIn1");
            var only2 = result.Strings("onlyIn2");
            _dataStatus.Text = Ui.Plural(_dataTables.Count, "table pair", "table pairs")
                + (only1.Count > 0 ? "; only in source: " + string.Join(", ", only1) : "")
                + (only2.Count > 0 ? "; only in target: " + string.Join(", ", only2) : "");
        }

        // ---- Options ----

        private UIElement Options()
        {
            var defs = _session.OptionDefinitions.List(_data ? "data" : "schema").OfType<JsonObject>().ToList();
            var current = new HashSet<string>(_project.Strings("options"), StringComparer.OrdinalIgnoreCase);
            var list = new StackPanel { Margin = new Thickness(0, 0, 12, 0) };
            var find = Ui.SearchBox("Find an option", 280);
            var count = Ui.Text("", brush: CompareTheme.Muted);
            count.Margin = new Thickness(12, 0, 0, 0);
            _optionHelp = Ui.Text("Point at an option to see what it does.", wrap: TextWrapping.Wrap, brush: CompareTheme.Muted);

            var groups = defs.Select(d => d.Str("group")).Where(g => g != "Hidden").Distinct().ToList();
            var headers = new Dictionary<string, TextBlock>();
            foreach (var group in groups)
            {
                var header = Ui.Text(group, 15, bold: true);
                header.Margin = new Thickness(0, list.Children.Count == 0 ? 0 : 18, 0, 6);
                headers[group] = header;
                list.Children.Add(header);
                foreach (var d in defs.Where(x => x.Str("group") == group))
                {
                    var name = d.Str("name");
                    var negated = d.Bool("negated");
                    var box = new CheckBox
                    {
                        Content = d.Str("label"),
                        IsChecked = negated ? !current.Contains(name) : current.Contains(name),
                        Margin = new Thickness(0, 1, 0, 1),
                        Tag = d,
                    };
                    box.Checked += (s, e) => OptionChanged(defs, d, true);
                    box.Unchecked += (s, e) => OptionChanged(defs, d, false);
                    box.MouseEnter += (s, e) => _optionHelp.Text = d.Str("label") + "\n\n" + Help(d);
                    box.GotKeyboardFocus += (s, e) => _optionHelp.Text = d.Str("label") + "\n\n" + Help(d);
                    _optionBoxes[name] = box;
                    list.Children.Add(box);
                }
            }
            find.TextChanged += (s, e) =>
            {
                var terms = find.Text.Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries);
                var shown = 0;
                foreach (var box in _optionBoxes.Values)
                {
                    var d = (JsonObject)box.Tag;
                    var text = d.Str("label") + " " + d.Str("group");
                    var visible = terms.All(t => text.IndexOf(t, StringComparison.OrdinalIgnoreCase) >= 0);
                    box.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
                    if (visible) shown++;
                }
                count.Text = terms.Length == 0 ? "" : "showing " + shown + " of " + _optionBoxes.Count + " options";
            };

            var buttons = Ui.Row(
                Ui.Button("Save as my defaults", () => { var settings = CompareSettings.Load(); if (_data) settings.MyDataDefaults = Selected(defs); else settings.MySchemaDefaults = Selected(defs); settings.Save(); }),
                Ui.Button("QuillSQL defaults", () => SetAll(defs, defs.Where(d => d.Bool("default")).Select(d => d.Str("name")))),
                Ui.Button("My defaults", () =>
                {
                    var mine = _data ? CompareSettings.Load().MyDataDefaults : CompareSettings.Load().MySchemaDefaults;
                    SetAll(defs, mine ?? defs.Where(d => d.Bool("default")).Select(d => d.Str("name")));
                }));
            var top = Ui.Row(find, count);
            top.Margin = new Thickness(0, 0, 0, 12);
            var helpTitle = Ui.Row(Ui.Icon(Glyph.Info, 14, CompareTheme.Accent), new Border { Width = 8 }, Ui.Text("About this option", bold: true));
            helpTitle.Margin = new Thickness(0, 0, 0, 8);
            var side = Ui.Card(Ui.Column(helpTitle, _optionHelp));
            side.Width = 300;
            side.VerticalAlignment = VerticalAlignment.Top;
            side.Margin = new Thickness(12, 0, 0, 0);
            buttons.Margin = new Thickness(-8, 12, 0, 0);
            var layout = new DockPanel();
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(buttons, Dock.Bottom);
            DockPanel.SetDock(side, Dock.Right);
            layout.Children.Add(top);
            layout.Children.Add(buttons);
            layout.Children.Add(side);
            layout.Children.Add(new ScrollViewer { Content = list, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });
            return Page("Options", "What counts as a difference, and how the deployment script is written.", layout);
        }

        private static string Help(JsonObject d)
        {
            if (!string.IsNullOrEmpty(d.Str("description"))) return d.Str("description");
            switch (d.Str("kind"))
            {
                case "Deploy": return "Changes the deployment script only.";
                case "Read": return "Changes how the databases are read.";
                case "Compare": return "Changes what counts as a difference, and so what is deployed.";
                default: return d.Str("group") == "Deployment" ? "Changes the deployment script." : "Changes how rows are paired and compared.";
            }
        }

        private bool _cascading;

        /// <summary>Ticking an option ticks what it requires; clearing one clears what requires it.</summary>
        private void OptionChanged(List<JsonObject> defs, JsonObject d, bool isChecked)
        {
            if (_cascading) return;
            _cascading = true;
            try
            {
                var engineOn = d.Bool("negated") ? !isChecked : isChecked;
                if (engineOn)
                {
                    foreach (var r in d.Strings("requires"))
                    {
                        SetEngine(defs, r, true);
                    }
                }
                else
                {
                    foreach (var other in defs.Where(o => o.Strings("requires").Contains(d.Str("name"))))
                    {
                        SetEngine(defs, other.Str("name"), false);
                    }
                }
                if (_data && engineOn && (d.Str("name") == "IncludeIdenticalRecords" || d.Str("name") == "UseChecksumComparison"))
                {
                    SetEngine(defs, d.Str("name") == "IncludeIdenticalRecords" ? "UseChecksumComparison" : "IncludeIdenticalRecords", false);
                }
            }
            finally
            {
                _cascading = false;
            }
        }

        private void SetEngine(List<JsonObject> defs, string name, bool on)
        {
            if (!_optionBoxes.TryGetValue(name, out var box)) return;
            var d = defs.First(x => x.Str("name") == name);
            box.IsChecked = d.Bool("negated") ? !on : on;
        }

        private void SetAll(List<JsonObject> defs, IEnumerable<string> names)
        {
            var on = new HashSet<string>(names, StringComparer.OrdinalIgnoreCase);
            _cascading = true;
            foreach (var d in defs)
            {
                if (_optionBoxes.TryGetValue(d.Str("name"), out var box))
                {
                    var engineOn = on.Contains(d.Str("name"));
                    box.IsChecked = d.Bool("negated") ? !engineOn : engineOn;
                }
            }
            _cascading = false;
        }

        /// <summary>Engine flags that are on, including hidden ones the project already had.</summary>
        private List<string> Selected(List<JsonObject> defs)
        {
            var result = new List<string>();
            var current = new HashSet<string>(_project.Strings("options"), StringComparer.OrdinalIgnoreCase);
            foreach (var d in defs)
            {
                var name = d.Str("name");
                if (_optionBoxes.TryGetValue(name, out var box))
                {
                    var engineOn = d.Bool("negated") ? box.IsChecked != true : box.IsChecked == true;
                    if (engineOn) result.Add(name);
                }
                else if (current.Contains(name))
                {
                    // Unlike the classic compare tool, flags without a check box are kept.
                    result.Add(name);
                }
            }
            return result;
        }

        /// <summary>Writes every tab back into the project.</summary>
        /// <param name="persist">Also remember passwords and servers (false for the unsaved-changes check).</param>
        public void Collect(bool persist = true)
        {
            _source?.Save(persist);
            _target?.Save(persist);
            var defs = _session.OptionDefinitions.List(_data ? "data" : "schema").OfType<JsonObject>().ToList();
            if (_optionBoxes.Count > 0) _project["options"] = Selected(defs).Cast<object>().ToList();

            var schemaMappings = _project.Obj("schemaMappings") ?? new JsonObject();
            schemaMappings["schemas"] = _ownerMappings.Where(m => !string.IsNullOrWhiteSpace(m.Source))
                .Select(m => (object)new List<object> { m.Source.Trim(), (m.Target ?? "").Trim() }).ToList();
            if (!_data)
            {
                schemaMappings["tables"] = _tableMappings.Where(m => !string.IsNullOrWhiteSpace(m.Source))
                    .Select(m => (object)new JsonObject
                    {
                        ["source"] = ParseName(m.Source),
                        ["target"] = ParseName(m.Target ?? ""),
                        ["columns"] = (m.Columns ?? "").Split(',').Select(p => p.Split('=')).Where(p => p.Length == 2 && p[0].Trim() != "")
                            .Select(p => (object)new List<object> { p[0].Trim().Trim('[', ']'), p[1].Trim().Trim('[', ']') }).ToList(),
                    }).ToList();
            }
            _project["schemaMappings"] = schemaMappings;

            if (_data)
            {
                var overrides = new List<object>();
                var deselected = new List<object>();
                foreach (var r in _dataTables)
                {
                    var src = ParseName(r.Source);
                    var o = new JsonObject { ["source"] = src };
                    if (!string.Equals(r.Source, r.Target, StringComparison.OrdinalIgnoreCase)) o["target"] = ParseName(r.Target ?? "");
                    var keyText = (r.Key ?? "").Trim();
                    if (keyText.StartsWith("(", StringComparison.Ordinal))
                    {
                        o["key"] = keyText.Trim('(', ')').Split(',').Select(k => (object)k.Trim().Trim('[', ']')).Where(k => (string)k != "").ToList();
                    }
                    else if (keyText != "" && r.KeyChoices.Count > 0 && keyText != r.KeyChoices[0])
                    {
                        o["key"] = new List<object> { "index:" + keyText };
                    }
                    if (!string.IsNullOrWhiteSpace(r.Where1)) o["where1"] = r.Where1;
                    if (!string.IsNullOrWhiteSpace(r.Where2)) o["where2"] = r.Where2;
                    var pairs = (r.ColumnMap ?? "").Split(',').Select(p => p.Split('=')).Where(p => p.Length == 2 && p[0].Trim() != "" && p[1].Trim() != "")
                        .Select(p => (object)new List<object> { p[0].Trim().Trim('[', ']'), p[1].Trim().Trim('[', ']') }).ToList();
                    if (pairs.Count > 0) o["columns"] = pairs;
                    if (!string.IsNullOrWhiteSpace(r.ExcludedColumns))
                    {
                        o["excludedColumns"] = r.ExcludedColumns.Split(',').Select(c => (object)c.Trim().Trim('[', ']')).Where(c => (string)c != "").ToList();
                    }
                    if (o.Count > 1) overrides.Add(o);
                    if (!r.Include) deselected.Add(new JsonObject { ["type"] = "Table", ["schema"] = src.Str("schema"), ["name"] = src.Str("name") });
                }
                var dm = _project.Obj("dataMappings") ?? new JsonObject();
                dm["tables"] = overrides;
                dm["schemas"] = schemaMappings["schemas"];
                _project["dataMappings"] = dm;
                _project["deselected"] = deselected;
                // "Include new tables" off: the tables ticked now are the only ones compared.
                if (_includeNew != null && _includeNew.IsChecked != true && !_tablesLoaded && _project.ContainsKey("selected"))
                {
                    // The tables were not loaded: the list stays as the project had it.
                    _project["includeNewTables"] = false;
                }
                else if (_includeNew != null && _includeNew.IsChecked != true)
                {
                    _project["includeNewTables"] = false;
                    _project["selected"] = _dataTables.Where(r => r.Include).Select(r => { var n = ParseName(r.Source); return (object)new JsonObject { ["type"] = "Table", ["schema"] = n.Str("schema"), ["name"] = n.Str("name") }; }).ToList();
                }
                else
                {
                    _project.Remove("includeNewTables");
                    _project.Remove("selected");
                }
            }
        }
    }

    /// <summary>One side of the Data sources tab.</summary>
    internal sealed class SourceEditor : StackPanel
    {
        private readonly CompareSession _session;
        private readonly string _key;
        private ComboBox _type, _server, _auth, _database, _snapshot, _folder, _sqlVersion, _collation;
        private TextBox _user, _revision, _tenant;
        private StackPanel _tenantField, _folderDefaults;
        /// <summary>The Authentication box's choices and their engine values.</summary>
        private static readonly (string Label, string Value)[] Authentications =
        {
            ("Windows authentication", "windows"),
            ("SQL Server authentication", "sql"),
            ("Microsoft Entra integrated", "entraIntegrated"),
            ("Microsoft Entra password", "entraPassword"),
            ("Microsoft Entra with MFA", "entraInteractive"),
            ("Microsoft Entra (access token)", "entra"),
        };
        /// <summary>SQL Server versions a scripts folder without database information can be read as.</summary>
        private static readonly (string Label, string Value)[] SqlVersions =
        {
            ("SQL Server 2008", "10"), ("SQL Server 2012", "11"), ("SQL Server 2014", "12"), ("SQL Server 2016", "13"),
            ("SQL Server 2017", "14"), ("SQL Server 2019", "15"), ("SQL Server 2022", "16"), ("SQL Server 2025", "17"),
        };
        private PasswordBox _password;
        private CheckBox _remember, _encrypt, _trust;
        private StackPanel _live, _file, _scripts, _revisionField;
        private TextBlock _empty;
        private BackupPane _backup;
        private TextBlock _status;

        public JsonObject Side { get; private set; }
        public string Password => _password.Password;

        public SourceEditor(CompareSession session, string key, string title)
        {
            _session = session;
            _key = key;
            var badge = new Border { Width = 36, Height = 36, CornerRadius = new CornerRadius(8), Child = Ui.Icon(Glyph.Server, 17, CompareTheme.AccentText) };
            ((TextBlock)badge.Child).HorizontalAlignment = HorizontalAlignment.Center;
            badge.SetResourceReference(Border.BackgroundProperty, "Q.Accent");
            var names = Ui.Column(Ui.Title(title, 17), Ui.Text(key == "source" ? "Database 1" : "Database 2", 12, brush: CompareTheme.Muted));
            names.Margin = new Thickness(12, 0, 0, 0);
            _type = new ComboBox { Width = 150, VerticalAlignment = VerticalAlignment.Center };
            // The classic compare tool's other kinds of data source are listed but not available yet.
            foreach (var (name, enabled) in new[] { ("Database", true), ("Snapshot", !session.IsData), ("Empty database", !session.IsData && key == "target"), ("Backup", true), ("Scripts folder", true), ("Source control", true) })
            {
                _type.Items.Add(new ComboBoxItem { Content = name, IsEnabled = enabled, ToolTip = enabled ? null : name == "Snapshot" || name == "Empty database" ? "Not available for this side or kind of comparison" : "Not available yet" });
            }
            _type.SelectionChanged += (s, e) => ShowType();
            var header = new DockPanel { Margin = new Thickness(0, 0, 0, 18) };
            DockPanel.SetDock(badge, Dock.Left);
            DockPanel.SetDock(_type, Dock.Right);
            header.Children.Add(badge);
            header.Children.Add(_type);
            header.Children.Add(names);
            Children.Add(header);

            _server = new ComboBox { IsEditable = true };
            ServerDiscovery.Attach(_server);
            _auth = new ComboBox { ItemsSource = Authentications.Select(a => a.Label).ToList() };
            _auth.SelectionChanged += (s, e) => ShowAuthentication();
            _user = new TextBox();
            _tenant = new TextBox { ToolTip = "The Microsoft Entra tenant: its id or domain. Empty: the domain of the user name, else any work or school account" };
            _tenantField = Ui.Form(Ui.Field("Tenant (optional)", _tenant));
            _password = new PasswordBox();
            _remember = Ui.Check("Remember credentials (Windows Credential Manager)", false);
            _encrypt = Ui.Check("Encrypt", true);
            _trust = Ui.Check("Trust server certificate", false);
            _database = new ComboBox { IsEditable = true };
            var refresh = Ui.IconButton(Glyph.Refresh, "List the databases", async () => await ListDatabases(), subtle: false);
            refresh.Margin = new Thickness(8, 0, 0, 0);
            var dbRow = new DockPanel();
            DockPanel.SetDock(refresh, Dock.Right);
            dbRow.Children.Add(refresh);
            dbRow.Children.Add(_database);
            var create = Ui.Button("New database…", async () => await CreateDatabase(), minWidth: 70, glyph: Glyph.Add);
            _status = Ui.Text("", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            var test = Ui.Button("Test connection", async () => await Test(), minWidth: 110, glyph: Glyph.Connect);
            test.Margin = new Thickness(0);
            _live = Ui.Column(Ui.Form(
                Ui.Field("Server", _server),
                Ui.Field("Authentication", _auth),
                Ui.Field("User name", _user),
                Ui.Field("Password", _password),
                Ui.Field(null, _remember),
                Ui.Field(null, _tenantField),
                Ui.Field(null, Ui.Row(_encrypt, _trust)),
                Ui.Field("Database", dbRow),
                Ui.Field(null, Ui.Row(test, create))), _status);
            var settings = CompareSettings.Load();
            _snapshot = new ComboBox { IsEditable = true, ItemsSource = settings.RecentSnapshots.ToList(), ToolTip = "Recently used snapshots are in the list" };
            var browse = Ui.Button("Browse…", glyph: Glyph.Open, click: () =>
            {
                var d = new OpenFileDialog { Filter = "Snapshots (*.snp, *.json)|*.snp;*.json|All files|*.*" };
                if (Dialogs.Show(d, null) == true) _snapshot.Text = d.FileName;
            });
            var fileRow = new DockPanel();
            DockPanel.SetDock(browse, Dock.Right);
            fileRow.Children.Add(browse);
            fileRow.Children.Add(_snapshot);
            _file = Ui.Column(Ui.Form(Ui.Field("Snapshot file", fileRow)), Ui.Text("Make a snapshot with quill schema /makesnapshot or Tools > Save snapshot.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            _empty = Ui.Text("An empty database: every object of the source is created. The changes can only be scripted, and the script can then run on a new database.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            Children.Add(_live);
            // A scripts folder in the classic compare tool's layout, or a revision of it in git, Mercurial or Subversion.
            _folder = new ComboBox { IsEditable = true, ItemsSource = settings.RecentFolders.ToList(), ToolTip = "Recently used scripts folders are in the list" };
            var browseFolder = Ui.Button("Browse…", glyph: Glyph.Folder, click: () =>
            {
                var f = Dialogs.PickFolder(_session.Host.Owner, _folder.Text, "Open scripts folder");
                if (f != null) _folder.Text = f;
            });
            var folderRow = new DockPanel();
            DockPanel.SetDock(browseFolder, Dock.Right);
            folderRow.Children.Add(browseFolder);
            folderRow.Children.Add(_folder);
            _revision = Ui.SearchBox("HEAD", 0);
            _revision.Width = double.NaN;
            _revision.ToolTip = "A source control revision of the folder: HEAD, a commit, a tag or a revision number";
            var choose = Ui.Button("Choose…", async () => await ChooseRevision(), minWidth: 80);
            choose.ToolTip = "Pick a revision from the folder's history";
            var revisionRow = new DockPanel();
            DockPanel.SetDock(choose, Dock.Right);
            revisionRow.Children.Add(choose);
            revisionRow.Children.Add(_revision);
            _revisionField = Ui.Form(Ui.Field("Revision", revisionRow));
            // Used when the folder has no database information (QuillDatabaseInfo.xml).
            _sqlVersion = new ComboBox { ItemsSource = SqlVersions.Select(v => v.Label).ToList(), SelectedIndex = 6 };
            _collation = new ComboBox { IsEditable = true, Text = "Latin1_General_CI_AS", ItemsSource = new[] { "Latin1_General_CI_AS", "SQL_Latin1_General_CP1_CI_AS", "Latin1_General_CS_AS", "Latin1_General_BIN2", "Latin1_General_100_CI_AS_SC_UTF8" } };
            _folderDefaults = Ui.Form(Ui.Field("SQL Server version (when the folder has no database information)", _sqlVersion), Ui.Field("Default collation", _collation));
            _scripts = Ui.Column(Ui.Form(Ui.Field("Scripts folder (in a git, Mercurial or Subversion working copy for source control)", folderRow)), _revisionField, _folderDefaults,
                Ui.Text("Deploying to a scripts folder rewrites the files of the objects deployed; a revision can only be compared or scripted.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            Children.Add(_file);
            Children.Add(_empty);
            Children.Add(_scripts);
            _backup = new BackupPane(session);
            Children.Add(_backup);
            Load(session.Project.Obj(key) ?? new JsonObject(), session.Passwords.TryGetValue(key, out var p) ? p : null);
        }

        private void ShowType()
        {
            if (_type.SelectedItem is ComboBoxItem item && !item.IsEnabled)
            {
                _type.SelectedIndex = 0;
                return;
            }
            var snapshot = _type.SelectedIndex == 1;
            var empty = _type.SelectedIndex == 2;
            // Source control is a scripts folder in git, Mercurial or Subversion at a revision.
            var control = _type.SelectedIndex == 5;
            var scripts = _type.SelectedIndex == 4 || control;
            var backup = _type.SelectedIndex == 3;
            _revisionField.Visibility = control ? Visibility.Visible : Visibility.Collapsed;
            _folderDefaults.Visibility = scripts && !control ? Visibility.Visible : Visibility.Collapsed;
            if (control && _revision.Text.Trim().Length == 0) _revision.Text = "HEAD";
            _live.Visibility = snapshot || empty || scripts || backup ? Visibility.Collapsed : Visibility.Visible;
            _backup.Visibility = backup ? Visibility.Visible : Visibility.Collapsed;
            _scripts.Visibility = scripts ? Visibility.Visible : Visibility.Collapsed;
            _file.Visibility = snapshot ? Visibility.Visible : Visibility.Collapsed;
            _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        }

        public void Load(JsonObject side, string password)
        {
            Side = side;
            _type.SelectedIndex = side.Str("type") == "snapshot" ? 1 : side.Str("type") == "empty" ? 2 : side.Str("type") == "scripts" ? (string.IsNullOrEmpty(side.Str("revision")) ? 4 : 5) : side.Str("type") == "backup" ? 3 : 0;
            _server.Text = side.Str("server") ?? "";
            var auth = side.Str("authentication") ?? "windows";
            _auth.SelectedIndex = Math.Max(0, Array.FindIndex(Authentications, a => string.Equals(a.Value, auth, StringComparison.OrdinalIgnoreCase)));
            _user.Text = side.Str("user") ?? "";
            _tenant.Text = side.Str("tenant") ?? "";
            var version = Array.FindIndex(SqlVersions, v => v.Value == side.Str("sqlVersion"));
            _sqlVersion.SelectedIndex = version >= 0 ? version : 6;
            _collation.Text = string.IsNullOrEmpty(side.Str("defaultCollation")) ? "Latin1_General_CI_AS" : side.Str("defaultCollation");
            _remember.IsChecked = side.Bool("rememberPassword");
            _password.Password = password ?? (side.Bool("rememberPassword") ? Credentials.Read(side.Str("server"), side.Str("user")) ?? "" : "");
            _encrypt.IsChecked = !side.ContainsKey("encrypt") || side.Bool("encrypt");
            _trust.IsChecked = side.Bool("trustServerCertificate");
            _database.Text = side.Str("database") ?? "";
            _snapshot.Text = side.Str("type") == "snapshot" ? side.Str("path") ?? "" : "";
            _folder.Text = side.Str("type") == "scripts" ? side.Str("path") ?? "" : "";
            _revision.Text = side.Str("revision") ?? "";
            _backup.Load(side, side.Str("type") == "backup" ? password : null);
            if (side.Str("type") == "backup" && !string.IsNullOrEmpty(side.Str("path"))) Loaded += async (s, e) => await _backup.Read();
            ShowType();
        }

        /// <summary>Which sign-in fields apply: user name for SQL, Entra password and MFA (a login hint), password for SQL and Entra password.</summary>
        private void ShowAuthentication()
        {
            var auth = Authentications[Math.Max(0, _auth.SelectedIndex)].Value;
            _user.IsEnabled = auth == "sql" || auth == "entraPassword" || auth == "entraInteractive";
            _password.IsEnabled = _remember.IsEnabled = auth == "sql" || auth == "entraPassword";
            _tenantField.Visibility = auth.StartsWith("entra", StringComparison.Ordinal) && auth != "entra" ? Visibility.Visible : Visibility.Collapsed;
            _user.ToolTip = auth == "entraInteractive" ? "Optional: the account to sign in with (user@domain)" : auth == "entraPassword" ? "The Microsoft Entra user name (user@domain)" : null;
        }

        private string Authentication => Authentications[Math.Max(0, _auth.SelectedIndex)].Value;

        private bool UsesPassword => Authentication == "sql" || Authentication == "entraPassword";

        /// <param name="persist">Also remember the password and the server, as when the user leaves the editor.</param>
        public void Save(bool persist = true)
        {
            Side["type"] = _type.SelectedIndex == 1 ? "snapshot" : _type.SelectedIndex == 2 ? "empty" : _type.SelectedIndex == 3 ? "backup" : _type.SelectedIndex >= 4 ? "scripts" : "database";
            Side["server"] = _server.Text.Trim();
            Side["authentication"] = Authentication;
            Side["user"] = _user.Text.Trim();
            if (_tenant.Text.Trim().Length > 0 && Authentication.StartsWith("entra", StringComparison.Ordinal)) Side["tenant"] = _tenant.Text.Trim();
            else Side.Remove("tenant");
            if (_type.SelectedIndex == 4)
            {
                Side["sqlVersion"] = SqlVersions[Math.Max(0, _sqlVersion.SelectedIndex)].Value;
                Side["defaultCollation"] = _collation.Text.Trim();
            }
            else
            {
                Side.Remove("sqlVersion");
                Side.Remove("defaultCollation");
            }
            Side["rememberPassword"] = _remember.IsChecked == true;
            Side["encrypt"] = _encrypt.IsChecked == true;
            Side["trustServerCertificate"] = _trust.IsChecked == true;
            Side["database"] = _database.Text.Trim();
            Side["path"] = _type.SelectedIndex >= 4 ? _folder.Text.Trim() : _type.SelectedIndex == 3 ? _backup.Path : _snapshot.Text.Trim();
            if (_type.SelectedIndex == 3) Side["backupSet"] = _backup.BackupSet;
            else Side.Remove("backupSet");
            if (_type.SelectedIndex == 5 && _revision.Text.Trim().Length > 0) Side["revision"] = _revision.Text.Trim();
            else Side.Remove("revision");
            _session.Project[_key] = Side;
            _session.Passwords[_key] = _type.SelectedIndex == 3 ? _backup.Passwords : _password.Password;
            if (!persist) return;
            var settings = CompareSettings.Load();
            if (_type.SelectedIndex >= 4) settings.AddRecent(settings.RecentFolders, Side.Str("path"));
            if (_type.SelectedIndex == 1) settings.AddRecent(settings.RecentSnapshots, Side.Str("path"));
            if (UsesPassword && _remember.IsChecked == true && _password.Password.Length > 0)
            {
                try
                {
                    Credentials.Write(Side.Str("server"), Side.Str("user"), _password.Password);
                }
                catch (System.ComponentModel.Win32Exception)
                {
                }
            }
            CompareSettings.Load().AddServer(Side.Str("server"));
        }

        private JsonObject WithPassword(string database = null)
        {
            Save();
            var s = Side.Clone();
            if (database != null) s["database"] = database;
            if (!string.IsNullOrEmpty(_password.Password)) s["password"] = _password.Password;
            if (s.Str("authentication") == "entra" && _session.AccessToken != null) s["accessToken"] = _session.AccessToken;
            return s;
        }

        private async System.Threading.Tasks.Task ListDatabases()
        {
            _status.Text = "Listing databases…";
            var result = await Ui.Try(_session.Host.Owner, () => _session.Engine.Request("databases", new JsonObject { ["source"] = WithPassword("master") }));
            if (result is List<object> names)
            {
                var current = _database.Text;
                _database.ItemsSource = names.Select(n => n as string).ToList();
                _database.Text = current;
                _status.Text = Ui.Plural(names.Count, "database", "databases") + " on " + Side.Str("server") + ".";
            }
            else
            {
                _status.Text = "";
            }
        }

        private async System.Threading.Tasks.Task Test()
        {
            _status.Text = "Connecting…";
            try
            {
                var result = await _session.Engine.RequestObject("test_connection", new JsonObject { ["source"] = WithPassword() });
                _status.Text = "Connected: SQL Server " + result.Str("version") + ", database " + result.Str("database") + ".";
            }
            catch (Exception ex) when (ex is EngineException || ex is System.IO.IOException || ex is InvalidOperationException || ex is System.ComponentModel.Win32Exception)
            {
                // The failure shows here, with its full text a click away.
                _status.Inlines.Clear();
                _status.Inlines.Add(new System.Windows.Documents.Run("Could not connect to " + Side.Str("server") + ". ") { Foreground = CompareTheme.High });
                var details = new System.Windows.Documents.Hyperlink(new System.Windows.Documents.Run("Show details…"));
                var text = ex.Message;
                details.Click += (s, e) => ShowDetails(text);
                _status.Inlines.Add(details);
            }
        }

        /// <summary>The whole error of a failed connection, to read and copy.</summary>
        private void ShowDetails(string text)
        {
            var w = Ui.Dialog(_session.Host.Owner, "Connection error", null, 640, 360);
            var code = Ui.Code();
            code.Text = text;
            code.TextWrapping = TextWrapping.Wrap;
            var heading = Ui.Title("Could not connect to " + Side.Str("server"), 16);
            heading.Margin = new Thickness(0, 0, 0, 10);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            DockPanel.SetDock(heading, Dock.Top);
            body.Children.Add(heading);
            body.Children.Add(Ui.Card(code, 0));
            var copy = Ui.Button("Copy", () => Clipboard.SetText(text), glyph: Glyph.Copy);
            var close = Ui.Button("Close", () => w.Close(), primary: true);
            close.IsCancel = true;
            var buttons = Ui.Row(copy, close);
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            w.Content = Ui.WithFooter(body, buttons);
            w.ShowDialog();
        }

        private async System.Threading.Tasks.Task CreateDatabase()
        {
            _status.Text = "Reading the server's collations…";
            var server = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("collations", new JsonObject { ["source"] = WithPassword("master") }));
            _status.Text = "";
            if (server == null) return;
            var sql = Dialogs.NewDatabase(_session.Host.Owner, Side.Str("server"), _database.Text, server, out var name);
            if (sql == null) return;
            var result = await Ui.Try(_session.Host.Owner, () => _session.Engine.Request("execute", new JsonObject { ["source"] = WithPassword("master"), ["sql"] = sql }));
            if (result != null)
            {
                _database.Text = name;
                _status.Text = "Created database " + name + ".";
            }
        }

        /// <summary>Lists the folder's revisions in source control and puts the chosen one in the Revision box.</summary>
        private async System.Threading.Tasks.Task ChooseRevision()
        {
            var folder = _folder.Text.Trim();
            if (folder.Length == 0)
            {
                Ui.Error(_session.Host.Owner, "Choose the scripts folder first.");
                return;
            }
            var list = await Ui.Try(_session.Host.Owner, async () => await _session.Engine.Request("revisions", new JsonObject { ["path"] = folder }) as List<object>);
            if (list == null) return;
            var chosen = Dialogs.ChooseRevision(_session.Host.Owner, folder, list.OfType<JsonObject>().ToList());
            if (chosen != null) _revision.Text = chosen;
        }
    }
}
