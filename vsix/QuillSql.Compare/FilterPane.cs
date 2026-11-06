using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>
    /// SQL Compare's filter: per object type (and for all types) an Include flag and an expression
    /// over @NAME and @SCHEMA. The pane beside the results ticks types in and out, edits each
    /// type's rule, and opens and saves .scpf files; the engine applies it (`schema_filter`).
    /// </summary>
    internal sealed class FilterPane : Border
    {
        public sealed class Rule
        {
            public string Type;
            public bool Include = true;
            /// <summary>null leaves the type out; TRUE takes every object.</summary>
            public string Expression = "TRUE";

            public bool IsAll => Include && Expression == "TRUE";
        }

        private readonly CompareSession _session;
        private readonly List<string> _types;
        private readonly Dictionary<string, Rule> _rules = new Dictionary<string, Rule>();
        private readonly StackPanel _list = new StackPanel();
        private readonly TextBlock _name = Ui.Text("", brush: CompareTheme.Muted);
        private readonly TextBlock _excluded = Ui.Text("", 12, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
        private string _filterName = "";
        private bool _caseSensitive;

        /// <summary>Raised with the filter (null: nothing excluded) after every change.</summary>
        public event Action<JsonObject> Changed;

        /// <param name="types">Object type labels shown in the results (Table, Stored procedure…).</param>
        public FilterPane(CompareSession session, IEnumerable<string> types)
        {
            _session = session;
            _types = types.ToList();
            Width = 260;
            Padding = new Thickness(12, 10, 12, 10);
            BorderThickness = new Thickness(0, 0, 1, 0);
            SetResourceReference(BorderBrushProperty, "Q.Divider");
            var title = Ui.Text("Filter", 15, bold: true);
            var tools = Ui.Row(
                Ui.IconButton(Glyph.Open, "Open an existing filter", async () => await OpenFile()),
                Ui.IconButton(Glyph.Save, "Save the current filter", async () => await SaveFile()),
                Ui.IconButton(Glyph.Delete, "Clear the filter (nothing excluded)", () => { _rules.Clear(); _filterName = ""; Fill(); Raise(); }));
            var head = new DockPanel();
            DockPanel.SetDock(tools, Dock.Right);
            head.Children.Add(tools);
            head.Children.Add(title);
            var custom = new Button { Content = "Custom filter rules…", Style = CompareTheme.Style("Q.SubtleButton"), HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(-8, 6, 0, 0) };
            custom.Click += (s, e) => Edit("None");
            _excluded.Margin = new Thickness(0, 10, 0, 0);
            var note = Ui.Text("Objects the filter leaves out are not deployed.", 12, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            note.Margin = new Thickness(0, 6, 0, 0);
            Child = new ScrollViewer { Content = Ui.Column(head, _name, _list, custom, _excluded, note), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
            Fill();
        }

        /// <summary>The filter element name of a type label: "Stored procedure" → StoredProcedure.</summary>
        public static string Key(string label) =>
            string.Concat((label ?? "").Split(new[] { ' ', '-' }, StringSplitOptions.RemoveEmptyEntries).Select(w => char.ToUpperInvariant(w[0]) + w.Substring(1)));

        private Rule RuleOf(string key) => _rules.TryGetValue(key, out var r) ? r : new Rule { Type = key };

        /// <summary>Takes a filter as the engine describes it (`filter_load`).</summary>
        public void SetFilter(JsonObject filter)
        {
            _rules.Clear();
            _filterName = filter?.Str("name") ?? "";
            _caseSensitive = filter?.Bool("caseSensitive") ?? false;
            foreach (var r in (filter?.List("rules") ?? new List<object>()).OfType<JsonObject>())
            {
                _rules[r.Str("type")] = new Rule { Type = r.Str("type"), Include = r.Bool("include"), Expression = r.Str("expression") };
            }
            Fill();
        }

        public JsonObject Filter()
        {
            var rules = _rules.Values.Where(r => !r.IsAll).ToList();
            if (rules.Count == 0) return null;
            return new JsonObject
            {
                ["name"] = _filterName,
                ["caseSensitive"] = _caseSensitive,
                ["rules"] = rules.Select(r => (object)new JsonObject { ["type"] = r.Type, ["include"] = r.Include, ["expression"] = r.Expression }).ToList(),
            };
        }

        public void ShowExcluded(int count) => _excluded.Text = "Objects excluded by this filter: " + count;

        private void Raise() => Changed?.Invoke(Filter());

        private static string Describe(Rule r) =>
            r.Expression == null ? "Excluded" : r.IsAll ? "" : (r.Include ? "Include if (" : "Exclude if (") + r.Expression + ")";

        private void Fill()
        {
            _list.Children.Clear();
            _name.Text = Filter() == null ? "Nothing excluded" : string.IsNullOrEmpty(_filterName) ? "Custom" : _filterName;
            _name.Margin = new Thickness(0, 2, 0, 8);
            var allTypes = _types.Select(Key).ToList();
            var included = allTypes.Count(k => RuleOf(k).Expression != null);
            var all = new CheckBox { Content = "All object types included", FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 2, 0, 2), IsThreeState = false };
            all.IsChecked = included == allTypes.Count && RuleOf("None").IsAll ? true : included == 0 ? false : (bool?)null;
            // Checked and Unchecked, not Click, so the keyboard and UI Automation work too.
            void setAll(bool on)
            {
                foreach (var k in allTypes) _rules[k] = new Rule { Type = k, Expression = on ? "TRUE" : null };
                Dispatcher.BeginInvoke(new Action(() => { Fill(); Raise(); }));
            }
            all.Checked += (s, e) => setAll(true);
            all.Unchecked += (s, e) => setAll(false);
            _list.Children.Add(all);
            var allRule = Describe(RuleOf("None"));
            if (allRule.Length > 0) _list.Children.Add(RuleText(allRule));
            foreach (var label in _types)
            {
                var key = Key(label);
                var rule = RuleOf(key);
                var box = new CheckBox { Content = label, Margin = new Thickness(0, 2, 0, 2), IsChecked = rule.IsAll ? true : rule.Expression == null ? false : (bool?)null };
                void set(bool on)
                {
                    _rules[key] = new Rule { Type = key, Expression = on ? "TRUE" : null };
                    Dispatcher.BeginInvoke(new Action(() => { Fill(); Raise(); }));
                }
                box.Checked += (s, e) => set(true);
                box.Unchecked += (s, e) => set(false);
                var edit = Ui.IconButton(Glyph.Edit, "Edit the filter rule for " + label, () => Edit(key, label));
                edit.Height = 26;
                var clear = Ui.IconButton(Glyph.Delete, "Clear the filter rule for " + label, () => { _rules.Remove(key); Fill(); Raise(); });
                clear.Height = 26;
                clear.Visibility = rule.IsAll ? Visibility.Collapsed : Visibility.Visible;
                var row = new DockPanel();
                var buttons = Ui.Row(edit, clear);
                buttons.Margin = new Thickness(0);
                DockPanel.SetDock(buttons, Dock.Right);
                row.Children.Add(buttons);
                row.Children.Add(box);
                _list.Children.Add(row);
                var text = Describe(rule);
                if (text.Length > 0 && text != "Excluded") _list.Children.Add(RuleText(text));
            }
        }

        private static TextBlock RuleText(string text)
        {
            var t = Ui.Text(text, 12, brush: CompareTheme.High, wrap: TextWrapping.Wrap);
            t.Margin = new Thickness(24, 0, 0, 4);
            return t;
        }

        private void Edit(string key, string label = null)
        {
            var rule = RuleOf(key);
            var edited = RuleEditor.Run(_session.Host.Owner, key == "None" ? "All object types in all schemas" : label, rule);
            if (edited == null) return;
            _rules[key] = edited;
            Fill();
            Raise();
        }

        private async System.Threading.Tasks.Task OpenFile()
        {
            var d = new OpenFileDialog { Filter = "Filter files (*.scpf)|*.scpf|All files (*.*)|*.*" };
            if (Dialogs.Show(d, _session.Host.Owner) != true) return;
            var reply = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("filter_load", new JsonObject { ["path"] = d.FileName }));
            if (reply == null) return;
            SetFilter(reply.Obj("filter"));
            Raise();
        }

        private async System.Threading.Tasks.Task SaveFile()
        {
            var d = new SaveFileDialog { Filter = "Filter files (*.scpf)|*.scpf|All files (*.*)|*.*", DefaultExt = "scpf", FileName = string.IsNullOrEmpty(_filterName) ? "" : _filterName + ".scpf" };
            if (Dialogs.Show(d, _session.Host.Owner) != true) return;
            _filterName = System.IO.Path.GetFileNameWithoutExtension(d.FileName);
            var filter = Filter() ?? new JsonObject { ["rules"] = new List<object>() };
            filter["name"] = _filterName;
            var ok = await Ui.Try(_session.Host.Owner, async () => (object)await _session.Engine.Request("filter_save", new JsonObject { ["path"] = d.FileName, ["filter"] = filter }));
            if (ok != null) Fill();
        }
    }

    /// <summary>"Edit filter rules": include or exclude objects whose name or schema meets conditions.</summary>
    internal static class RuleEditor
    {
        public sealed class Condition
        {
            public string Clause { get; set; } = "Object name";
            public string Operator { get; set; } = "Equals";
            public string Value { get; set; } = "";
        }

        public static readonly string[] Clauses = { "Object name", "Schema name" };
        public static readonly string[] Operators = { "Equals", "Does not equal", "Begins with", "Does not begin with", "Ends with", "Does not end with", "Contains", "Does not contain", "LIKE", "NOT LIKE" };

        private static string Quote(string s) => "'" + (s ?? "").Replace("'", "''") + "'";
        private static string Like(string s) => (s ?? "").Replace("\\", "\\\\").Replace("%", "\\%").Replace("_", "\\_").Replace("[", "\\[");

        /// <summary>One condition in the filter language.</summary>
        public static string Text(Condition c)
        {
            var v = c.Clause == "Schema name" ? "@SCHEMA" : "@NAME";
            var not = c.Operator.StartsWith("Does not", StringComparison.Ordinal) || c.Operator == "NOT LIKE";
            switch (c.Operator)
            {
                case "Equals":
                case "Does not equal":
                    return v + (not ? " != " : " = ") + Quote(c.Value);
                case "LIKE":
                case "NOT LIKE":
                    return v + (not ? " NOT LIKE " : " LIKE ") + Quote(c.Value);
                default:
                    var pattern = Like(c.Value);
                    pattern = c.Operator.EndsWith("begin with", StringComparison.Ordinal) || c.Operator == "Begins with" ? pattern + "%"
                        : c.Operator.EndsWith("end with", StringComparison.Ordinal) || c.Operator == "Ends with" ? "%" + pattern
                        : "%" + pattern + "%";
                    return v + (not ? " NOT LIKE " : " LIKE ") + Quote(pattern) + " ESCAPE '\\'";
            }
        }

        private static readonly Regex One = new Regex(@"^\s*(@NAME|@SCHEMA)\s*(=|!=|NOT\s+LIKE|LIKE)\s*'((?:[^']|'')*)'(\s+ESCAPE\s+'\\')?\s*$", RegexOptions.IgnoreCase);

        /// <summary>Splits an expression of conditions joined all by AND or all by OR; null when it is anything else.</summary>
        public static List<Condition> Parse(string expression, out bool any)
        {
            any = false;
            var text = (expression ?? "").Trim();
            while (text.StartsWith("(", StringComparison.Ordinal) && text.EndsWith(")", StringComparison.Ordinal) && Balanced(text.Substring(1, text.Length - 2))) text = text.Substring(1, text.Length - 2).Trim();
            if (text.Length == 0 || text == "TRUE") return new List<Condition>();
            var parts = new List<string>();
            var joins = new HashSet<string>();
            var cur = new StringBuilder();
            var quoted = false;
            var words = Regex.Split(text, @"(\s+|')");
            for (var i = 0; i < words.Length; i++)
            {
                var w = words[i];
                if (w == "'") quoted = !quoted;
                if (!quoted && (w.Equals("AND", StringComparison.OrdinalIgnoreCase) || w.Equals("OR", StringComparison.OrdinalIgnoreCase)))
                {
                    joins.Add(w.ToUpperInvariant());
                    parts.Add(cur.ToString());
                    cur.Clear();
                    continue;
                }
                cur.Append(w);
            }
            parts.Add(cur.ToString());
            if (joins.Count > 1) return null;
            any = joins.Contains("OR");
            var list = new List<Condition>();
            foreach (var p in parts)
            {
                var m = One.Match(p);
                if (!m.Success) return null;
                var clause = m.Groups[1].Value.ToUpperInvariant() == "@SCHEMA" ? "Schema name" : "Object name";
                var op = Regex.Replace(m.Groups[2].Value.ToUpperInvariant(), @"\s+", " ");
                var value = m.Groups[3].Value.Replace("''", "'");
                var not = op == "!=" || op == "NOT LIKE";
                if (op == "=" || op == "!=")
                {
                    list.Add(new Condition { Clause = clause, Operator = not ? "Does not equal" : "Equals", Value = value });
                    continue;
                }
                if (m.Groups[4].Success)
                {
                    // Begins with, ends with, contains: one wildcard at an end, the rest escaped.
                    var starts = value.StartsWith("%", StringComparison.Ordinal);
                    var ends = value.EndsWith("%", StringComparison.Ordinal) && !value.EndsWith("\\%", StringComparison.Ordinal);
                    var inner = value.Substring(starts ? 1 : 0, value.Length - (starts ? 1 : 0) - (ends ? 1 : 0));
                    if ((starts || ends) && !Regex.IsMatch(inner, @"(?<!\\)[%_\[]"))
                    {
                        var plain = Regex.Replace(inner, @"\\(.)", "$1");
                        var kind = starts && ends ? "ontain" : starts ? "end with" : "begin with";
                        var name = kind == "ontain" ? (not ? "Does not contain" : "Contains") : kind == "end with" ? (not ? "Does not end with" : "Ends with") : (not ? "Does not begin with" : "Begins with");
                        list.Add(new Condition { Clause = clause, Operator = name, Value = plain });
                        continue;
                    }
                    return null;
                }
                list.Add(new Condition { Clause = clause, Operator = not ? "NOT LIKE" : "LIKE", Value = value });
            }
            return list;
        }

        private static bool Balanced(string s)
        {
            var depth = 0;
            var quoted = false;
            foreach (var ch in s)
            {
                if (ch == '\'') quoted = !quoted;
                if (quoted) continue;
                if (ch == '(') depth++;
                if (ch == ')' && --depth < 0) return false;
            }
            return depth == 0;
        }

        public static FilterPane.Rule Run(Window owner, string forWhat, FilterPane.Rule rule)
        {
            FilterPane.Rule result = null;
            var w = Ui.Dialog(owner, "Edit filter rules", null, 760, 560);
            var include = new RadioButton { Content = "Include if:", IsChecked = rule.Include, Margin = new Thickness(0, 0, 16, 0) };
            var exclude = new RadioButton { Content = "Exclude if:", IsChecked = !rule.Include };
            var conditions = new ObservableCollection<Condition>(Parse(rule.Expression == "TRUE" ? "" : rule.Expression, out var any) ?? new List<Condition>());
            var parsed = Parse(rule.Expression == "TRUE" ? "" : rule.Expression, out _) != null;
            var match = new ComboBox { ItemsSource = new[] { "All conditions (AND)", "Any condition (OR)" }, SelectedIndex = any ? 1 : 0, Width = 200 };
            var grid = new DataGrid { ItemsSource = conditions, AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column };
            DataGridComboBoxColumn combo(string header, string path, string[] items, double width) => new DataGridComboBoxColumn
            {
                Header = header, ItemsSource = items, Width = width,
                SelectedItemBinding = new Binding(path) { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged },
            };
            grid.Columns.Add(combo("Clause", "Clause", Clauses, 150));
            grid.Columns.Add(combo("Operator", "Operator", Operators, 180));
            grid.Columns.Add(new DataGridTextColumn { Header = "Value", Binding = new Binding("Value") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged }, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            var expression = Ui.Code(readOnly: true);
            expression.Height = 70;
            expression.TextWrapping = TextWrapping.Wrap;
            var manual = Ui.Check("Edit the expression directly", !parsed);
            string build() => conditions.Count == 0 ? "TRUE" : string.Join(match.SelectedIndex == 1 ? " OR " : " AND ", conditions.Select(Text));
            void refresh()
            {
                if (manual.IsChecked != true) expression.Text = build();
            }
            expression.Text = rule.Expression ?? "TRUE";
            void mode()
            {
                var direct = manual.IsChecked == true;
                expression.IsReadOnly = !direct;
                grid.IsEnabled = match.IsEnabled = !direct;
                refresh();
            }
            manual.Checked += (s, e) => mode();
            manual.Unchecked += (s, e) =>
            {
                var again = Parse(expression.Text, out var anyAgain);
                if (again == null)
                {
                    Ui.Error(w, "The expression has more than conditions joined by AND or by OR, so it can only be edited as text.");
                    manual.IsChecked = true;
                    return;
                }
                conditions.Clear();
                foreach (var c in again) conditions.Add(c);
                match.SelectedIndex = anyAgain ? 1 : 0;
                mode();
            };
            grid.CellEditEnding += (s, e) => w.Dispatcher.BeginInvoke(new Action(refresh));
            grid.CurrentCellChanged += (s, e) => refresh();
            match.SelectionChanged += (s, e) => refresh();
            mode();
            var add = Ui.Button("Add condition", () => { conditions.Add(new Condition()); refresh(); }, glyph: Glyph.Add);
            var remove = Ui.Button("Remove", () => { if (grid.SelectedItem is Condition c) { conditions.Remove(c); refresh(); } }, glyph: Glyph.Delete);
            var matchLabel = Ui.Text("Match", brush: CompareTheme.Muted);
            matchLabel.Margin = new Thickness(0, 0, 8, 0);
            var top = Ui.Column(Ui.Row(Ui.Text("For: ", brush: CompareTheme.Muted), Ui.Text(forWhat, bold: true)), Ui.Row(include, exclude), Ui.Row(matchLabel, match, add, remove));
            top.Margin = new Thickness(0, 0, 0, 8);
            var bottom = Ui.Column(manual, expression);
            bottom.Margin = new Thickness(0, 10, 0, 0);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(bottom, Dock.Bottom);
            body.Children.Add(top);
            body.Children.Add(bottom);
            body.Children.Add(Ui.Card(grid, 4));
            var ok = Ui.Button("OK", () =>
            {
                grid.CommitEdit(DataGridEditingUnit.Row, true);
                refresh();
                var text = expression.Text.Trim();
                result = new FilterPane.Rule { Type = rule.Type, Include = include.IsChecked == true, Expression = text.Length == 0 ? "TRUE" : text };
                w.Close();
            }, primary: true, minWidth: 100);
            ok.IsDefault = true;
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            var buttons = Ui.Row(ok, cancel);
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            w.Content = Ui.WithFooter(body, buttons);
            w.ShowDialog();
            return result;
        }
    }
}
