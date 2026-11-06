using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>The state of one Compare window: the project, its passwords and the engine.</summary>
    public sealed class CompareSession
    {
        public Engine Engine { get; }
        public ICompareHost Host { get; }
        public JsonObject Project { get; set; }
        /// <summary>Passwords typed this session, by side ("source", "target"); never saved in the project.</summary>
        public Dictionary<string, string> Passwords { get; } = new Dictionary<string, string>();
        public string AccessToken { get; set; }
        public JsonObject OptionDefinitions { get; set; } = new JsonObject();
        public string FilePath { get; set; }

        public CompareSession(Engine engine, ICompareHost host, JsonObject project)
        {
            Engine = engine;
            Host = host;
            Project = project;
        }

        public bool IsData => Project.Str("kind") == "data";

        /// <summary>The project with the session's passwords, for engine requests.</summary>
        public JsonObject WithPasswords()
        {
            var p = Project.Clone();
            foreach (var side in new[] { "source", "target" })
            {
                var s = p.Obj(side);
                if (s == null) continue;
                var password = Passwords.TryGetValue(side, out var typed) && !string.IsNullOrEmpty(typed)
                    ? typed
                    : s.Bool("rememberPassword") ? Credentials.Read(s.Str("server"), s.Str("user")) : null;
                if (password != null) s["password"] = password;
                if (AccessToken != null && s.Str("authentication") == "entra") s["accessToken"] = AccessToken;
            }
            return p;
        }

        /// <summary>A new, empty project of a kind, with "my defaults" options when set.</summary>
        public static JsonObject NewProject(bool data, JsonObject optionDefinitions)
        {
            var settings = CompareSettings.Load();
            var mine = data ? settings.MyDataDefaults : settings.MySchemaDefaults;
            var defaults = mine ?? optionDefinitions.List(data ? "data" : "schema").OfType<JsonObject>().Where(o => o.Bool("default")).Select(o => o.Str("name")).ToList();
            JsonObject side() => new JsonObject { ["type"] = "database", ["server"] = "", ["database"] = "", ["authentication"] = "windows", ["encrypt"] = true };
            return new JsonObject
            {
                ["version"] = 1.0,
                ["kind"] = data ? "data" : "schema",
                ["source"] = side(),
                ["target"] = side(),
                ["options"] = defaults.Cast<object>().ToList(),
            };
        }
    }

    /// <summary>
    /// A complete Schema or Data Compare window: toolbar, project editor, results and deployment.
    /// Hosted by an SSMS tool window and by QuillCompare.exe.
    /// </summary>
    public sealed class CompareView : DockPanel
    {
        private readonly Engine _engine;
        private readonly ICompareHost _host;
        private CompareSession _session;
        private ProjectEditor _editor;
        private SchemaResults _schemaResults;
        private DataResults _dataResults;
        private readonly ContentControl _content = new ContentControl();
        private readonly TextBlock _title = Ui.Text("", 15, bold: true);
        private readonly TextBlock _status = Ui.Text("", brush: CompareTheme.Muted);
        private readonly Button _compare, _deploy, _edit, _cancel;
        private readonly ComboBox _recent = new ComboBox { Width = 220, Margin = new Thickness(4, 0, 0, 0), ToolTip = "Recent projects" };
        private JsonObject _optionDefs;
        private bool _dirty;

        /// <summary>Raised when the title changes (project name, kind, dirty mark).</summary>
        public event Action<string> TitleChanged;

        public CompareView(Engine engine, ICompareHost host)
        {
            _engine = engine;
            _host = host;
            Background = CompareTheme.Background;
            TextElement.SetForeground(this, CompareTheme.Foreground);

            _compare = Ui.Button("Compare", async () => await Compare(), primary: true);
            _deploy = Ui.Button("Deploy…", async () => await Deploy());
            _edit = Ui.Button("Edit project", ShowEditor);
            _cancel = Ui.Button("Stop", () => { _engine.Cancel(); SetBusy(false, "Stopped."); });
            _cancel.Visibility = Visibility.Collapsed;
            var newMenu = new ContextMenu();
            newMenu.Items.Add(MenuItem("New schema comparison", () => New(false)));
            newMenu.Items.Add(MenuItem("New data comparison", () => New(true)));
            var newButton = Ui.Button("New ▾", () => { });
            newButton.ContextMenu = newMenu;
            newButton.Click += (s, e) => { newMenu.PlacementTarget = newButton; newMenu.IsOpen = true; };
            _recent.SelectionChanged += async (s, e) =>
            {
                if (_recent.SelectedItem is string path && File.Exists(path))
                {
                    _recent.SelectedIndex = -1;
                    await Open(path);
                }
            };
            var toolbar = Ui.Row(
                newButton,
                Ui.Button("Open…", async () => await OpenDialog()),
                Ui.Button("Save", async () => await Save(false)),
                Ui.Button("Save as…", async () => await Save(true)),
                _recent,
                new Border { Width = 16 },
                _edit, _compare, _deploy, _cancel);
            var header = new DockPanel { Margin = new Thickness(8, 6, 8, 6) };
            DockPanel.SetDock(toolbar, Dock.Right);
            header.Children.Add(toolbar);
            header.Children.Add(Ui.Column(_title, _status));
            var headerBorder = new Border { Child = header, Background = CompareTheme.Panel, BorderBrush = CompareTheme.Border, BorderThickness = new Thickness(0, 0, 0, 1) };
            SetDock(headerBorder, Dock.Top);
            Children.Add(headerBorder);
            Children.Add(_content);
            RefreshRecent();
        }

        private static MenuItem MenuItem(string text, Action click)
        {
            var m = new MenuItem { Header = text };
            m.Click += (s, e) => click();
            return m;
        }

        private void RefreshRecent()
        {
            _recent.ItemsSource = CompareSettings.Load().Recent.Where(File.Exists).ToList();
        }

        private async Task<JsonObject> OptionDefinitions()
        {
            if (_optionDefs == null)
            {
                _optionDefs = await Ui.Try(_host.Owner, () => _engine.RequestObject("options")) ?? new JsonObject();
            }
            return _optionDefs;
        }

        /// <summary>Starts a new comparison, optionally with the source (and target) filled in.</summary>
        public async void New(bool data, JsonObject source = null, string sourcePassword = null)
        {
            var defs = await OptionDefinitions();
            var project = CompareSession.NewProject(data, defs);
            if (source != null)
            {
                project["source"] = source;
                // The target starts on the same server, which is the usual case.
                var target = source.Clone();
                target["database"] = "";
                project["target"] = target;
            }
            _session = new CompareSession(_engine, _host, project) { OptionDefinitions = defs };
            if (sourcePassword != null)
            {
                _session.Passwords["source"] = sourcePassword;
                _session.Passwords["target"] = sourcePassword;
            }
            _dirty = false;
            ShowEditor();
        }

        /// <summary>Opens the last project, or a new schema comparison.</summary>
        public async Task Start()
        {
            var settings = CompareSettings.Load();
            var last = settings.Recent.FirstOrDefault(File.Exists);
            if (settings.ReopenLast && last != null) await Open(last);
            else New(false);
        }

        public async Task Open(string path)
        {
            var defs = await OptionDefinitions();
            var project = await Ui.Try(_host.Owner, () => _engine.RequestObject("project_load", new JsonObject { ["path"] = path }));
            if (project == null) return;
            var imported = path.EndsWith(".scp", StringComparison.OrdinalIgnoreCase) || path.EndsWith(".sdc", StringComparison.OrdinalIgnoreCase);
            _session = new CompareSession(_engine, _host, project) { OptionDefinitions = defs, FilePath = imported ? null : path };
            if (!imported) CompareSettings.Load().AddRecent(path);
            RefreshRecent();
            _dirty = imported;
            ShowEditor();
            if (imported) _status.Text = "Imported from " + Path.GetFileName(path) + ". Passwords are not imported; save as a QuillSQL project.";
        }

        private async Task OpenDialog()
        {
            var d = new OpenFileDialog
            {
                Filter = "Compare projects (*.qsc, *.qdc)|*.qsc;*.qdc|SQL Compare and Data Compare projects (*.scp, *.sdc)|*.scp;*.sdc|All files|*.*",
            };
            if (d.ShowDialog(_host.Owner) == true) await Open(d.FileName);
        }

        private async Task Save(bool saveAs)
        {
            if (_session == null) return;
            _editor?.Collect();
            CollectSelection();
            var path = _session.FilePath;
            if (saveAs || path == null)
            {
                var ext = _session.IsData ? "qdc" : "qsc";
                var d = new SaveFileDialog
                {
                    Filter = _session.IsData ? "Data compare project (*.qdc)|*.qdc" : "Schema compare project (*.qsc)|*.qsc",
                    DefaultExt = ext,
                    FileName = SuggestedName() + "." + ext,
                };
                if (d.ShowDialog(_host.Owner) != true) return;
                path = d.FileName;
            }
            var ok = await Ui.Try(_host.Owner, () => _engine.Request("project_save", new JsonObject { ["path"] = path, ["project"] = _session.Project }));
            if (ok == null) return;
            _session.FilePath = path;
            _dirty = false;
            CompareSettings.Load().AddRecent(path);
            RefreshRecent();
            UpdateTitle();
            _status.Text = "Saved " + path;
        }

        private string SuggestedName()
        {
            string db(string side) => _session.Project.Obj(side)?.Str("database") ?? "";
            var name = db("source") + "_to_" + db("target");
            foreach (var c in Path.GetInvalidFileNameChars()) name = name.Replace(c, '_');
            return name == "_to_" ? "Comparison" : name;
        }

        private void UpdateTitle()
        {
            if (_session == null) return;
            string side(string key)
            {
                var s = _session.Project.Obj(key);
                if (s == null) return "?";
                return s.Str("type") == "snapshot" ? Path.GetFileName(s.Str("path")) : (s.Str("server") + " / " + s.Str("database"));
            }
            var kind = _session.IsData ? "Data compare" : "Schema compare";
            var file = _session.FilePath != null ? Path.GetFileName(_session.FilePath) : "New project";
            _title.Text = kind + ": " + side("source") + "  →  " + side("target");
            TitleChanged?.Invoke((_dirty ? "*" : "") + file + " - QuillSQL " + kind);
        }

        private void ShowEditor()
        {
            if (_session == null) return;
            _editor = new ProjectEditor(_session, async () => await Compare(), () => { if (_schemaResults != null || _dataResults != null) ShowResults(); });
            _content.Content = _editor;
            _deploy.IsEnabled = false;
            _edit.IsEnabled = false;
            UpdateTitle();
        }

        private void ShowResults()
        {
            _content.Content = (UIElement)_schemaResults ?? _dataResults;
            _deploy.IsEnabled = true;
            _edit.IsEnabled = true;
        }

        private void SetBusy(bool busy, string text)
        {
            _status.Text = text;
            _compare.IsEnabled = !busy;
            _deploy.IsEnabled = !busy && (_schemaResults != null || _dataResults != null);
            _cancel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
            Cursor = busy ? System.Windows.Input.Cursors.AppStarting : null;
        }

        private void CollectSelection()
        {
            if (_session == null) return;
            if (_schemaResults != null && !_session.IsData) _session.Project["deselected"] = _schemaResults.Deselected;
        }

        private void Progress(string text) => Dispatcher.BeginInvoke(new Action(() => _status.Text = text + "…"));

        public async Task Compare()
        {
            if (_session == null) return;
            _editor?.Collect();
            var source = _session.Project.Obj("source");
            var target = _session.Project.Obj("target");
            bool incomplete(JsonObject s) => s == null || (s.Str("type") == "snapshot" ? string.IsNullOrEmpty(s.Str("path")) : string.IsNullOrEmpty(s.Str("server")) || string.IsNullOrEmpty(s.Str("database")));
            if (incomplete(source) || incomplete(target))
            {
                Ui.Error(_host.Owner, "Choose a server and database (or a snapshot) for both the source and the target.");
                if (!(_content.Content is ProjectEditor)) ShowEditor();
                return;
            }
            SetBusy(true, "Comparing…");
            var op = _session.IsData ? "data_compare" : "schema_compare";
            var result = await Ui.Try(_host.Owner, () => _engine.RequestObject(op, new JsonObject { ["project"] = _session.WithPasswords() }, Progress));
            if (result == null)
            {
                SetBusy(false, "The comparison did not finish.");
                return;
            }
            _schemaResults = null;
            _dataResults = null;
            if (_session.IsData) _dataResults = new DataResults(_session, result);
            else _schemaResults = new SchemaResults(_session, result);
            SetBusy(false, "Compared at " + DateTime.Now.ToShortTimeString() + ".");
            ShowResults();
            UpdateTitle();
        }

        private async Task Deploy()
        {
            if (_session == null || (_schemaResults == null && _dataResults == null)) return;
            JsonObject script;
            SetBusy(true, "Creating the deployment script…");
            if (_schemaResults != null)
            {
                var selected = _schemaResults.Selected;
                if (selected.Count == 0)
                {
                    SetBusy(false, "Tick the objects to deploy first.");
                    return;
                }
                script = await Ui.Try(_host.Owner, () => _engine.RequestObject("schema_script", new JsonObject
                {
                    ["session"] = _schemaResults.Session,
                    ["selected"] = selected,
                    ["project"] = _session.Project,
                }));
            }
            else
            {
                script = await Ui.Try(_host.Owner, () => _engine.RequestObject("data_script", new JsonObject
                {
                    ["session"] = _dataResults.Session,
                    ["tables"] = _dataResults.SelectedTables,
                    ["kinds"] = new List<object> { false, true, true, true },
                }));
            }
            SetBusy(false, "");
            if (script == null) return;
            var session = _schemaResults?.Session ?? _dataResults.Session;
            var target = _session.Project.Obj("target");
            var targetName = target?.Str("type") == "snapshot" ? null : target?.Str("server") + " / " + target?.Str("database");
            var wizard = new DeployWindow(_host, script, targetName);
            if (wizard.Run() != DeployWindow.Choice.Deploy) return;
            SetBusy(true, "Deploying…");
            var done = await Ui.Try(_host.Owner, () => _engine.RequestObject("deploy", new JsonObject { ["session"] = session }, Progress));
            SetBusy(false, done == null ? "The deployment failed and was rolled back." : "Deployed (" + done.Long("batches") + " batches). Compare again to check.");
            if (done != null && Ui.Confirm(_host.Owner, "The deployment finished. Compare again now?")) await Compare();
        }

        /// <summary>Asks before discarding unsaved project changes.</summary>
        public bool CanClose() => !_dirty || Ui.Confirm(_host.Owner, "The project has unsaved changes. Close it anyway?");
    }

    /// <summary>Review the script and warnings, then deploy, open the script, or save it.</summary>
    internal sealed class DeployWindow
    {
        public enum Choice { None, Deploy }

        private readonly ICompareHost _host;
        private readonly JsonObject _script;
        private readonly string _target;

        public DeployWindow(ICompareHost host, JsonObject script, string target)
        {
            _host = host;
            _script = script;
            _target = target;
        }

        public Choice Run()
        {
            var choice = Choice.None;
            var sql = _script.Str("sql") ?? "";
            var warnings = _script.List("warnings").OfType<JsonObject>().ToList();
            var tabs = new TabControl();
            var code = Ui.Code();
            code.Text = sql.Length == 0 ? "-- Nothing to deploy." : sql;
            tabs.Items.Add(new TabItem { Header = "Deployment script", Content = code });
            var list = new ListBox();
            foreach (var w in warnings)
            {
                var sev = w.Str("severity");
                var item = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(2, 4, 2, 4) };
                item.Inlines.Add(new System.Windows.Documents.Run(sev + "  ") { FontWeight = FontWeights.SemiBold, Foreground = sev == "High" ? CompareTheme.High : sev == "Medium" ? CompareTheme.Medium : CompareTheme.Muted });
                item.Inlines.Add(new System.Windows.Documents.Run((string.IsNullOrEmpty(w.Str("object")) ? "" : w.Str("object") + ": ") + w.Str("message")));
                list.Items.Add(item);
            }
            var warningTab = new TabItem { Header = "Warnings (" + warnings.Count + ")", Content = list };
            tabs.Items.Add(warningTab);
            if (warnings.Any(w => w.Str("severity") == "High")) tabs.SelectedItem = warningTab;

            var window = Ui.Dialog(_host.Owner, "Deploy", null, 980, 680);
            var heading = Ui.Text(_target == null ? "The target is a snapshot: the script can be saved or opened, not run." : "Review the changes to " + _target + ".", 14, bold: true);
            heading.Margin = new Thickness(0, 0, 0, 8);
            var deploy = Ui.Button("Deploy now", () =>
            {
                var high = warnings.Count(w => w.Str("severity") == "High");
                var message = "Run this script on " + _target + " now?" + (high > 0 ? "\n\nThere are " + high + " high-severity warnings: data may be lost." : "");
                if (Ui.Confirm(window, message))
                {
                    choice = Choice.Deploy;
                    window.Close();
                }
            }, primary: true, minWidth: 110);
            deploy.IsEnabled = _target != null && sql.Length > 0;
            var open = Ui.Button("Open in editor", () => { _host.OpenScript(sql, "Deployment script"); window.Close(); }, minWidth: 110);
            var save = Ui.Button("Save script…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = "deploy.sql" };
                if (d.ShowDialog(window) == true) File.WriteAllText(d.FileName, sql, new System.Text.UTF8Encoding(true));
            }, minWidth: 100);
            var copy = Ui.Button("Copy", () => Clipboard.SetText(sql));
            var close = Ui.Button("Close", () => window.Close());
            var buttons = Ui.Row(copy, save, open, deploy, close);
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            var root = new DockPanel { Margin = new Thickness(12) };
            DockPanel.SetDock(heading, Dock.Top);
            DockPanel.SetDock(buttons, Dock.Bottom);
            root.Children.Add(heading);
            root.Children.Add(buttons);
            root.Children.Add(tabs);
            window.Content = root;
            window.ShowDialog();
            return choice;
        }
    }
}
