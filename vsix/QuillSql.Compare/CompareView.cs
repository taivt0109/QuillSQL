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
        private readonly TextBlock _title = Ui.Title("");
        private readonly TextBlock _sourceName = Ui.Text(""), _targetName = Ui.Text("");
        private readonly TextBlock _status = Ui.Text("", brush: CompareTheme.Muted);
        private readonly ProgressBar _busy = new ProgressBar { IsIndeterminate = true, Visibility = Visibility.Hidden, Margin = new Thickness(0) };
        private readonly Button _compare, _deploy, _edit, _cancel;
        private readonly ContextMenu _openMenu = new ContextMenu();
        private JsonObject _optionDefs;
        private bool _dirty;

        /// <summary>Raised when the title changes (project name, kind, dirty mark).</summary>
        public event Action<string> TitleChanged;

        public CompareView(Engine engine, ICompareHost host)
        {
            _engine = engine;
            _host = host;
            CompareTheme.Attach(this);
            SetResourceReference(BackgroundProperty, "Q.Bg");
            SetResourceReference(TextElement.ForegroundProperty, "Q.Text");

            _compare = Ui.Button("Compare", async () => await Compare(), primary: true, minWidth: 110, glyph: Glyph.Play);
            _deploy = Ui.Button("Deploy…", async () => await Deploy(), minWidth: 100, glyph: Glyph.Upload);
            _edit = Ui.Command(Glyph.Edit, "Edit project", ShowEditor);
            _cancel = Ui.Button("Stop", () => { _engine.Cancel(); SetBusy(false, "Stopped."); }, minWidth: 90, glyph: Glyph.Stop);
            _cancel.Visibility = Visibility.Collapsed;

            var newMenu = new ContextMenu();
            newMenu.Items.Add(Ui.MenuItem("Schema comparison", () => New(false), Glyph.List));
            newMenu.Items.Add(Ui.MenuItem("Data comparison", () => New(true), Glyph.Document));
            _openMenu.Opened += (s, e) => FillOpenMenu();
            var commands = Ui.Row(
                Ui.MenuButton(Glyph.Add, "New", newMenu),
                Ui.MenuButton(Glyph.Open, "Open", _openMenu),
                Ui.Command(Glyph.Save, "Save", async () => await Save(false), "Save the project (Ctrl+S)"),
                Ui.Command(Glyph.SaveAs, "Save as", async () => await Save(true)),
                Separator(),
                _edit);
            var actions = Ui.Row(_cancel, _deploy, _compare);
            var bar = new DockPanel { Margin = new Thickness(10, 8, 16, 0) };
            DockPanel.SetDock(actions, Dock.Right);
            bar.Children.Add(actions);
            bar.Children.Add(commands);

            // The page header: what is compared with what.
            var arrow = Ui.Icon(Glyph.Forward, 12, CompareTheme.Muted);
            arrow.Margin = new Thickness(10, 1, 10, 0);
            _sourceName.Foreground = _targetName.Foreground = CompareTheme.Muted;
            var sides = Ui.Row(Ui.Icon(Glyph.Server, 13, CompareTheme.Muted), new Border { Width = 6 }, _sourceName, arrow, Ui.Icon(Glyph.Server, 13, CompareTheme.Muted), new Border { Width = 6 }, _targetName);
            _status.HorizontalAlignment = HorizontalAlignment.Right;
            _status.TextTrimming = TextTrimming.CharacterEllipsis;
            var subtitle = new DockPanel();
            DockPanel.SetDock(sides, Dock.Left);
            subtitle.Children.Add(sides);
            subtitle.Children.Add(_status);
            var header = Ui.Column(_title, subtitle);
            header.Margin = new Thickness(28, 14, 28, 6);

            var top = Ui.Column(bar, header, _busy);
            SetDock(top, Dock.Top);
            Children.Add(top);
            Children.Add(_content);
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await Save(false)), System.Windows.Input.Key.S, System.Windows.Input.ModifierKeys.Control));
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await OpenDialog()), System.Windows.Input.Key.O, System.Windows.Input.ModifierKeys.Control));
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await Compare()), System.Windows.Input.Key.F5, System.Windows.Input.ModifierKeys.None));
        }

        private static UIElement Separator()
        {
            var line = new Border { Width = 1, Height = 20, Margin = new Thickness(8, 0, 8, 0) };
            line.SetResourceReference(Border.BackgroundProperty, "Q.Divider");
            return line;
        }

        private sealed class Relay : System.Windows.Input.ICommand
        {
            private readonly Action _run;
            public Relay(Action run) => _run = run;
            public event EventHandler CanExecuteChanged { add { } remove { } }
            public bool CanExecute(object parameter) => true;
            public void Execute(object parameter) => _run();
        }

        /// <summary>Open…, then the recent projects.</summary>
        private void FillOpenMenu()
        {
            _openMenu.Items.Clear();
            _openMenu.Items.Add(Ui.MenuItem("Open project…", async () => await OpenDialog(), Glyph.Open, "Ctrl+O"));
            var recent = CompareSettings.Load().Recent.Where(File.Exists).Take(10).ToList();
            if (recent.Count == 0) return;
            _openMenu.Items.Add(new Separator());
            foreach (var path in recent)
            {
                var item = Ui.MenuItem(Path.GetFileName(path), async () => await Open(path), path.EndsWith(".qdc", StringComparison.OrdinalIgnoreCase) ? Glyph.Document : Glyph.List);
                item.ToolTip = path;
                _openMenu.Items.Add(item);
            }
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
            _title.Text = (_session.FilePath != null ? Path.GetFileNameWithoutExtension(_session.FilePath) : kind) + (_dirty ? " •" : "");
            _sourceName.Text = side("source");
            _targetName.Text = side("target");
            TitleChanged?.Invoke((_dirty ? "*" : "") + file + " - QuillSQL " + kind);
        }

        private void ShowEditor()
        {
            if (_session == null) return;
            _editor = new ProjectEditor(_session, async () => await Compare(), () => { if (_schemaResults != null || _dataResults != null) ShowResults(); });
            _content.Content = _editor;
            _deploy.IsEnabled = false;
            _edit.Visibility = Visibility.Collapsed;
            UpdateTitle();
        }

        private void ShowResults()
        {
            _content.Content = (UIElement)_schemaResults ?? _dataResults;
            _deploy.IsEnabled = true;
            _edit.Visibility = Visibility.Visible;
        }

        private void SetBusy(bool busy, string text)
        {
            _status.Text = text;
            _compare.IsEnabled = !busy;
            _deploy.IsEnabled = !busy && (_schemaResults != null || _dataResults != null);
            _cancel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
            _busy.Visibility = busy ? Visibility.Visible : Visibility.Hidden;
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
            var high = warnings.Count(w => w.Str("severity") == "High");
            var tabs = new TabControl();
            var code = Ui.Code();
            code.Text = sql.Length == 0 ? "-- Nothing to deploy." : sql;
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Code, "Deployment script"), Content = code });
            var list = new StackPanel { Margin = new Thickness(16, 12, 16, 12) };
            foreach (var w in warnings)
            {
                var sev = w.Str("severity");
                var brush = sev == "High" ? CompareTheme.High : sev == "Medium" ? CompareTheme.Medium : CompareTheme.Muted;
                var icon = Ui.Icon(sev == "Low" ? Glyph.Info : Glyph.Warning, 15, brush);
                icon.VerticalAlignment = VerticalAlignment.Top;
                icon.Margin = new Thickness(0, 2, 12, 0);
                var text = new TextBlock { TextWrapping = TextWrapping.Wrap };
                text.Inlines.Add(new Run((string.IsNullOrEmpty(w.Str("object")) ? sev : w.Str("object"))) { FontWeight = FontWeights.SemiBold });
                text.Inlines.Add(new LineBreak());
                text.Inlines.Add(new Run(w.Str("message")) { Foreground = CompareTheme.Muted });
                var item = new DockPanel { Margin = new Thickness(0, 0, 0, 14) };
                DockPanel.SetDock(icon, Dock.Left);
                item.Children.Add(icon);
                item.Children.Add(text);
                list.Children.Add(item);
            }
            if (warnings.Count == 0) list.Children.Add(Ui.Text("No warnings.", brush: CompareTheme.Muted));
            var warningTab = new TabItem
            {
                Header = Ui.IconText(Glyph.Warning, "Warnings (" + warnings.Count + ")"),
                Content = new ScrollViewer { Content = list, VerticalScrollBarVisibility = ScrollBarVisibility.Auto },
            };
            tabs.Items.Add(warningTab);
            if (high > 0) tabs.SelectedItem = warningTab;

            var window = Ui.Dialog(_host.Owner, "Deploy", null, 1000, 700);
            var heading = Ui.Title(_target == null ? "Deployment script" : "Deploy to " + _target);
            var lead = Ui.Text(
                _target == null
                    ? "The target is a snapshot: the script can be saved or opened, not run."
                    : "Review the script and its warnings. It runs in one transaction and is rolled back if any statement fails.",
                brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            lead.Margin = new Thickness(0, 4, 0, 14);
            var top = Ui.Column(heading, lead);
            if (high > 0)
            {
                var alert = Ui.Row(Ui.Icon(Glyph.Warning, 14, CompareTheme.High), new Border { Width = 8 },
                    Ui.Text(Ui.Plural(high, "high-severity warning", "high-severity warnings") + ": data may be lost.", brush: CompareTheme.High));
                alert.Margin = new Thickness(0, 0, 0, 12);
                top.Children.Add(alert);
            }
            var card = Ui.Card(tabs, 0);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 20) };
            DockPanel.SetDock(top, Dock.Top);
            body.Children.Add(top);
            body.Children.Add(card);

            var deploy = Ui.Button("Deploy now", () =>
            {
                var message = "Run this script on " + _target + " now?" + (high > 0 ? "\n\nThere are " + high + " high-severity warnings: data may be lost." : "");
                if (Ui.Confirm(window, message))
                {
                    choice = Choice.Deploy;
                    window.Close();
                }
            }, primary: true, minWidth: 120, glyph: Glyph.Upload);
            deploy.IsEnabled = _target != null && sql.Length > 0;
            var open = Ui.Button("Open in editor", () => { _host.OpenScript(sql, "Deployment script"); window.Close(); }, minWidth: 110, glyph: Glyph.Edit);
            var save = Ui.Button("Save script…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = "deploy.sql" };
                if (d.ShowDialog(window) == true) File.WriteAllText(d.FileName, sql, new System.Text.UTF8Encoding(true));
            }, minWidth: 100, glyph: Glyph.Save);
            var copy = Ui.Button("Copy", () => Clipboard.SetText(sql), glyph: Glyph.Copy);
            var close = Ui.Button("Close", () => window.Close());
            close.IsCancel = true;
            var left = Ui.Row(copy, save, open);
            left.Margin = new Thickness(-8, 0, 0, 0);
            var right = Ui.Row(deploy, close);
            var buttons = new DockPanel();
            DockPanel.SetDock(right, Dock.Right);
            buttons.Children.Add(right);
            buttons.Children.Add(left);
            window.Content = Ui.WithFooter(body, buttons);
            window.ShowDialog();
            return choice;
        }
    }
}
