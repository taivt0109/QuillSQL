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
            /// <summary>The backup settings chosen (without the password), for the project; null when not backing up.</summary>
            public JsonObject Backup;
            /// <summary>A snapshot of the target to write before deploying, or null.</summary>
            public string SnapshotPath;
            /// <summary>Open the scripts folder's changed files in Explorer afterwards.</summary>
            public bool ShowFiles;
        }

        private readonly ICompareHost _host;
        private readonly string _source, _target;
        private readonly bool _schema;
        private readonly string _selectedText;
        private readonly List<Change> _changes;
        private readonly List<Dependency> _dependencies;
        private readonly bool _includeDependencies;
        private readonly Func<bool, List<int>, JsonObject, Task<JsonObject>> _makeScript;

        /// <param name="target">The target as shown, or null when only a script can be made.</param>
        /// <param name="dependencies">Schema: objects the selection needs; null for data.</param>
        /// <param name="makeScript">Makes the script: with all dependencies, or with these extra differences, and the backup (or null).</param>
        public DeployWizard(ICompareHost host, string source, string target, string selectedText, List<Change> changes, List<Dependency> dependencies, bool includeDependencies, Func<bool, List<int>, JsonObject, Task<JsonObject>> makeScript)
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

        /// <summary>What the target's server offers for backups (`backup_defaults`); null when the target cannot be backed up.</summary>
        public Func<Task<JsonObject>> BackupDefaults { get; set; }

        /// <summary>The project's backup settings, to start from.</summary>
        public JsonObject BackupSettings { get; set; }

        /// <summary>The files deploying would write and delete in a scripts folder target; null for other targets.</summary>
        public Func<Task<JsonObject>> FolderPreview { get; set; }

        /// <summary>Shows the differences of a dependency (by difference index); null when there is no viewer.</summary>
        public Func<int, Task> ShowDependency { get; set; }

        /// <summary>A snapshot of the target can be written before deploying (schema, live or folder target).</summary>
        public string SnapshotName { get; set; }

        /// <summary>Where "Create a deployment script" opens it, and how to change that; null when it is fixed.</summary>
        public string EditorName { get; set; }
        public Action ChangeEditor { get; set; }

        public Outcome Run()
        {
            var outcome = new Outcome();
            var window = Ui.Dialog(_host.Owner, "Deployment", null, 1000, 720);
            var steps = new List<string>();
            var page = 0;

            // Page 1: deployment method.
            // The choices of the last deployment come back (method, recompare, save a copy, show files, snapshot).
            var settings = CompareSettings.Load();
            var remembered = settings.Wizard ?? new JsonObject();
            bool recall(string key, bool fallback) => remembered.ContainsKey(key) ? remembered.Bool(key) : fallback;
            var scriptOnly = _target == null || remembered.Str("method") == "script";
            var deploy = new RadioButton { Content = DeployLabel, IsChecked = !scriptOnly, IsEnabled = _target != null, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 6) };
            var recompare = Ui.Check("Recompare after deployment", recall("recompare", true));
            var saved = BackupSettings ?? new JsonObject();
            var backup = Ui.Check("Back up target before deployment", BackupDefaults != null && saved.Bool("enabled"));
            backup.IsEnabled = BackupDefaults != null;
            backup.ToolTip = BackupDefaults != null ? "The backup is the first statement of the deployment script" : "Not available for this target";
            var saveCopy = Ui.Check("Save a copy of the deployment script:", recall("saveCopy", false));
            var showFiles = Ui.Check("Show the changed files in Explorer afterwards", recall("showFiles", false));
            showFiles.Visibility = FolderPreview != null ? Visibility.Visible : Visibility.Collapsed;
            var copyPath = new TextBox { Text = Path.Combine(settings.ScriptFolder, "deploy_" + DateTime.Now.ToString("yyyyMMdd_HHmmss") + ".sql"), Width = 460, IsEnabled = saveCopy.IsChecked == true };
            var browse = Ui.Button("Browse…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = Path.GetFileName(copyPath.Text) };
                if (Dialogs.Show(d, window) == true) copyPath.Text = d.FileName;
            });
            browse.IsEnabled = saveCopy.IsChecked == true;
            saveCopy.Checked += (s, e) => { copyPath.IsEnabled = browse.IsEnabled = true; };
            saveCopy.Unchecked += (s, e) => { copyPath.IsEnabled = browse.IsEnabled = false; };
            // A snapshot of the target, to compare with or roll back to later (schema).
            var snapshot = Ui.Check("Create a snapshot of the target before deployment:", SnapshotName != null && recall("snapshot", false));
            var snapshotPath = new TextBox { Text = SnapshotName ?? "", Width = 460, IsEnabled = snapshot.IsChecked == true };
            var snapshotBrowse = Ui.Button("Browse…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL Compare snapshot (*.snp)|*.snp|QuillSQL snapshot (*.json)|*.json", FileName = Path.GetFileName(snapshotPath.Text), InitialDirectory = Directory.Exists(Path.GetDirectoryName(snapshotPath.Text)) ? Path.GetDirectoryName(snapshotPath.Text) : null };
                if (Dialogs.Show(d, window) == true) snapshotPath.Text = d.FileName;
            });
            snapshotBrowse.IsEnabled = snapshot.IsChecked == true;
            snapshot.Checked += (s, e) => snapshotPath.IsEnabled = snapshotBrowse.IsEnabled = true;
            snapshot.Unchecked += (s, e) => snapshotPath.IsEnabled = snapshotBrowse.IsEnabled = false;
            var snapshotRow = Ui.Column(snapshot, Ui.Row(snapshotPath, snapshotBrowse));
            snapshotRow.Visibility = SnapshotName != null ? Visibility.Visible : Visibility.Collapsed;
            var deployOptions = Ui.Column(recompare, showFiles, saveCopy, Ui.Row(copyPath, browse), snapshotRow);
            deployOptions.Margin = new Thickness(26, 0, 0, 18);
            var script = new RadioButton { Content = "Create a deployment script", IsChecked = scriptOnly, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 6) };
            var openIn = EditorName ?? (_host is Window ? "the program registered for .sql files" : "a new query window connected to the target");
            var scriptNote = new TextBlock { TextWrapping = TextWrapping.Wrap, Foreground = CompareTheme.Muted };
            scriptNote.Inlines.Add(new Run("Open deployment script in: " + openIn + ". Nothing is changed on the target. "));
            if (ChangeEditor != null)
            {
                var change = new Hyperlink(new Run("Change…"));
                change.Click += (s, e) =>
                {
                    ChangeEditor();
                    scriptNote.Inlines.Clear();
                    scriptNote.Inlines.Add(new Run("The script opens in the editor chosen in Application options. "));
                };
                scriptNote.Inlines.Add(change);
            }
            scriptNote.Margin = new Thickness(26, 0, 0, 0);
            deploy.Checked += (s, e) => deployOptions.IsEnabled = true;
            script.Checked += (s, e) => deployOptions.IsEnabled = false;
            deployOptions.IsEnabled = _target != null;
            backup.Margin = new Thickness(0, 18, 0, 0);
            var methodPage = Ui.Column(deploy, deployOptions, script, scriptNote, backup);
            if (_target == null)
            {
                var why = Ui.Text("The target is a snapshot, a backup, an empty database or a source control revision, so the changes can only be scripted.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
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
            if (ShowDependency != null)
            {
                // Double-click a dependency to see how it differs.
                depGrid.MouseDoubleClick += async (s, e) => { if (depGrid.SelectedItem is DependencyRow r) await ShowDependency(r.Source.Index); };
                depGrid.ToolTip = "Double-click a dependency to see its differences";
            }
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

            // Backup of target: SQL Compare's options for a native backup or SQL Backup.
            var provider = new ComboBox { Width = 260, HorizontalAlignment = HorizontalAlignment.Left };
            provider.Items.Add(new ComboBoxItem { Content = "Native SQL Server" });
            provider.Items.Add(new ComboBoxItem { Content = "SQL Backup", IsEnabled = false, ToolTip = "SQL Backup is not installed on the target's server" });
            provider.SelectedIndex = 0;
            var folderBox = new TextBox { Text = saved.Str("folder") ?? "" };
            var fileBox = new TextBox { Text = saved.Str("file") ?? "", IsEnabled = saved.ContainsKey("nameAutomatically") && !saved.Bool("nameAutomatically") };
            var auto = Ui.Check("Name file automatically", !fileBox.IsEnabled);
            auto.Checked += (s, e) => fileBox.IsEnabled = false;
            auto.Unchecked += (s, e) => fileBox.IsEnabled = true;
            var overwrite = Ui.Check("Overwrite existing backup files of the same name", saved.Bool("overwrite"));
            var differential = Ui.Check("Differential backup", saved.Bool("differential"));
            var levels = new[] { "Highest speed", "High speed", "Standard speed", "Best compression" };
            var compression = new Slider { Minimum = 1, Maximum = 4, IsSnapToTickEnabled = true, TickFrequency = 1, TickPlacement = System.Windows.Controls.Primitives.TickPlacement.BottomRight, Width = 260, Value = Math.Max(1, Math.Min(4, saved.Long("compressionLevel") == 0 ? 1 : saved.Long("compressionLevel"))) };
            var compressionText = Ui.Text(levels[(int)compression.Value - 1], brush: CompareTheme.Muted);
            compression.ValueChanged += (s, e) => compressionText.Text = levels[(int)compression.Value - 1];
            var encrypt = Ui.Check("Encrypt backup", (saved.Str("encryption") ?? "none") != "none");
            var password = new PasswordBox { Width = 260, HorizontalAlignment = HorizontalAlignment.Left };
            var confirm = new PasswordBox { Width = 260, HorizontalAlignment = HorizontalAlignment.Left };
            var strong = Ui.Check("256-bit encryption", saved.Str("encryption") != "aes128");
            var threads = new TextBox { Width = 60, HorizontalAlignment = HorizontalAlignment.Left, Text = Math.Max(1, saved.Long("threadCount")).ToString() };
            var multi = Ui.Check("Use multiple threads", saved.Long("threadCount") > 1);
            var sqbOptions = Ui.Form(
                Ui.Field("Compression", Ui.Row(compression, new Border { Width = 10 }, compressionText)),
                Ui.Field(null, encrypt),
                Ui.Field("Password", password),
                Ui.Field("Confirm password", confirm),
                Ui.Field(null, strong),
                Ui.Field(null, Ui.Row(multi, threads)));
            sqbOptions.Visibility = Visibility.Collapsed;
            JsonObject caps = null;
            provider.SelectionChanged += (s, e) =>
            {
                var sqb = provider.SelectedIndex == 1;
                sqbOptions.Visibility = sqb ? Visibility.Visible : Visibility.Collapsed;
                // The default folder follows the provider while it has not been changed.
                if (caps != null)
                {
                    var from = sqb ? caps.Str("nativeFolder") : caps.Str("sqbFolder");
                    if (folderBox.Text == from || folderBox.Text.Length == 0) folderBox.Text = sqb ? caps.Str("sqbFolder") : caps.Str("nativeFolder");
                }
            };
            var backupStatus = Ui.Text("", brush: CompareTheme.High, wrap: TextWrapping.Wrap);
            var folderNote = Ui.Text("A folder on the SQL Server machine, as the server sees it. The backup is the first statement of the script, and a command to restore it is at the end.", 12, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            var backupPage = new ScrollViewer
            {
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                Content = Ui.Column(Ui.Form(
                    Ui.Field("Perform a full backup using", provider),
                    Ui.Field("Folder", folderBox),
                    Ui.Field(null, folderNote),
                    Ui.Field("File name", fileBox),
                    Ui.Field("Backup options", Ui.Column(auto, overwrite, differential))), sqbOptions, backupStatus),
            };
            JsonObject backupRequest()
            {
                if (!steps.Contains("Backup of target")) return null;
                var sqb = provider.SelectedIndex == 1;
                int.TryParse(threads.Text, out var n);
                return new JsonObject
                {
                    ["enabled"] = true,
                    ["provider"] = sqb ? "sqb" : "native",
                    ["differential"] = differential.IsChecked == true,
                    ["folder"] = folderBox.Text.Trim(),
                    ["file"] = fileBox.Text.Trim(),
                    ["nameAutomatically"] = auto.IsChecked == true,
                    ["overwrite"] = overwrite.IsChecked == true,
                    ["compressionLevel"] = sqb ? (int)compression.Value : 1,
                    ["encryption"] = sqb && encrypt.IsChecked == true ? (strong.IsChecked == true ? "aes256" : "aes128") : "none",
                    ["threadCount"] = sqb && multi.IsChecked == true ? Math.Max(1, Math.Min(32, n)) : 1,
                    ["password"] = sqb && encrypt.IsChecked == true ? password.Password : null,
                };
            }            var defaultsLoaded = false;

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

            void fillSteps()
            {
                steps.Clear();
                steps.Add("Deployment method");
                if (backup.IsChecked == true && BackupDefaults != null) steps.Add("Backup of target");
                if (_schema) steps.Add("Dependencies");
                steps.Add("Review");
                if (IsLoadedPage()) show();
            }
            bool IsLoadedPage() => stepRow.Children.Count > 0;

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
                else if (name == "Backup of target")
                {
                    title.Text = "Backup of target";
                    lead.Text = "Back up " + _target + " before the deployment. If the backup fails, the rest of the script does not run.";
                    content.Content = backupPage;
                    next.Content = "Next >";
                    if (!defaultsLoaded)
                    {
                        defaultsLoaded = true;
                        window.Dispatcher.BeginInvoke(new Action(async () =>
                        {
                            var d = await BackupDefaults();
                            if (d == null) return;
                            caps = d.Obj("capabilities") ?? new JsonObject();
                            var installed = caps.Long("sqbVersion") > 0;
                            ((ComboBoxItem)provider.Items[1]).IsEnabled = installed;
                            if (installed) ((ComboBoxItem)provider.Items[1]).ToolTip = "SQL Backup " + caps.Long("sqbVersion") + " is installed on the target's server";
                            if (installed && saved.Str("provider") == "sqb") provider.SelectedIndex = 1;
                            if (folderBox.Text.Length == 0) folderBox.Text = (provider.SelectedIndex == 1 ? caps.Str("sqbFolder") : caps.Str("nativeFolder")) ?? "";
                            if (fileBox.Text.Length == 0) fileBox.Text = d.Str("file") ?? "";
                        }));
                    }
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
                        if (snapshot.IsChecked == true && SnapshotName != null && snapshotPath.Text.Trim().Length > 0) outcome.SnapshotPath = snapshotPath.Text.Trim();
                        outcome.ShowFiles = FolderPreview != null && showFiles.IsChecked == true;
                    }
                    var st = CompareSettings.Load();
                    st.Wizard = new JsonObject
                    {
                        ["method"] = script.IsChecked == true ? "script" : "deploy",
                        ["recompare"] = recompare.IsChecked == true,
                        ["saveCopy"] = saveCopy.IsChecked == true,
                        ["showFiles"] = showFiles.IsChecked == true,
                        ["snapshot"] = snapshot.IsChecked == true,
                    };
                    if (outcome.SaveCopyTo != null && st.UpdateScriptFolder) st.ScriptFolder = Path.GetDirectoryName(outcome.SaveCopyTo);
                    st.Save();
                    outcome.Script = made;
                    if (BackupDefaults != null)
                    {
                        outcome.Backup = backupRequest() ?? new JsonObject { ["enabled"] = false };
                        outcome.Backup.Remove("password");
                    }
                    window.Close();
                    return;
                }
                if (steps[page] == "Backup of target")
                {
                    var sqbChecks = provider.SelectedIndex == 1 && encrypt.IsChecked == true;
                    backupStatus.Text = folderBox.Text.Trim().Length == 0 ? "Folder cannot be empty"
                        : auto.IsChecked != true && fileBox.Text.Trim().Length == 0 ? "Enter a file name to continue"
                        : sqbChecks && password.Password.Length == 0 ? "No password provided"
                        : sqbChecks && password.Password != confirm.Password ? "Passwords must match"
                        : "";
                    if (backupStatus.Text.Length > 0) return;
                }
                page++;
                if (steps[page] == "Review")
                {
                    next.IsEnabled = back.IsEnabled = false;
                    content.Content = Ui.Text("Creating the deployment script…", brush: CompareTheme.Muted);
                    foreach (var r in (List<DependencyRow>)depGrid.ItemsSource) r.Source.Deploy = r.Deploy;
                    var extra = _dependencies.Where(d => d.Deploy).Select(d => d.Index).ToList();
                    made = await _makeScript(allDeps.IsChecked == true, extra, backupRequest());
                    if (made == null)
                    {
                        page--;
                        show();
                        next.IsEnabled = true;
                        return;
                    }
                    var files = FolderPreview != null && deploy.IsChecked == true ? await FolderPreview() : null;
                    FillReview(review, made, extra, files);
                }
                show();
            }

            deploy.Checked += (s, e) => fillSteps();
            script.Checked += (s, e) => fillSteps();
            backup.Checked += (s, e) => fillSteps();
            backup.Unchecked += (s, e) => fillSteps();
            next.Click += async (s, e) => await forward();
            fillSteps();
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

        /// <summary>What a script action reads as in the summary: "Drop constraint [FK_x] from [dbo].[T]".</summary>
        private static string ActionText(JsonObject a)
        {
            var action = a.Str("action") ?? "";
            var text = action.Length > 0 ? char.ToUpperInvariant(action[0]) + action.Substring(1) : action;
            var obj = a.Str("object") ?? "";
            var detail = a.Str("detail") ?? "";
            switch (action)
            {
                case "rename": return "Rename " + obj + " to " + detail;
                case "create index": case "alter index": return text + " " + detail + " on " + obj;
                case "drop index": return "Drop index " + detail + " from " + obj;
                case "drop constraint": case "drop column": return text + " " + detail + " from " + obj;
                case "alter column": return "Alter column " + detail + " on " + obj;
                case "role membership": return "Role membership: " + detail + " in " + obj;
                default:
                    if (action.StartsWith("add ", StringComparison.Ordinal)) return text + (detail.Length > 0 ? " " + detail : "") + " to " + obj;
                    if (action.EndsWith(" permissions", StringComparison.Ordinal)) return text + " (" + detail + ") on " + obj;
                    return (text + " " + obj).Trim();
            }
        }

        private void FillReview(TabControl tabs, JsonObject made, List<int> extra, JsonObject files = null)
        {
            tabs.Items.Clear();
            var changes = _changes.Concat(_dependencies.Where(d => d.Deploy && extra.Contains(d.Index)).Select(d => new Change { Object = d.Name, Type = d.Type, Action = d.Action, Index = d.Index })).ToList();
            // A schema script lists what each statement does, side effects included (dependent
            // objects dropped and created again, tables rebuilt); a data script is summed up by table.
            var actions = made.List("actions").OfType<JsonObject>().ToList();

            // Summary: by object, or by modification in the order the script makes them.
            var viewBy = new ComboBox { Width = 220, ItemsSource = actions.Count > 0 ? new[] { "Object", "Modification (in order)" } : new[] { "Object", "Modification" }, SelectedIndex = 0 };
            var tree = new TreeView();
            void fill()
            {
                tree.Items.Clear();
                var byObject = viewBy.SelectedIndex == 0;
                if (actions.Count > 0)
                {
                    if (!byObject)
                    {
                        var n = 0;
                        foreach (var a in actions) tree.Items.Add(new TreeViewItem { Header = ++n + ".  " + ActionText(a) });
                        return;
                    }
                    var selected = new HashSet<string>(changes.Select(c => c.Object), StringComparer.OrdinalIgnoreCase);
                    foreach (var g in actions.GroupBy(a => a.Str("object") ?? "").OrderBy(g => selected.Contains(g.Key) ? 0 : 1).ThenBy(g => g.Key))
                    {
                        var header = (g.Key.Length == 0 ? "Database" : g.Key) + (selected.Contains(g.Key) || g.Key.Length == 0 ? "" : "  (affected)");
                        var node = new TreeViewItem { Header = header + " (" + g.Count() + ")", IsExpanded = true, FontWeight = FontWeights.SemiBold };
                        foreach (var a in g) node.Items.Add(new TreeViewItem { Header = ActionText(a), FontWeight = FontWeights.Normal });
                        tree.Items.Add(node);
                    }
                    return;
                }
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
            var copyActions = Ui.Command(Glyph.Copy, "Copy", () =>
            {
                var lines = actions.Count > 0 ? actions.Select(ActionText) : changes.Select(c => c.Action + " " + c.Object + " (" + c.Type + ")");
                Clipboard.SetText(string.Join(Environment.NewLine, lines));
            }, "Copy the actions to the clipboard");
            var summaryTop = Ui.Row(viewLabel, viewBy, new Border { Width = 12 }, copyActions);
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
            var copyWarnings = Ui.Command(Glyph.Copy, "Copy", () => Clipboard.SetText(string.Join(Environment.NewLine, warnings.Select(w => w.Str("severity") + "\t" + w.Str("object") + "\t" + w.Str("message")))), "Copy the warnings to the clipboard");
            copyWarnings.HorizontalAlignment = HorizontalAlignment.Left;
            copyWarnings.Margin = new Thickness(12, 6, 12, 0);
            copyWarnings.IsEnabled = warnings.Count > 0;
            var warningPanel = new DockPanel();
            DockPanel.SetDock(copyWarnings, Dock.Top);
            warningPanel.Children.Add(copyWarnings);
            warningPanel.Children.Add(new ScrollViewer { Content = list, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });
            var warningTab = new TabItem { Header = Ui.IconText(Glyph.Warning, "Warnings (" + warnings.Count + ")"), Content = warningPanel };
            tabs.Items.Add(warningTab);
            if (files != null)
            {
                // The files of the scripts folder that deploying writes or deletes.
                var rows = files.Strings("written").Select(f => new { File = f, Change = "Write" }).Concat(files.Strings("deleted").Select(f => new { File = f, Change = "Delete" })).ToList();
                var fileGrid = new DataGrid { ItemsSource = rows, AutoGenerateColumns = false, IsReadOnly = true, HeadersVisibility = DataGridHeadersVisibility.Column };
                fileGrid.Columns.Add(new DataGridTextColumn { Header = "Change", Binding = new System.Windows.Data.Binding("Change"), Width = 90 });
                fileGrid.Columns.Add(new DataGridTextColumn { Header = "File", Binding = new System.Windows.Data.Binding("File"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
                var copyFiles = Ui.Command(Glyph.Copy, "Copy files", () => Clipboard.SetText(string.Join(Environment.NewLine, rows.Select(r => r.Change + "\t" + r.File))), "Copy the list of files");
                copyFiles.HorizontalAlignment = HorizontalAlignment.Left;
                copyFiles.Margin = new Thickness(12, 6, 12, 4);
                var filePanel = new DockPanel();
                DockPanel.SetDock(copyFiles, Dock.Top);
                filePanel.Children.Add(copyFiles);
                filePanel.Children.Add(rows.Count == 0 ? (UIElement)Ui.Text("No files change.", brush: CompareTheme.Muted) : fileGrid);
                tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Folder, "Affected files (" + rows.Count + ")"), Content = filePanel });
            }

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
                    if (Dialogs.Show(d, _host.Owner) == true) File.WriteAllText(d.FileName, sql, CompareSettings.EncodingOf(CompareSettings.Load().ScriptEncoding));
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
