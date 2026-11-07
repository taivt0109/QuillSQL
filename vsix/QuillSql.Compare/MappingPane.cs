using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;

namespace QuillSql.Compare
{
    /// <summary>
    /// The Table mapping and Owner mapping tabs as SQL Compare has them: the mapped pairs above
    /// (Unmap), what is left on each side below (Map one with one), Restore defaults, and a search
    /// box over both. The pairs come from the engine (`mapping_preview`), so they are what the
    /// comparison will use, default pairs and automatic column pairs included.
    /// </summary>
    internal sealed class MappingPane : DockPanel
    {
        public sealed class Pair
        {
            public string SourceOwner { get; set; }
            public string Source { get; set; }
            public string Target { get; set; }
            public string TargetOwner { get; set; }
            public string How { get; set; }
            public string Columns { get; set; }
            public JsonObject Data { get; set; }
        }

        public sealed class Item
        {
            public string Owner { get; set; }
            public string Name { get; set; }
            public string Full { get; set; }
            public override string ToString() => Full;
        }

        private readonly bool _tables;
        private readonly Func<Task<JsonObject>> _load;
        private readonly Action<string, string> _map;
        private readonly Action<string> _unmap;
        private readonly Action _restore;
        private readonly Action<JsonObject> _columns;
        private readonly DataGrid _mapped = new DataGrid { AutoGenerateColumns = false, CanUserAddRows = false, IsReadOnly = true, HeadersVisibility = DataGridHeadersVisibility.Column, SelectionMode = DataGridSelectionMode.Extended };
        private readonly ListBox _left = new ListBox { SelectionMode = SelectionMode.Single }, _right = new ListBox { SelectionMode = SelectionMode.Single };
        private readonly TextBox _find = Ui.SearchBox("Find", 220);
        private readonly TextBlock _status = Ui.Text("", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
        private readonly TextBlock _unmappedTitle = Ui.Text("", bold: true);
        private List<Pair> _pairs = new List<Pair>();
        private List<Item> _only1 = new List<Item>(), _only2 = new List<Item>();

        /// <param name="tables">Tables (else owners).</param>
        /// <param name="columns">Opens the column mapping of a mapped table; null for owners.</param>
        public MappingPane(bool tables, Func<Task<JsonObject>> load, Action<string, string> map, Action<string> unmap, Action restore, Action<JsonObject> columns)
        {
            _tables = tables;
            _load = load;
            _map = map;
            _unmap = unmap;
            _restore = restore;
            _columns = columns;
            var what = tables ? "Table" : "Owner";
            if (tables)
            {
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Owner", Binding = new Binding("SourceOwner"), Width = 110 });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Table", Binding = new Binding("Source"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "", Binding = new Binding("How"), Width = 90 });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Table", Binding = new Binding("Target"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Owner", Binding = new Binding("TargetOwner"), Width = 110 });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Columns", Binding = new Binding("Columns"), Width = 170 });
                _mapped.MouseDoubleClick += (s, e) => { if (_mapped.SelectedItem is Pair p) _columns?.Invoke(p.Data); };
            }
            else
            {
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Owner", Binding = new Binding("Source"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "", Binding = new Binding("How"), Width = 90 });
                _mapped.Columns.Add(new DataGridTextColumn { Header = "Owner", Binding = new Binding("Target"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            }
            var refresh = Ui.Button("Refresh", async () => await Reload(), glyph: Glyph.Refresh);
            var restoreButton = Ui.Button("Restore defaults", async () => { _restore(); await Reload(); });
            restoreButton.ToolTip = "Forget your mappings and pair everything by name again";
            var unmapButton = Ui.Button("Unmap", async () =>
            {
                var chosen = _mapped.SelectedItems.OfType<Pair>().ToList();
                if (chosen.Count == 0) return;
                foreach (var p in chosen) _unmap(p.Data.Str("key"));
                await Reload();
            }, glyph: Glyph.Delete);
            var mapButton = Ui.Button("Map", async () =>
            {
                if (!(_left.SelectedItem is Item a) || !(_right.SelectedItem is Item b))
                {
                    _status.Text = "Select one " + what.ToLowerInvariant() + " on each side to map them.";
                    return;
                }
                _map(a.Full, b.Full);
                await Reload();
            }, primary: true, glyph: Glyph.Add);
            var columnsButton = Ui.Button("Columns…", () => { if (_mapped.SelectedItem is Pair p) _columns?.Invoke(p.Data); }, glyph: Glyph.Edit);
            columnsButton.Visibility = tables ? Visibility.Visible : Visibility.Collapsed;
            _find.TextChanged += (s, e) => Show();
            var top = Ui.Row(_find, new Border { Width = 8 }, refresh, restoreButton);
            top.Margin = new Thickness(0, 0, 0, 8);
            var mappedTools = Ui.Row(unmapButton, columnsButton);
            mappedTools.Margin = new Thickness(-8, 6, 0, 10);
            var leftHead = Ui.Text("Unmapped in the source", brush: CompareTheme.Muted);
            var rightHead = Ui.Text("Unmapped in the target", brush: CompareTheme.Muted);
            var lists = new Grid();
            lists.ColumnDefinitions.Add(new ColumnDefinition());
            lists.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(12) });
            lists.ColumnDefinitions.Add(new ColumnDefinition());
            var leftPanel = new DockPanel();
            DockPanel.SetDock(leftHead, Dock.Top);
            leftPanel.Children.Add(leftHead);
            leftPanel.Children.Add(Ui.Card(_left, 2));
            var rightPanel = new DockPanel();
            DockPanel.SetDock(rightHead, Dock.Top);
            rightPanel.Children.Add(rightHead);
            rightPanel.Children.Add(Ui.Card(_right, 2));
            Grid.SetColumn(rightPanel, 2);
            lists.Children.Add(leftPanel);
            lists.Children.Add(rightPanel);
            var unmappedHead = new DockPanel { Margin = new Thickness(0, 4, 0, 6) };
            DockPanel.SetDock(mapButton, Dock.Right);
            unmappedHead.Children.Add(mapButton);
            unmappedHead.Children.Add(_unmappedTitle);
            var split = new Grid();
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1.3, GridUnitType.Star) });
            split.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            split.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
            var upper = new DockPanel();
            DockPanel.SetDock(mappedTools, Dock.Bottom);
            upper.Children.Add(mappedTools);
            upper.Children.Add(Ui.Card(_mapped, 4));
            var lower = new DockPanel();
            DockPanel.SetDock(unmappedHead, Dock.Top);
            lower.Children.Add(unmappedHead);
            lower.Children.Add(lists);
            Grid.SetRow(lower, 2);
            var splitter = new GridSplitter { Height = 8, HorizontalAlignment = HorizontalAlignment.Stretch, ResizeBehavior = GridResizeBehavior.PreviousAndNext };
            Grid.SetRow(splitter, 1);
            split.Children.Add(upper);
            split.Children.Add(splitter);
            split.Children.Add(lower);
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(_status, Dock.Bottom);
            Children.Add(top);
            Children.Add(_status);
            Children.Add(split);
            _status.Text = "Choose the data sources, then Refresh to see how " + (tables ? "tables" : "owners") + " pair.";
            Loaded += async (s, e) => { if (_pairs.Count == 0 && _only1.Count == 0 && _only2.Count == 0) await Reload(); };
        }

        private static string Q(JsonObject n) => n == null ? "" : "[" + n.Str("schema") + "].[" + n.Str("name") + "]";

        public async Task Reload()
        {
            _status.Text = "Reading the data sources…";
            var reply = await _load();
            if (reply == null)
            {
                _status.Text = "Could not read the data sources.";
                return;
            }
            var section = reply.Obj(_tables ? "tables" : "owners") ?? new JsonObject();
            _pairs = section.List("mapped").OfType<JsonObject>().Select(m =>
            {
                if (!_tables)
                {
                    m["key"] = m.Str("source");
                    return new Pair { Source = m.Str("source"), Target = m.Str("target"), How = m.Bool("user") ? "mapped" : "same name", Data = m };
                }
                var cols = m.List("columns").OfType<JsonObject>().ToList();
                var auto = cols.Count(c => c.Bool("auto"));
                var renamed = cols.Count(c => !c.Bool("auto") && !string.Equals(c.Str("source"), c.Str("target"), StringComparison.OrdinalIgnoreCase));
                m["key"] = Q(m.Obj("source"));
                return new Pair
                {
                    SourceOwner = m.Obj("source")?.Str("schema"),
                    Source = m.Obj("source")?.Str("name"),
                    Target = m.Obj("target")?.Str("name"),
                    TargetOwner = m.Obj("target")?.Str("schema"),
                    How = m.Bool("user") ? "mapped" : "same name",
                    Columns = Ui.Plural(m.List("sourceColumns").Count, "column", "columns") + (renamed > 0 ? ", " + renamed + " mapped" : "") + (auto > 0 ? ", " + auto + " auto-mapped" : ""),
                    Data = m,
                };
            }).ToList();
            Item item(object o) => o is JsonObject n ? new Item { Owner = n.Str("schema"), Name = n.Str("name"), Full = Q(n) } : new Item { Name = o as string, Full = o as string };
            _only1 = section.List("only1").Select(item).ToList();
            _only2 = section.List("only2").Select(item).ToList();
            _status.Text = Ui.Plural(_pairs.Count, _tables ? "table mapped" : "owner mapped", _tables ? "tables mapped" : "owners mapped") + ". Double-click a table to map its columns.";
            if (!_tables) _status.Text = Ui.Plural(_pairs.Count, "owner mapped", "owners mapped") + ".";
            Show();
        }

        private void Show()
        {
            var f = _find.Text.Trim();
            bool hit(params string[] s) => f.Length == 0 || s.Any(x => (x ?? "").IndexOf(f, StringComparison.OrdinalIgnoreCase) >= 0);
            _mapped.ItemsSource = _pairs.Where(p => hit(p.SourceOwner, p.Source, p.Target, p.TargetOwner)).ToList();
            _left.ItemsSource = _only1.Where(i => hit(i.Full)).ToList();
            _right.ItemsSource = _only2.Where(i => hit(i.Full)).ToList();
            var n = _only1.Count + _only2.Count;
            _unmappedTitle.Text = n == 0 ? "Everything is mapped" : Ui.Plural(n, _tables ? "table not mapped" : "owner not mapped", _tables ? "tables not mapped" : "owners not mapped");
        }
    }
}
