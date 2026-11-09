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
    /// The classic compare tool's filter: per object type (and for all types) an Include flag and an expression
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

        /// <param name="types">Object type labels shown in the results (Table, Stored procedure�).</param>
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
            var custom = new Button { Content = "Custom filter rules�", Style = CompareTheme.Style("Q.SubtleButton"), HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(-8, 6, 0, 0) };
            custom.Click += (s, e) => Edit("None");
            _excluded.Margin = new Thickness(0, 10, 0, 0);
            var note = Ui.Text("Objects the filter leaves out are not deployed.", 12, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            note.Margin = new Thickness(0, 6, 0, 0);
            Child = new ScrollViewer { Content = Ui.Column(head, _name, _list, custom, _excluded, note), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
            Fill();
        }

        /// <summary>The filter element name of a type label: "Stored procedure" ? StoredProcedure.</summary>
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

        private System.Threading.Tasks.Task OpenFile()
        {
            // Recently used filters first, as a menu; then the file dialog.
            var settings = CompareSettings.Load();
            var recent = settings.RecentFilters.Where(System.IO.File.Exists).ToList();
            if (recent.Count == 0 || !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("QUILL_TEST_DIALOGS"))) return Browse();
            var menu = new ContextMenu();
            CompareTheme.Attach(menu);
            foreach (var path in recent)
            {
                var item = Ui.MenuItem(System.IO.Path.GetFileName(path).Replace("_", "__"), async () => await Load(path), Glyph.Filter);
                item.ToolTip = path;
                menu.Items.Add(item);
            }
            menu.Items.Add(new Separator());
            menu.Items.Add(Ui.MenuItem("Browse…", async () => await Browse(), Glyph.Open));
            menu.IsOpen = true;
            return System.Threading.Tasks.Task.CompletedTask;
        }

        private async System.Threading.Tasks.Task Browse()
        {
            var folder = CompareSettings.Load().FilterFolder;
            var d = new OpenFileDialog { Filter = "Filter files (*.scpf)|*.scpf|All files (*.*)|*.*", InitialDirectory = System.IO.Directory.Exists(folder) ? folder : null };
            if (Dialogs.Show(d, _session.Host.Owner) != true) return;
            await Load(d.FileName);
        }

        private async System.Threading.Tasks.Task Load(string path)
        {
            var reply = await Ui.Try(_session.Host.Owner, () => _session.Engine.RequestObject("filter_load", new JsonObject { ["path"] = path }));
            if (reply == null) return;
            var settings = CompareSettings.Load();
            settings.AddRecent(settings.RecentFilters, path);
            SetFilter(reply.Obj("filter"));
            Raise();
        }

        private async System.Threading.Tasks.Task SaveFile()
        {
            var folder = CompareSettings.Load().FilterFolder;
            var d = new SaveFileDialog { Filter = "Filter files (*.scpf)|*.scpf|All files (*.*)|*.*", DefaultExt = "scpf", FileName = string.IsNullOrEmpty(_filterName) ? "" : _filterName + ".scpf", InitialDirectory = System.IO.Directory.Exists(folder) ? folder : null };
            if (Dialogs.Show(d, _session.Host.Owner) != true) return;
            var settings = CompareSettings.Load();
            settings.AddRecent(settings.RecentFilters, d.FileName);
            _filterName = System.IO.Path.GetFileNameWithoutExtension(d.FileName);
            var filter = Filter() ?? new JsonObject { ["rules"] = new List<object>() };
            filter["name"] = _filterName;
            var ok = await Ui.Try(_session.Host.Owner, async () => (object)await _session.Engine.Request("filter_save", new JsonObject { ["path"] = d.FileName, ["filter"] = filter }));
            if (ok != null) Fill();
        }
    }

    /// <summary>
    /// "Edit filter rules": include or exclude objects whose name or schema meets conditions, in
    /// groups of "all of" (AND) or "any of" (OR) that can hold further groups, as the classic compare tool's
    /// clause tree does.
    /// </summary>
    internal static class RuleEditor
    {
        /// <summary>A condition, or a group of conditions and groups.</summary>
        public sealed class Node
        {
            public bool IsGroup;
            public bool Any;
            public List<Node> Children = new List<Node>();
            public string Clause = "Object name";
            public string Operator = "Equals";
            public string Value = "";

            public static Node Group(bool any = false) => new Node { IsGroup = true, Any = any };
        }

        public static readonly string[] Clauses = { "Object name", "Schema name" };
        public static readonly string[] Operators = { "Equals", "Does not equal", "Begins with", "Does not begin with", "Ends with", "Does not end with", "Contains", "Does not contain", "LIKE", "NOT LIKE" };

        private static string Quote(string s) => "'" + (s ?? "").Replace("'", "''") + "'";
        private static string Like(string s) => (s ?? "").Replace("\\", "\\\\").Replace("%", "\\%").Replace("_", "\\_").Replace("[", "\\[");

        /// <summary>A condition in the filter language.</summary>
        public static string Text(Node c)
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
                    pattern = c.Operator.EndsWith("egin with", StringComparison.Ordinal) || c.Operator == "Begins with" ? pattern + "%"
                        : c.Operator.EndsWith("nd with", StringComparison.Ordinal) || c.Operator == "Ends with" ? "%" + pattern
                        : "%" + pattern + "%";
                    return v + (not ? " NOT LIKE " : " LIKE ") + Quote(pattern) + " ESCAPE '\\'";
            }
        }

        /// <summary>The expression of a group: its children joined by AND or OR, nested groups in parentheses.</summary>
        public static string Expression(Node group, bool top = true)
        {
            var parts = group.Children.Select(c => c.IsGroup ? (c.Children.Count == 0 ? null : c.Children.Count == 1 ? Expression(c, false) : "(" + Expression(c, false) + ")") : Text(c)).Where(p => p != null).ToList();
            if (parts.Count == 0) return top ? "TRUE" : null;
            return string.Join(group.Any ? " OR " : " AND ", parts);
        }

        // ---- Reading an expression back into the tree ----

        private static readonly Regex Token = new Regex(@"'(?:[^']|'')*'|\(|\)|!=|=|[@A-Za-z_][\w@]*|\S", RegexOptions.Compiled);

        private sealed class Reader
        {
            private readonly List<string> _t;
            private int _i;
            public Reader(string text) { _t = Token.Matches(text).Cast<Match>().Select(m => m.Value).ToList(); }
            private string Peek => _i < _t.Count ? _t[_i] : null;
            private bool Is(string word) => Peek != null && Peek.Equals(word, StringComparison.OrdinalIgnoreCase);
            public bool Done => _i >= _t.Count;

            /// <summary>or := and (OR and)*; and := term (AND term)*; term := ( or ) | condition.</summary>
            public Node Or()
            {
                var first = And();
                if (!Is("OR")) return first;
                var g = Node.Group(any: true);
                Add(g, first);
                while (Is("OR"))
                {
                    _i++;
                    Add(g, And());
                }
                return g;
            }

            private Node And()
            {
                var first = Term();
                if (!Is("AND")) return first;
                var g = Node.Group();
                Add(g, first);
                while (Is("AND"))
                {
                    _i++;
                    Add(g, Term());
                }
                return g;
            }

            /// <summary>A group of the same kind joins its parent; others nest.</summary>
            private static void Add(Node group, Node child)
            {
                if (child.IsGroup && child.Any == group.Any) group.Children.AddRange(child.Children);
                else group.Children.Add(child);
            }

            private Node Term()
            {
                if (Is("("))
                {
                    _i++;
                    var inner = Or();
                    if (!Is(")")) throw new FormatException("missing )");
                    _i++;
                    return inner;
                }
                var variable = Peek?.ToUpperInvariant();
                if (variable != "@NAME" && variable != "@SCHEMA") throw new FormatException("expected @NAME or @SCHEMA");
                _i++;
                string op;
                if (Is("NOT"))
                {
                    _i++;
                    if (!Is("LIKE")) throw new FormatException("expected LIKE");
                    op = "NOT LIKE";
                }
                else op = Peek?.ToUpperInvariant();
                _i++;
                if (op != "=" && op != "!=" && op != "LIKE" && op != "NOT LIKE") throw new FormatException("expected an operator");
                var literal = Peek;
                if (literal == null || !literal.StartsWith("'", StringComparison.Ordinal)) throw new FormatException("expected a string");
                _i++;
                var value = literal.Substring(1, literal.Length - 2).Replace("''", "'");
                var escaped = false;
                if (Is("ESCAPE"))
                {
                    _i++;
                    if (Peek != "'\\'") throw new FormatException("only ESCAPE '\\' can be edited as conditions");
                    _i++;
                    escaped = true;
                }
                var c = new Node { Clause = variable == "@SCHEMA" ? "Schema name" : "Object name" };
                var not = op == "!=" || op == "NOT LIKE";
                if (op == "=" || op == "!=")
                {
                    c.Operator = not ? "Does not equal" : "Equals";
                    c.Value = value;
                    return c;
                }
                if (escaped)
                {
                    // Begins with, ends with, contains: one wildcard at an end, the rest escaped.
                    var starts = value.StartsWith("%", StringComparison.Ordinal);
                    var ends = value.EndsWith("%", StringComparison.Ordinal) && !value.EndsWith("\\%", StringComparison.Ordinal);
                    var inner = value.Substring(starts ? 1 : 0, Math.Max(0, value.Length - (starts ? 1 : 0) - (ends ? 1 : 0)));
                    if ((starts || ends) && !Regex.IsMatch(inner, @"(?<!\\)[%_\[]"))
                    {
                        c.Value = Regex.Replace(inner, @"\\(.)", "$1");
                        c.Operator = starts && ends ? (not ? "Does not contain" : "Contains") : starts ? (not ? "Does not end with" : "Ends with") : (not ? "Does not begin with" : "Begins with");
                        return c;
                    }
                    throw new FormatException("a LIKE pattern with ESCAPE can only be edited as text");
                }
                c.Operator = not ? "NOT LIKE" : "LIKE";
                c.Value = value;
                return c;
            }
        }

        /// <summary>The tree of an expression; null when it uses what the tree cannot show (NOT, other ESCAPE characters).</summary>
        public static Node Parse(string expression)
        {
            var text = (expression ?? "").Trim();
            if (text.Length == 0 || text.Equals("TRUE", StringComparison.OrdinalIgnoreCase)) return Node.Group();
            try
            {
                var r = new Reader(text);
                var node = r.Or();
                if (!r.Done) return null;
                if (node.IsGroup) return node;
                var g = Node.Group();
                g.Children.Add(node);
                return g;
            }
            catch (FormatException)
            {
                return null;
            }
        }

        // ---- The window ----

        public static FilterPane.Rule Run(Window owner, string forWhat, FilterPane.Rule rule)
        {
            FilterPane.Rule result = null;
            var w = Ui.Dialog(owner, "Edit filter rules", null, 860, 620);
            var include = new RadioButton { Content = "Include if:", IsChecked = rule.Include, Margin = new Thickness(0, 0, 16, 0) };
            var exclude = new RadioButton { Content = "Exclude if:", IsChecked = !rule.Include };
            var root = Parse(rule.Expression == "TRUE" ? "" : rule.Expression);
            var parsed = root != null;
            root = root ?? Node.Group();
            var tree = new StackPanel();
            var expression = Ui.Code(readOnly: true);
            expression.Height = 80;
            expression.TextWrapping = TextWrapping.Wrap;
            expression.Text = rule.Expression ?? "TRUE";
            var manual = Ui.Check("Edit the expression directly", !parsed);
            void refresh()
            {
                if (manual.IsChecked != true) expression.Text = Expression(root);
            }
            void rebuild()
            {
                tree.Children.Clear();
                tree.Children.Add(GroupView(root, null, rebuild, refresh));
                refresh();
            }
            void mode()
            {
                var direct = manual.IsChecked == true;
                expression.IsReadOnly = !direct;
                tree.IsEnabled = !direct;
                refresh();
            }
            manual.Checked += (s, e) => mode();
            manual.Unchecked += (s, e) =>
            {
                var again = Parse(expression.Text);
                if (again == null)
                {
                    Ui.Error(w, "The expression uses what the conditions cannot show (NOT, or an ESCAPE character other than \\), so it can only be edited as text.");
                    manual.IsChecked = true;
                    return;
                }
                root = again;
                rebuild();
                mode();
            };
            rebuild();
            mode();
            var forRow = Ui.Row(Ui.Text("For: ", brush: CompareTheme.Muted), Ui.Text(forWhat, bold: true));
            var top = Ui.Column(forRow, Ui.Row(include, exclude));
            top.Margin = new Thickness(0, 0, 0, 8);
            var bottom = Ui.Column(manual, expression);
            bottom.Margin = new Thickness(0, 10, 0, 0);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(bottom, Dock.Bottom);
            body.Children.Add(top);
            body.Children.Add(bottom);
            body.Children.Add(Ui.Card(new ScrollViewer { Content = tree, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }, 10));
            var ok = Ui.Button("OK", () =>
            {
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

        /// <summary>A group: "all of" or "any of", its conditions and groups, and buttons to add more.</summary>
        private static UIElement GroupView(Node group, Node parent, Action rebuild, Action refresh)
        {
            var kind = new ComboBox { ItemsSource = new[] { "All of the following are true (AND)", "Any of the following is true (OR)" }, SelectedIndex = group.Any ? 1 : 0, Width = 280 };
            kind.SelectionChanged += (s, e) => { group.Any = kind.SelectedIndex == 1; refresh(); };
            var head = Ui.Row(kind,
                Ui.Command(Glyph.Add, "Condition", () => { group.Children.Add(new Node()); rebuild(); }, "Add a condition to this group"),
                Ui.Command(Glyph.List, "Group", () => { group.Children.Add(Node.Group(!group.Any)); rebuild(); }, "Add a group of conditions inside this group"));
            if (parent != null) head.Children.Add(Ui.IconButton(Glyph.Delete, "Remove this group", () => { parent.Children.Remove(group); rebuild(); }));
            var list = new StackPanel { Margin = new Thickness(parent == null ? 0 : 0, 2, 0, 2) };
            list.Children.Add(head);
            foreach (var child in group.Children.ToList())
            {
                UIElement view;
                if (child.IsGroup)
                {
                    view = GroupView(child, group, rebuild, refresh);
                }
                else
                {
                    var clause = new ComboBox { ItemsSource = Clauses, SelectedItem = child.Clause, Width = 140 };
                    clause.SelectionChanged += (s, e) => { child.Clause = (string)clause.SelectedItem; refresh(); };
                    var op = new ComboBox { ItemsSource = Operators, SelectedItem = child.Operator, Width = 170, Margin = new Thickness(6, 0, 6, 0) };
                    op.SelectionChanged += (s, e) => { child.Operator = (string)op.SelectedItem; refresh(); };
                    var value = new TextBox { Text = child.Value, Width = 240 };
                    value.TextChanged += (s, e) => { child.Value = value.Text; refresh(); };
                    System.Windows.Automation.AutomationProperties.SetName(value, "Value");
                    var remove = Ui.IconButton(Glyph.Delete, "Remove this condition", () => { group.Children.Remove(child); rebuild(); });
                    view = Ui.Row(clause, op, value, remove);
                }
                var indent = new Border { Child = view, Margin = new Thickness(24, 0, 0, 0), BorderThickness = new Thickness(2, 0, 0, 0), Padding = new Thickness(10, 0, 0, 0) };
                indent.SetResourceReference(Border.BorderBrushProperty, "Q.Divider");
                list.Children.Add(indent);
            }
            if (group.Children.Count == 0)
            {
                var empty = Ui.Text(parent == null ? "No conditions: every object matches." : "An empty group is left out.", 12, brush: CompareTheme.Muted);
                empty.Margin = new Thickness(24, 2, 0, 2);
                list.Children.Add(empty);
            }
            return list;
        }
    }
}
