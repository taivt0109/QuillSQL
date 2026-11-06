using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>
    /// The deployment wizard: how to deploy, the dependencies that come along (schema), then the
    /// summary, warnings and script to review before deploying or opening the script.
    /// </summary>
    internal sealed class DeployWizard
    {
        /// <summary>One line of the summary: an object (or table), what deployment does to it.</summary>
        public sealed class Change
        {
            public string Object;
            public string Type;
            public string Action;
            public int Index = -1;
        }

        public sealed class Dependency
        {
            public int Index;
            public string Name;
            public string Type;
            public string Action;
            public string ReferencedBy;
            public bool Deploy = true;
        }

        public enum Choice { None, Deploy, OpenScript }

        public sealed class Outcome
        {
            public Choice Choice;
            public bool Recompare;
            public string SaveCopyTo;
            public JsonObject Script;
        }

        private readonly ICompareHost _host;
        private readonly string _source, _target;
        private readonly bool _schema;
        private readonly string _selectedText;
        private readonly List<Change> _changes;
        private readonly List<Dependency> _dependencies;
        private readonly bool _includeDependencies;
        private readonly Func<bool, List<int>, Task<JsonObject>> _makeScript;

        /// <param name="target">The target as shown, or null when only a script can be made.</param>
        /// <param name="dependencies">Schema: objects the selection needs; null for data.</param>
        /// <param name="makeScript">Makes the script: with all dependencies, or with these extra differences.</param>
        public DeployWizard(ICompareHost host, string source, string target, string selectedText, List<Change> changes, List<Dependency> dependencies, bool includeDependencies, Func<bool, List<int>, Task<JsonObject>> makeScript)
        {
            _host = host;
            _source = source;
            _target = target;
            _schema = dependencies != null;
            _selectedText = selectedText;
            _changes = changes;
            _dependencies = dependencies ?? new List<Dependency>();
            _includeDependencies = includeDependencies;
            _makeScript = makeScript;
        }

        /// <summary>The first deployment method: deploying, or updating a scripts folder.</summary>
        public string DeployLabel { get; set; } = "Deploy using QuillSQL Compare";

        public Outcome Run()
        {
            var outcome = new Outcome();
            var window = Ui.Dialog(_host.Owner, "Deployment", null, 1000, 720);
            var steps = new List<string> { "Deployment method" };
            if (_schema) steps.Add("Dependencies");
            steps.Add("Review");
            var page = 0;

            // Page 1: deployment method.
            var deploy = new RadioButton { Content = DeployLabel, IsChecked = _target != null, IsEnabled = _target != null, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 6) };
            var recompare = Ui.Check("Recompare after deployment", true);
            var backup = Ui.Check("Back up target before deployment", false);
            backup.IsEnabled = false;
            backup.ToolTip = "Backups are not available yet; back up the target with SQL Server first.";
            var saveCopy = Ui.Check("Save a copy of the deployment script:", false);
            var copyPath = new TextBox { Text = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare", "Scripts", "deploy_" + DateTime.Now.ToString("yyyyMMdd_HHmmss") + ".sql"), Width = 460, IsEnabled = false };
            var browse = Ui.Button("Browse…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = Path.GetFileName(copyPath.Text) };
                if (Dialogs.Show(d, window) == true) copyPath.Text = d.FileName;
            });
            browse.IsEnabled = false;
            saveCopy.Checked += (s, e) => { copyPath.IsEnabled = browse.IsEnabled = true; };
            saveCopy.Unchecked += (s, e) => { copyPath.IsEnabled = browse.IsEnabled = false; };
            var deployOptions = Ui.Column(recompare, backup, saveCopy, Ui.Row(copyPath, browse));
            deployOptions.Margin = new Thickness(26, 0, 0, 18);
            var script = new RadioButton { Content = "Create a deployment script", IsChecked = _target == null, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 6) };
            var scriptNote = Ui.Text("The script opens in " + (_host is Window ? "the program registered for .sql files" : "a new query window") + "; nothing is changed on the target.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            scriptNote.Margin = new Thickness(26, 0, 0, 0);
            deploy.Checked += (s, e) => deployOptions.IsEnabled = true;
            script.Checked += (s, e) => deployOptions.IsEnabled = false;
            deployOptions.IsEnabled = _target != null;
            var methodPage = Ui.Column(deploy, deployOptions, script, scriptNote);
            if (_target == null)
            {
                var why = Ui.Text("The target is a snapshot, an empty database or a source control revision, so the changes can only be scripted.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
                why.Margin = new Thickness(0, 14, 0, 0);
                methodPage.Children.Add(why);
            }

            // Page 2 (schema): dependencies.
            var allDeps = Ui.Check("Deploy all dependencies (recommended)", _includeDependencies);
            var depGrid = new DataGrid { AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column, ItemsSource = _dependencies.Select(d => new DependencyRow(d)).ToList() };
            var depCheck = Ui.CheckColumn("Deploy", "Deploy", 70);
            depGrid.Columns.Add(depCheck);
            depGrid.Columns.Add(new DataGridTextColumn { Header = "Dependencies", Binding = new System.Windows.Data.Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            depGrid.Columns.Add(new DataGridTextColumn { Header = "Type", Binding = new System.Windows.Data.Binding("Type"), IsReadOnly = true, Width = 130 });
            depGrid.Columns.Add(new DataGridTextColumn { Header = "Referencing or referenced by", Binding = new System.Windows.Data.Binding("ReferencedBy"), IsReadOnly = true, Width = 280 });
            void syncDeps()
            {
                var all = allDeps.IsChecked == true;
                depGrid.IsEnabled = !all;
                if (all) foreach (var r in (List<DependencyRow>)depGrid.ItemsSource) r.Deploy = true;
                depGrid.Items.Refresh();
            }
            allDeps.Checked += (s, e) => syncDeps();
            allDeps.Unchecked += (s, e) => syncDeps();
            syncDeps();
            var depCounts = Ui.Text(Ui.Plural(_changes.Count, "object", "objects") + " you selected for deployment; " + Ui.Plural(_dependencies.Count, "object references or is referenced by them", "objects reference or are referenced by them") + " and differ" + (_dependencies.Count == 1 ? "s" : "") + ".", wrap: TextWrapping.Wrap);
            depCounts.Margin = new Thickness(0, 0, 0, 12);
            var depBody = new DockPanel();
            var depTop = Ui.Column(depCounts, allDeps);
            depTop.Margin = new Thickness(0, 0, 0, 10);
            DockPanel.SetDock(depTop, Dock.Top);
            depBody.Children.Add(depTop);
            if (_dependencies.Count == 0) depBody.Children.Add(Ui.Text("No dependencies", 15, brush: CompareTheme.Muted));
            else depBody.Children.Add(Ui.Card(depGrid, 4));

            // Last page: review.
            var review = new TabControl();
            var reviewHeading = Ui.Text(_selectedText, 15, bold: true);
            reviewHeading.Margin = new Thickness(0, 0, 0, 10);
            var reviewBody = new DockPanel();
            DockPanel.SetDock(reviewHeading, Dock.Top);
            reviewBody.Children.Add(reviewHeading);
            reviewBody.Children.Add(Ui.Card(review, 0));
            JsonObject made = null;

            var title = Ui.Title("");
            var lead = Ui.Text("", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            lead.Margin = new Thickness(0, 4, 0, 14);
            var stepRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 16) };
            var content = new ContentControl();
            var header = Ui.Column(stepRow, title, lead);
            var body = new DockPanel { Margin = new Thickness(24, 18, 24, 18) };
            DockPanel.SetDock(header, Dock.Top);
            body.Children.Add(header);
            body.Children.Add(content);

            var back = Ui.Button("< Back", () => { }, minWidth: 90);
            var next = Ui.Button("Next >", () => { }, primary: true, minWidth: 130);
            var cancel = Ui.Button("Cancel", () => window.Close(), minWidth: 90);
            cancel.IsCancel = true;
            var sides = Ui.Text(_source + "  →  " + (_target ?? "script only"), brush: CompareTheme.Muted);
            var buttons = new DockPanel();
            var right = Ui.Row(back, next, cancel);
            DockPanel.SetDock(right, Dock.Right);
            buttons.Children.Add(right);
            buttons.Children.Add(sides);

            void show()
            {
                stepRow.Children.Clear();
                for (var i = 0; i < steps.Count; i++)
                {
                    var t = Ui.Text((i + 1) + "  " + steps[i], bold: i == page, brush: i == page ? null : CompareTheme.Muted);
                    t.Margin = new Thickness(0, 0, 24, 0);
                    stepRow.Children.Add(t);
                }
                var name = steps[page];
                back.IsEnabled = page > 0;
                if (name == "Deployment method")
                {
                    title.Text = "Deployment method";
                    lead.Text = "Choose whether to deploy the changes now or to create a script for them.";
                    content.Content = methodPage;
                    next.Content = "Next >";
                }
                else if (name == "Dependencies")
                {
                    title.Text = "Dependencies";
                    lead.Text = "Objects that the selected objects need, and that differ, are deployed with them unless you leave them out.";
                    content.Content = depBody;
                    next.Content = "Next >";
                }
                else
                {
                    title.Text = "Review deployment script";
                    lead.Text = script.IsChecked == true ? "Review the summary, the warnings and the script, then open the script." : DeployLabel == "Update the scripts folder" ? "Review the summary, the warnings and the script, then update the folder: the files of the deployed objects are rewritten." : "Review the summary, the warnings and the script. It runs in one transaction and is rolled back if any statement fails.";
                    content.Content = reviewBody;
                    next.Content = script.IsChecked == true ? "Open script in editor" : DeployLabel == "Update the scripts folder" ? "Update scripts" : "Deploy now";
                    var high = made?.List("warnings").OfType<JsonObject>().Count(w => w.Str("severity") == "High") ?? 0;
                    next.IsEnabled = made != null && (made.Str("sql") ?? "").Length > 0;
                    if (high > 0) lead.Text += " There " + (high == 1 ? "is 1 high-severity warning" : "are " + high + " high-severity warnings") + ": data may be lost.";
                }
            }

            async Task forward()
            {
                if (steps[page] == "Review")
                {
                    var sql = made.Str("sql") ?? "";
                    if (script.IsChecked == true)
                    {
                        outcome.Choice = Choice.OpenScript;
                    }
                    else
                    {
                        var high = made.List("warnings").OfType<JsonObject>().Count(w => w.Str("severity") == "High");
                        var message = "You are about to make permanent changes to:\n\n" + _target + "\n\nDo you want to deploy now?" + (high > 0 ? "\n\nThere " + (high == 1 ? "is 1 high-severity warning" : "are " + high + " high-severity warnings") + ": data may be lost." : "");
                        if (Ui.Message(window, "Confirm deployment", message, Ui.Tone.Warning, "Deploy now", "Cancel") != 0) return;
                        outcome.Choice = Choice.Deploy;
                        outcome.Recompare = recompare.IsChecked == true;
                        if (saveCopy.IsChecked == true && copyPath.Text.Trim().Length > 0) outcome.SaveCopyTo = copyPath.Text.Trim();
                    }
                    outcome.Script = made;
                    window.Close();
                    return;
                }
                page++;
                if (steps[page] == "Review")
                {
                    next.IsEnabled = back.IsEnabled = false;
                    content.Content = Ui.Text("Creating the deployment script…", brush: CompareTheme.Muted);
                    foreach (var r in (List<DependencyRow>)depGrid.ItemsSource) r.Source.Deploy = r.Deploy;
                    var extra = _dependencies.Where(d => d.Deploy).Select(d => d.Index).ToList();
                    made = await _makeScript(allDeps.IsChecked == true, extra);
                    if (made == null)
                    {
                        page--;
                        show();
                        next.IsEnabled = true;
                        return;
                    }
                    FillReview(review, made, extra);
                }
                show();
            }

            back.Click += (s, e) => { if (page > 0) { page--; show(); } };
            next.Click += async (s, e) => await forward();
            show();
            window.Content = Ui.WithFooter(body, buttons);
            window.ShowDialog();
            return outcome;
        }

        private sealed class DependencyRow
        {
            public readonly Dependency Source;
            public DependencyRow(Dependency d) { Source = d; Deploy = d.Deploy; }
            public bool Deploy { get; set; }
            public string Name => Source.Name;
            public string Type => Source.Type;
            public string ReferencedBy => Source.ReferencedBy;
        }

        private void FillReview(TabControl tabs, JsonObject made, List<int> extra)
        {
            tabs.Items.Clear();
            var changes = _changes.Concat(_dependencies.Where(d => d.Deploy && extra.Contains(d.Index)).Select(d => new Change { Object = d.Name, Type = d.Type, Action = d.Action, Index = d.Index })).ToList();

            // Summary: by object, or by what is done.
            var viewBy = new ComboBox { Width = 200, ItemsSource = new[] { "Object", "Modification" }, SelectedIndex = 0 };
            var tree = new TreeView();
            void fill()
            {
                tree.Items.Clear();
                var byObject = viewBy.SelectedIndex == 0;
                foreach (var g in changes.GroupBy(c => byObject ? c.Type : c.Action).OrderBy(g => g.Key))
                {
                    var node = new TreeViewItem { Header = g.Key + " (" + g.Count() + ")", IsExpanded = true, FontWeight = FontWeights.SemiBold };
                    foreach (var c in g.OrderBy(c => c.Object)) node.Items.Add(new TreeViewItem { Header = byObject ? c.Action + "  " + c.Object : c.Object + "  (" + c.Type + ")", FontWeight = FontWeights.Normal });
                    tree.Items.Add(node);
                }
            }
            viewBy.SelectionChanged += (s, e) => fill();
            fill();
            var viewLabel = Ui.Text("View by", brush: CompareTheme.Muted);
            viewLabel.Margin = new Thickness(0, 0, 8, 0);
            var summaryTop = Ui.Row(viewLabel, viewBy);
            summaryTop.Margin = new Thickness(12, 8, 12, 4);
            var summary = new DockPanel();
            DockPanel.SetDock(summaryTop, Dock.Top);
            summary.Children.Add(summaryTop);
            summary.Children.Add(tree);
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.List, "Summary"), Content = summary });

            var warnings = made.List("warnings").OfType<JsonObject>().ToList();
            var list = new StackPanel { Margin = new Thickness(16, 12, 16, 12) };
            foreach (var w in warnings.OrderBy(w => w.Str("severity") == "High" ? 0 : w.Str("severity") == "Medium" ? 1 : 2))
            {
                var sev = w.Str("severity");
                var brush = sev == "High" ? CompareTheme.High : sev == "Medium" ? CompareTheme.Medium : CompareTheme.Muted;
                var icon = Ui.Icon(sev == "Low" ? Glyph.Info : Glyph.Warning, 15, brush);
                icon.VerticalAlignment = VerticalAlignment.Top;
                icon.Margin = new Thickness(0, 2, 12, 0);
                var text = new TextBlock { TextWrapping = TextWrapping.Wrap };
                text.Inlines.Add(new Run(sev + (string.IsNullOrEmpty(w.Str("object")) ? "" : " · " + w.Str("object"))) { FontWeight = FontWeights.SemiBold });
                text.Inlines.Add(new LineBreak());
                text.Inlines.Add(new Run(w.Str("message")) { Foreground = CompareTheme.Muted });
                var item = new DockPanel { Margin = new Thickness(0, 0, 0, 14) };
                DockPanel.SetDock(icon, Dock.Left);
                item.Children.Add(icon);
                item.Children.Add(text);
                list.Children.Add(item);
            }
            if (warnings.Count == 0) list.Children.Add(Ui.Text("No warnings", brush: CompareTheme.Muted));
            var warningTab = new TabItem { Header = Ui.IconText(Glyph.Warning, "Warnings (" + warnings.Count + ")"), Content = new ScrollViewer { Content = list, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } };
            tabs.Items.Add(warningTab);

            var sql = made.Str("sql") ?? "";
            var code = Ui.Code();
            code.Text = sql.Length == 0 ? "-- Nothing to deploy." : sql;
            var find = Ui.SearchBox("Find", 220);
            void findNext(bool forward)
            {
                if (find.Text.Length == 0) return;
                var from = forward ? code.SelectionStart + Math.Max(code.SelectionLength, 0) : code.SelectionStart - 1;
                var at = forward ? code.Text.IndexOf(find.Text, Math.Min(from, code.Text.Length), StringComparison.OrdinalIgnoreCase)
                    : from < 0 ? -1 : code.Text.LastIndexOf(find.Text, from, StringComparison.OrdinalIgnoreCase);
                if (at < 0) at = forward ? code.Text.IndexOf(find.Text, StringComparison.OrdinalIgnoreCase) : code.Text.LastIndexOf(find.Text, StringComparison.OrdinalIgnoreCase);
                if (at < 0) return;
                code.Focus();
                code.Select(at, find.Text.Length);
                code.ScrollToLine(code.GetLineIndexFromCharacterIndex(at));
            }
            find.KeyDown += (s, e) => { if (e.Key == System.Windows.Input.Key.Enter) findNext((System.Windows.Input.Keyboard.Modifiers & System.Windows.Input.ModifierKeys.Shift) == 0); };
            var scriptTools = Ui.Row(find,
                Ui.IconButton(Glyph.Up, "Find previous", () => findNext(false)),
                Ui.IconButton(Glyph.Down, "Find next", () => findNext(true)),
                Ui.Command(Glyph.Copy, "Copy", () => Clipboard.SetText(sql)),
                Ui.Command(Glyph.Save, "Save script…", () =>
                {
                    var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = "deploy.sql" };
                    if (Dialogs.Show(d, _host.Owner) == true) File.WriteAllText(d.FileName, sql, new System.Text.UTF8Encoding(true));
                }));
            scriptTools.Margin = new Thickness(12, 6, 12, 4);
            var scriptPanel = new DockPanel();
            DockPanel.SetDock(scriptTools, Dock.Top);
            scriptPanel.Children.Add(scriptTools);
            scriptPanel.Children.Add(code);
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Code, "Deployment script"), Content = scriptPanel });
            tabs.SelectedItem = warnings.Count > 0 ? warningTab : tabs.Items[0];
        }
    }
}
