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

            var tabs = new TabControl { Style = CompareTheme.Style("Q.NavTabs"), Margin = new Thickness(4, 0, 0, 0) };
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Connect, "Data sources", 12), Content = DataSources() });
            if (_data)
            {
                tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Tables & views", 12), Content = TablesAndViews() });
            }
            else
            {
                tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Table mapping", 12), Content = TableMapping() });
            }
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.People, "Owner mapping", 12), Content = OwnerMapping() });
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Settings, "Options", 12), Content = Options() });
            Children.Add(tabs);
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
            var mappings = _project.Obj("schemaMappings") ?? new JsonObject();
            foreach (var t in mappings.List("tables").OfType<JsonObject>())
            {
                var cols = t.List("columns").OfType<List<object>>().Select(p => p.Count == 2 ? p[0] + "=" + p[1] : "");
                _tableMappings.Add(new TableMappingRow { Source = QName(t.Obj("source")), Target = QName(t.Obj("target")), Columns = string.Join(", ", cols) });
            }
            const string help = "Tables are paired by schema and name. Map a table to a differently named one here; columns pair by name, and similar columns are paired automatically unless that option is off. Write column pairs as old=new, separated by commas.";
            var detect = Ui.Button("Detect renames", async () => await DetectRenames(), minWidth: 120, glyph: Glyph.Search);
            detect.ToolTip = "Find tables only in one database that look like a renamed table of the other";
            return MappingGrid("Table mapping", help, _tableMappings, new[] { ("Table in source", "Source", 220.0), ("Table in target", "Target", 220.0), ("Column pairs", "Columns", 300.0) },
                () => _tableMappings.Add(new TableMappingRow { Source = "[dbo].[]", Target = "[dbo].[]" }), detect);
        }

        /// <summary>Reads both databases and offers the tables that look renamed as mappings.</summary>
        private async System.Threading.Tasks.Task DetectRenames()
        {
            Collect();
            var renames = await Ui.Try(_session.Host.Owner, async () => await _session.Engine.Request("schema_renames", new JsonObject { ["project"] = _session.WithPasswords() }) as List<object>);
            if (renames == null) return;
            if (renames.Count == 0)
            {
                Ui.Message(_session.Host.Owner, "Table mapping", "No tables look renamed.", Ui.Tone.Info, "OK");
                return;
            }
            var chosen = Dialogs.ConfirmRenames(_session.Host.Owner, renames.OfType<JsonObject>().ToList());
            if (chosen == null) return;
            foreach (var r in chosen)
            {
                var row = new TableMappingRow { Source = QName(r.Obj("source")), Target = QName(r.Obj("target")) };
                if (!_tableMappings.Any(m => string.Equals(m.Source, row.Source, StringComparison.OrdinalIgnoreCase))) _tableMappings.Add(row);
            }
        }

        private UIElement OwnerMapping()
        {
            var mappings = _project.Obj("schemaMappings") ?? new JsonObject();
            foreach (var pair in mappings.List("schemas").OfType<List<object>>())
            {
                if (pair.Count == 2) _ownerMappings.Add(new OwnerMappingRow { Source = pair[0] as string, Target = pair[1] as string });
            }
            const string help = "Objects are paired with objects of the same schema. Map a schema of the source to a different schema of the target here.";
            return MappingGrid("Owner mapping", help, _ownerMappings, new[] { ("Schema in source", "Source", 220.0), ("Schema in target", "Target", 220.0) },
                () => _ownerMappings.Add(new OwnerMappingRow { Source = "dbo", Target = "dbo" }));
        }

        private static UIElement MappingGrid<T>(string title, string help, ObservableCollection<T> rows, (string, string, double)[] columns, Action add, UIElement extra = null)
        {
            var grid = new DataGrid
            {
                ItemsSource = rows,
                AutoGenerateColumns = false,
                CanUserAddRows = false,
            };
            foreach (var (header, path, width) in columns)
            {
                grid.Columns.Add(new DataGridTextColumn { Header = header, Binding = new Binding(path) { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged }, Width = width });
            }
            var buttons = Ui.Row(
                Ui.Button("Add", add, glyph: Glyph.Add),
                Ui.Button("Remove", () => { if (grid.SelectedItem is T item) rows.Remove(item); }, glyph: Glyph.Delete));
            if (extra != null) buttons.Children.Insert(0, extra);
            return Page(title, help, grid, buttons);
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
            var grid = new DataGrid
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
            var content = new DockPanel();
            DockPanel.SetDock(_dataStatus, Dock.Top);
            content.Children.Add(_dataStatus);
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
                var deselected = _project.List("deselected").OfType<JsonObject>().Any(d => "[" + d.Str("schema") + "].[" + d.Str("name") + "]" == source);
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
                    // Unlike SQL Compare, flags without a check box are kept.
                    result.Add(name);
                }
            }
            return result;
        }

        /// <summary>Writes every tab back into the project.</summary>
        public void Collect()
        {
            _source?.Save();
            _target?.Save();
            var defs = _session.OptionDefinitions.List(_data ? "data" : "schema").OfType<JsonObject>().ToList();
            if (_optionBoxes.Count > 0) _project["options"] = Selected(defs);

            var schemaMappings = _project.Obj("schemaMappings") ?? new JsonObject();
            schemaMappings["schemas"] = _ownerMappings.Where(m => !string.IsNullOrWhiteSpace(m.Source) && !string.IsNullOrWhiteSpace(m.Target))
                .Select(m => (object)new List<object> { m.Source.Trim(), m.Target.Trim() }).ToList();
            if (!_data)
            {
                schemaMappings["tables"] = _tableMappings.Where(m => !string.IsNullOrWhiteSpace(m.Source) && !string.IsNullOrWhiteSpace(m.Target))
                    .Select(m => (object)new JsonObject
                    {
                        ["source"] = ParseName(m.Source),
                        ["target"] = ParseName(m.Target),
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
                    if (!string.Equals(r.Source, r.Target, StringComparison.OrdinalIgnoreCase)) o["target"] = ParseName(r.Target);
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
                _project["dataMappings"] = dm;
                _project["deselected"] = deselected;
            }
        }
    }

    /// <summary>One side of the Data sources tab.</summary>
    internal sealed class SourceEditor : StackPanel
    {
        private readonly CompareSession _session;
        private readonly string _key;
        private ComboBox _type, _server, _auth, _database;
        private TextBox _user, _snapshot, _folder, _revision;
        private PasswordBox _password;
        private CheckBox _remember, _encrypt, _trust;
        private StackPanel _live, _file, _scripts, _revisionField;
        private TextBlock _empty;
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
            // SQL Compare's other kinds of data source are listed but not available yet.
            foreach (var (name, enabled) in new[] { ("Database", true), ("Snapshot", !session.IsData), ("Empty database", !session.IsData && key == "target"), ("Backup", false), ("Scripts folder", !session.IsData), ("Source control", !session.IsData) })
            {
                _type.Items.Add(new ComboBoxItem { Content = name, IsEnabled = enabled, ToolTip = enabled ? null : name == "Snapshot" || name == "Empty database" || ((name == "Scripts folder" || name == "Source control") && session.IsData) ? "Not available for this side or kind of comparison" : "Not available yet" });
            }
            _type.SelectionChanged += (s, e) => ShowType();
            var header = new DockPanel { Margin = new Thickness(0, 0, 0, 18) };
            DockPanel.SetDock(badge, Dock.Left);
            DockPanel.SetDock(_type, Dock.Right);
            header.Children.Add(badge);
            header.Children.Add(_type);
            header.Children.Add(names);
            Children.Add(header);

            _server = new ComboBox { IsEditable = true, ItemsSource = CompareSettings.Load().RecentServers };
            _auth = new ComboBox { ItemsSource = new[] { "Windows authentication", "SQL Server authentication", "Microsoft Entra (access token)" } };
            _auth.SelectionChanged += (s, e) => { var sql = _auth.SelectedIndex == 1; _user.IsEnabled = sql; _password.IsEnabled = sql; _remember.IsEnabled = sql; };
            _user = new TextBox();
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
                Ui.Field(null, Ui.Row(_encrypt, _trust)),
                Ui.Field("Database", dbRow),
                Ui.Field(null, Ui.Row(test, create))), _status);
            _snapshot = new TextBox();
            var browse = Ui.Button("Browse…", glyph: Glyph.Open, click: () =>
            {
                var d = new OpenFileDialog { Filter = "QuillSQL snapshots (*.json)|*.json|All files|*.*" };
                if (Dialogs.Show(d, null) == true) _snapshot.Text = d.FileName;
            });
            var fileRow = new DockPanel();
            DockPanel.SetDock(browse, Dock.Right);
            fileRow.Children.Add(browse);
            fileRow.Children.Add(_snapshot);
            _file = Ui.Column(Ui.Form(Ui.Field("Snapshot file", fileRow)), Ui.Text("Make a snapshot with quill schema /makesnapshot or Tools > Save snapshot.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            _empty = Ui.Text("An empty database: every object of the source is created. The changes can only be scripted, and the script can then run on a new database.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            Children.Add(_live);
            // A scripts folder in SQL Compare's layout, or a revision of it in git, Mercurial or Subversion.
            _folder = new TextBox();
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
            _revisionField = Ui.Form(Ui.Field("Revision", _revision));
            _scripts = Ui.Column(Ui.Form(Ui.Field("Scripts folder (in a git, Mercurial or Subversion working copy for source control)", folderRow)), _revisionField,
                Ui.Text("Deploying to a scripts folder rewrites the files of the objects deployed; a revision can only be compared or scripted.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            Children.Add(_file);
            Children.Add(_empty);
            Children.Add(_scripts);
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
            _revisionField.Visibility = control ? Visibility.Visible : Visibility.Collapsed;
            if (control && _revision.Text.Trim().Length == 0) _revision.Text = "HEAD";
            _live.Visibility = snapshot || empty || scripts ? Visibility.Collapsed : Visibility.Visible;
            _scripts.Visibility = scripts ? Visibility.Visible : Visibility.Collapsed;
            _file.Visibility = snapshot ? Visibility.Visible : Visibility.Collapsed;
            _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        }

        public void Load(JsonObject side, string password)
        {
            Side = side;
            _type.SelectedIndex = side.Str("type") == "snapshot" ? 1 : side.Str("type") == "empty" ? 2 : side.Str("type") == "scripts" ? (string.IsNullOrEmpty(side.Str("revision")) ? 4 : 5) : 0;
            _server.Text = side.Str("server") ?? "";
            var auth = side.Str("authentication");
            _auth.SelectedIndex = auth == "sql" ? 1 : auth == "entra" ? 2 : 0;
            _user.Text = side.Str("user") ?? "";
            _remember.IsChecked = side.Bool("rememberPassword");
            _password.Password = password ?? (side.Bool("rememberPassword") ? Credentials.Read(side.Str("server"), side.Str("user")) ?? "" : "");
            _encrypt.IsChecked = !side.ContainsKey("encrypt") || side.Bool("encrypt");
            _trust.IsChecked = side.Bool("trustServerCertificate");
            _database.Text = side.Str("database") ?? "";
            _snapshot.Text = side.Str("type") == "snapshot" ? side.Str("path") ?? "" : "";
            _folder.Text = side.Str("type") == "scripts" ? side.Str("path") ?? "" : "";
            _revision.Text = side.Str("revision") ?? "";
            ShowType();
        }

        public void Save()
        {
            Side["type"] = _type.SelectedIndex == 1 ? "snapshot" : _type.SelectedIndex == 2 ? "empty" : _type.SelectedIndex >= 4 ? "scripts" : "database";
            Side["server"] = _server.Text.Trim();
            Side["authentication"] = _auth.SelectedIndex == 1 ? "sql" : _auth.SelectedIndex == 2 ? "entra" : "windows";
            Side["user"] = _user.Text.Trim();
            Side["rememberPassword"] = _remember.IsChecked == true;
            Side["encrypt"] = _encrypt.IsChecked == true;
            Side["trustServerCertificate"] = _trust.IsChecked == true;
            Side["database"] = _database.Text.Trim();
            Side["path"] = _type.SelectedIndex >= 4 ? _folder.Text.Trim() : _snapshot.Text.Trim();
            if (_type.SelectedIndex == 5 && _revision.Text.Trim().Length > 0) Side["revision"] = _revision.Text.Trim();
            else Side.Remove("revision");
            _session.Project[_key] = Side;
            _session.Passwords[_key] = _password.Password;
            if (_auth.SelectedIndex == 1 && _remember.IsChecked == true && _password.Password.Length > 0)
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
            var result = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("test_connection", new JsonObject { ["source"] = WithPassword() }));
            _status.Text = result == null ? "" : "Connected: SQL Server " + result.Str("version") + ", database " + result.Str("database") + ".";
        }

        private async System.Threading.Tasks.Task CreateDatabase()
        {
            var name = Ui.Prompt(_session.Host.Owner, "New database", "Name of the new, empty database on " + Side.Str("server"), _database.Text);
            if (string.IsNullOrWhiteSpace(name)) return;
            var sql = "CREATE DATABASE [" + name.Replace("]", "]]") + "]";
            var result = await Ui.Try(_session.Host.Owner, () => _session.Engine.Request("execute", new JsonObject { ["source"] = WithPassword("master"), ["sql"] = sql }));
            if (result != null)
            {
                _database.Text = name;
                _status.Text = "Created database " + name + ".";
            }
        }
    }
}
