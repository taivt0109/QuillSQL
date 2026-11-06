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
            _switch = Ui.Command(Glyph.Switch, "Switch direction", async () => await SwitchDirection(), "Switch deployment direction (Ctrl+D): the target becomes the source");
            _toolsMenu.Opened += (s, e) => FillToolsMenu();
            _tools = Ui.MenuButton(Glyph.Report, "Tools", _toolsMenu);
            var commands = Ui.Row(
                Ui.MenuButton(Glyph.Add, "New", newMenu),
                Ui.MenuButton(Glyph.Open, "Open", _openMenu),
                Ui.Command(Glyph.Save, "Save", async () => await Save(false), "Save the project (Ctrl+S)"),
                Ui.Command(Glyph.SaveAs, "Save as", async () => await Save(true)),
                Ui.Command(Glyph.Recent, "My projects", async () => await MyProjects(), "My projects (Ctrl+M)"),
                Separator(),
                _edit,
                _switch,
                _tools);
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
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await SwitchDirection()), System.Windows.Input.Key.D, System.Windows.Input.ModifierKeys.Control));
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await MyProjects()), System.Windows.Input.Key.M, System.Windows.Input.ModifierKeys.Control));
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(async () => await Deploy()), System.Windows.Input.Key.F8, System.Windows.Input.ModifierKeys.None));
            InputBindings.Add(new System.Windows.Input.KeyBinding(new Relay(() => { if (_session != null) ShowEditor(); }), System.Windows.Input.Key.E, System.Windows.Input.ModifierKeys.Control));
            ShowResultCommands(false);
        }

        private readonly Button _switch, _tools;
        private readonly ContextMenu _toolsMenu = new ContextMenu();

        private void ShowResultCommands(bool shown)
        {
            _switch.Visibility = shown ? Visibility.Visible : Visibility.Collapsed;
        }

        /// <summary>Reports, exports and saved results.</summary>
        private void FillToolsMenu()
        {
            _toolsMenu.Items.Clear();
            if (_session != null && !_session.IsData)
            {
                _toolsMenu.Items.Add(Ui.MenuItem("Create scripts folder…", async () => await CreateScriptsFolder(), Glyph.Folder));
                _toolsMenu.Items.Add(Ui.MenuItem("Create snapshot…", async () => { _editor?.Collect(); await Dialogs.CreateSnapshot(_session); }, Glyph.Save));
            }
            if (_schemaResults != null)
            {
                _toolsMenu.Items.Add(Ui.MenuItem("Create report…", async () => await Dialogs.SchemaReport(_session, _schemaResults), Glyph.Report));
            }
            if (_dataResults != null)
            {
                var identicalStored = _session.Project.Strings("options").Contains("IncludeIdenticalRecords");
                _toolsMenu.Items.Add(Ui.MenuItem("Export CSV files…", async () => await Dialogs.ExportCsv(_session, _dataResults, identicalStored), Glyph.Export));
                _toolsMenu.Items.Add(Ui.MenuItem("HTML report…", async () => await Dialogs.DataReport(_session, _dataResults), Glyph.Report));
                _toolsMenu.Items.Add(new Separator());
                _toolsMenu.Items.Add(Ui.MenuItem("Save results…", async () => await SaveResults(), Glyph.Save));
            }
            _toolsMenu.Items.Add(new Separator());
            _toolsMenu.Items.Add(Ui.MenuItem("Application options…", () => Dialogs.ApplicationOptions(_host.Owner), Glyph.Settings));
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
            _openMenu.Items.Add(Ui.MenuItem("Open saved data comparison…", async () => await OpenResults(), Glyph.Document));
            _openMenu.Items.Add(Ui.MenuItem("My projects…", async () => await MyProjects(), Glyph.Recent, "Ctrl+M"));
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
            if (settings.ShowMyProjects && await MyProjects()) return;
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
            if (Dialogs.Show(d, _host.Owner) == true) await Open(d.FileName);
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
                    Filter = _session.IsData
                        ? "Data compare project (*.qdc)|*.qdc|SQL Data Compare project (*.sdc)|*.sdc"
                        : "Schema compare project (*.qsc)|*.qsc|SQL Compare project (*.scp)|*.scp",
                    DefaultExt = ext,
                    FileName = SuggestedName() + "." + ext,
                };
                if (Dialogs.Show(d, _host.Owner) != true) return;
                path = d.FileName;
            }
            // SQL Compare and SQL Data Compare projects are written in their format, without passwords.
            var vendor = path.EndsWith(".scp", StringComparison.OrdinalIgnoreCase) || path.EndsWith(".sdc", StringComparison.OrdinalIgnoreCase);
            var ok = await Ui.Try(_host.Owner, () => _engine.Request(vendor ? "project_export" : "project_save", new JsonObject { ["path"] = path, ["project"] = _session.Project }));
            if (ok == null) return;
            if (vendor)
            {
                _status.Text = "Saved " + path + " for SQL Compare. Keep saving as a QuillSQL project to keep every setting.";
                return;
            }
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
                return s.Str("type") == "snapshot" ? Path.GetFileName(s.Str("path")) : s.Str("type") == "scripts" ? s.Str("path") + (string.IsNullOrEmpty(s.Str("revision")) ? "" : " @ " + s.Str("revision")) : s.Str("type") == "empty" ? "Empty database" : (s.Str("server") + " / " + s.Str("database"));
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
            ShowResultCommands(false);
            UpdateTitle();
        }

        private void ShowResults()
        {
            _content.Content = (UIElement)_schemaResults ?? _dataResults;
            _deploy.IsEnabled = true;
            _edit.Visibility = Visibility.Visible;
            ShowResultCommands(true);
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
            bool incomplete(JsonObject s) => s == null || (s.Str("type") == "empty" ? false : s.Str("type") == "snapshot" || s.Str("type") == "scripts" ? string.IsNullOrEmpty(s.Str("path")) : string.IsNullOrEmpty(s.Str("server")) || string.IsNullOrEmpty(s.Str("database")));
            if (incomplete(source) || incomplete(target))
            {
                Ui.Error(_host.Owner, "Choose a server and database (or a snapshot or scripts folder) for both the source and the target.");
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
            ShowComparison(result);
            SetBusy(false, "Compared at " + DateTime.Now.ToShortTimeString() + ".");
            ShowResults();
            UpdateTitle();
        }

        /// <summary>Writes the source or the target as a scripts folder in SQL Compare's layout.</summary>
        private async Task CreateScriptsFolder()
        {
            _editor?.Collect();
            var choice = Ui.Message(_host.Owner, "Create scripts folder", "Create a scripts folder with the objects of which data source?", Ui.Tone.Question, "Source", "Target", "Cancel");
            if (choice < 0 || choice == 2) return;
            var side = _session.WithPasswords().Obj(choice == 0 ? "source" : "target");
            if (side == null || side.Str("type") == "empty")
            {
                Ui.Error(_host.Owner, "Choose a database, snapshot or scripts folder first.");
                return;
            }
            var folder = Dialogs.PickFolder(_host.Owner, Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare"), "Create scripts folder in");
            if (folder == null) return;
            if (Directory.Exists(folder) && Directory.EnumerateFileSystemEntries(folder).Any() && !Ui.Confirm(_host.Owner, "The folder " + folder + " is not empty. Write the scripts into it anyway?")) return;
            SetBusy(true, "Creating the scripts folder…");
            var project = _session.WithPasswords();
            project["source"] = side;
            var done = await Ui.Try(_host.Owner, () => _engine.RequestObject("make_scripts", new JsonObject { ["project"] = project, ["path"] = folder }, Progress));
            SetBusy(false, done == null ? "The scripts folder was not created." : "Created the scripts folder " + folder + " (" + Ui.Plural(done.Long("files"), "file", "files") + ").");
        }

        /// <summary>Shows a comparison reply as results.</summary>
        private void ShowComparison(JsonObject result)
        {
            if (_session.IsData)
            {
                _dataResults = new DataResults(_session, result);
            }
            else
            {
                _schemaResults = new SchemaResults(_session, result);
                _schemaResults.ReviewRenames += async renames => await MapRenames(renames);
                _schemaResults.FilterChanged += () => { _dirty = true; UpdateTitle(); };
            }
        }

        /// <summary>Confirmed renames become table mappings, and the databases are compared again.</summary>
        private async Task MapRenames(List<JsonObject> renames)
        {
            var chosen = Dialogs.ConfirmRenames(_host.Owner, renames);
            if (chosen == null || chosen.Count == 0) return;
            var mappings = _session.Project.Obj("schemaMappings") ?? new JsonObject();
            var tables = mappings.List("tables");
            foreach (var r in chosen) tables.Add(new JsonObject { ["source"] = r.Obj("source"), ["target"] = r.Obj("target"), ["columns"] = new List<object>() });
            mappings["tables"] = tables;
            _session.Project["schemaMappings"] = mappings;
            _dirty = true;
            _editor = null;
            await Compare();
        }

        /// <summary>Switch deployment direction: the target becomes the source. Schema results are compared again from what was read; data is compared again.</summary>
        private async Task SwitchDirection()
        {
            if (_session == null || (_schemaResults == null && _dataResults == null)) return;
            if (_session.Project.Obj("target")?.Str("type") == "empty")
            {
                Ui.Error(_host.Owner, "An empty database cannot be the source.");
                return;
            }
            CollectSelection();
            if (_schemaResults != null)
            {
                SetBusy(true, "Switching deployment direction…");
                var result = await Ui.Try(_host.Owner, () => _engine.RequestObject("schema_switch", new JsonObject { ["session"] = _schemaResults.Session }));
                if (result == null)
                {
                    SetBusy(false, "");
                    return;
                }
                SwapSides();
                _schemaResults = null;
                ShowComparison(result);
                SetBusy(false, "Deployment direction switched: " + result.Str("targetName") + " is now the target.");
                ShowResults();
                UpdateTitle();
                return;
            }
            SwapSides();
            await Compare();
        }

        /// <summary>Swaps source and target in the project, with their passwords and mappings.</summary>
        private void SwapSides()
        {
            var p = _session.Project;
            var source = p.Obj("source");
            p["source"] = p.Obj("target");
            p["target"] = source;
            _session.Passwords.TryGetValue("source", out var sp);
            _session.Passwords.TryGetValue("target", out var tp);
            _session.Passwords["source"] = tp;
            _session.Passwords["target"] = sp;
            var mappings = p.Obj("schemaMappings");
            if (mappings != null)
            {
                foreach (var pair in mappings.List("schemas").OfType<List<object>>()) pair.Reverse();
                foreach (var t in mappings.List("tables").OfType<JsonObject>())
                {
                    var s = t.Obj("source");
                    t["source"] = t.Obj("target");
                    t["target"] = s;
                    foreach (var c in t.List("columns").OfType<List<object>>()) c.Reverse();
                }
            }
            _dirty = true;
            _editor = null;
        }

        /// <summary>Opens My projects; true when a project was opened or started from it.</summary>
        private async Task<bool> MyProjects()
        {
            var choice = Dialogs.MyProjects(_engine, _host.Owner);
            if (choice == null) return false;
            if (choice.NewData.HasValue)
            {
                New(choice.NewData.Value);
                return true;
            }
            await Open(choice.Path);
            if (choice.CompareNow) await Compare();
            return true;
        }

        /// <summary>Saves the open data comparison, rows and all, to open it again later.</summary>
        private async Task SaveResults()
        {
            if (_dataResults == null) return;
            var d = new SaveFileDialog { Filter = "Saved data comparison (*.qdcr)|*.qdcr", DefaultExt = "qdcr", FileName = SuggestedName() + ".qdcr" };
            if (Dialogs.Show(d, _host.Owner) != true) return;
            var ok = await Ui.Try(_host.Owner, () => _engine.Request("data_session_save", new JsonObject { ["session"] = _dataResults.Session, ["path"] = d.FileName }));
            if (ok != null) _status.Text = "Saved the results to " + d.FileName;
        }

        private async Task OpenResults()
        {
            var d = new OpenFileDialog { Filter = "Saved data comparison (*.qdcr)|*.qdcr|All files|*.*" };
            if (Dialogs.Show(d, _host.Owner) != true) return;
            if (!CanClose()) return;
            var defs = await OptionDefinitions();
            SetBusy(true, "Opening " + Path.GetFileName(d.FileName) + "…");
            var result = await Ui.Try(_host.Owner, () => _engine.RequestObject("data_session_load", new JsonObject { ["path"] = d.FileName }));
            SetBusy(false, result == null ? "" : "Opened results saved in " + d.FileName + ".");
            if (result == null) return;
            _session = new CompareSession(_engine, _host, result.Obj("project") ?? CompareSession.NewProject(true, defs)) { OptionDefinitions = defs };
            _dirty = false;
            _schemaResults = null;
            _editor = null;
            ShowComparison(result);
            ShowResults();
            UpdateTitle();
        }

        private async Task Deploy()
        {
            if (_session == null || (_schemaResults == null && _dataResults == null)) return;
            var target = _session.Project.Obj("target");
            var source = _session.Project.Obj("source");
            string describe(JsonObject s) => s?.Str("type") == "snapshot" ? Path.GetFileName(s.Str("path"))
                : s?.Str("type") == "scripts" ? s.Str("path") + (string.IsNullOrEmpty(s.Str("revision")) ? "" : " @ " + s.Str("revision"))
                : s?.Str("type") == "empty" ? "Empty database" : s?.Str("server") + " / " + s?.Str("database");
            // A scripts folder is updated file by file; a revision of one can only be scripted.
            var folder = target?.Str("type") == "scripts";
            var live = target != null && target.Str("type") != "snapshot" && target.Str("type") != "empty" && !(folder && !string.IsNullOrEmpty(target.Str("revision")));
            DeployWizard wizard;
            long session;
            if (_schemaResults != null)
            {
                var results = _schemaResults;
                var selected = results.Selected;
                if (selected.Count == 0)
                {
                    Ui.Error(_host.Owner, "You have not selected any objects to deploy.");
                    return;
                }
                session = results.Session;
                SetBusy(true, "Finding dependencies…");
                var deps = await Ui.Try(_host.Owner, async () => await _engine.Request("schema_dependencies", new JsonObject { ["session"] = session, ["selected"] = selected }) as List<object>);
                SetBusy(false, "");
                string action(string kind) => kind == "OnlyIn1" ? "Create" : kind == "OnlyIn2" ? "Drop" : "Alter";
                string name(JsonObject d) => (string.IsNullOrEmpty(d.Str("schema")) ? "" : "[" + d.Str("schema") + "].") + "[" + d.Str("name") + "]";
                var changes = selected.Select(i => results.Difference((int)i)).Where(d => d != null)
                    .Select(d => new DeployWizard.Change { Object = name(d), Type = d.Str("type"), Action = action(d.Str("kind")), Index = (int)d.Long("index") }).ToList();
                var dependencies = (deps ?? new List<object>()).OfType<JsonObject>().Select(x =>
                {
                    var d = results.Difference((int)x.Long("index"));
                    return new DeployWizard.Dependency { Index = (int)x.Long("index"), Name = name(d), Type = d.Str("type"), Action = action(d.Str("kind")), ReferencedBy = string.Join(", ", x.Strings("referencedBy")) };
                }).ToList();
                var options = _session.Project.Strings("options");
                wizard = new DeployWizard(_host, describe(source), live ? describe(target) : null, Ui.Plural(changes.Count, "object", "objects") + " selected for deployment", changes, dependencies, options.Contains("IncludeDependencies"),
                    async (all, extra, backup) =>
                    {
                        var project = _session.Project.Clone();
                        var opts = project.Strings("options").Where(o => o != "IncludeDependencies").Cast<object>().ToList();
                        if (all) opts.Add("IncludeDependencies");
                        project["options"] = opts;
                        var ids = selected.Concat(all ? new List<object>() : extra.Select(e => (object)e)).ToList();
                        return await Ui.Try(_host.Owner, () => _engine.RequestObject("schema_script", new JsonObject { ["session"] = session, ["selected"] = ids, ["project"] = project, ["backup"] = backup, ["isolationLevel"] = CompareSettings.Load().IsolationLevel }));
                    });
            }
            else
            {
                var results = _dataResults;
                if (results.SelectedRows == 0)
                {
                    Ui.Error(_host.Owner, "You have not selected any objects to deploy.");
                    return;
                }
                session = results.Session;
                var changes = new List<DeployWizard.Change>();
                foreach (var t in results.Tables.Where(t => t.SelectedTotal > 0))
                {
                    foreach (var (kind, verb) in new[] { (DataResults.OnlyInSource, "Insert"), (DataResults.Different, "Update"), (DataResults.OnlyInTarget, "Delete") })
                    {
                        var n = t.SelectedCount(kind);
                        if (n > 0) changes.Add(new DeployWizard.Change { Object = t.Name + " (" + Ui.Plural(n, "row", "rows") + ")", Type = "Table", Action = verb });
                    }
                }
                wizard = new DeployWizard(_host, describe(source), live ? describe(target) : null, results.SelectedRows.ToString("N0") + " rows selected for deployment", changes, null, false,
                    async (all, extra, backup) => await Ui.Try(_host.Owner, () => _engine.RequestObject("data_script", new JsonObject
                    {
                        ["session"] = session,
                        ["tables"] = results.SelectedTables,
                        ["kinds"] = new List<object> { false, true, true, true },
                        ["selections"] = results.Selections(),
                        ["backup"] = backup,
                        ["isolationLevel"] = CompareSettings.Load().IsolationLevel,
                        ["maxTransactionSizeMB"] = CompareSettings.Load().SplitTransactions ? CompareSettings.Load().MaxTransactionSizeMB : 0,
                    })));
            }
            if (folder) wizard.DeployLabel = "Update the scripts folder";
            else if (live) wizard.BackupDefaults = async () => await Ui.Try(_host.Owner, () => _engine.RequestObject("backup_defaults", new JsonObject { ["session"] = session }));
            wizard.BackupSettings = _session.Project.Obj("backup");
            var outcome = wizard.Run();
            if (outcome.Choice == DeployWizard.Choice.None) return;
            // The backup settings belong to the project, as in SQL Compare.
            if (outcome.Backup != null && Json.Write(outcome.Backup) != Json.Write(_session.Project.Obj("backup") ?? new JsonObject { ["enabled"] = false }))
            {
                _session.Project["backup"] = outcome.Backup;
                _dirty = true;
                UpdateTitle();
            }
            var sql = outcome.Script.Str("sql") ?? "";
            if (outcome.Choice == DeployWizard.Choice.OpenScript)
            {
                _host.OpenScript(sql, "Deployment script");
                return;
            }
            if (outcome.SaveCopyTo != null)
            {
                try
                {
                    Directory.CreateDirectory(Path.GetDirectoryName(outcome.SaveCopyTo));
                    File.WriteAllText(outcome.SaveCopyTo, sql, new System.Text.UTF8Encoding(true));
                }
                catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException)
                {
                    Ui.Error(_host.Owner, "The copy of the script was not saved: " + ex.Message);
                }
            }
            SetBusy(true, "Deploying…");
            var request = new JsonObject { ["session"] = session };
            var done = await Ui.Try(_host.Owner, () => _engine.RequestObject("deploy", request, Progress));
            SetBusy(false, done == null ? (folder ? "The scripts folder was not updated." : "The deployment failed and was rolled back.")
                : done.ContainsKey("written") ? "Updated the scripts folder: " + Ui.Plural(done.List("written").Count, "file written", "files written") + ", " + done.List("deleted").Count + " deleted."
                : "Deployed (" + done.Long("batches") + " batches).");
            if (done == null) return;
            if (outcome.Recompare) await Compare();
            Ui.Message(_host.Owner, "QuillSQL Compare", "Deployment was successful.", Ui.Tone.Info, "OK");
        }

        /// <summary>Asks before discarding unsaved project changes.</summary>
        public bool CanClose() => !_dirty || Ui.Confirm(_host.Owner, "The project has unsaved changes. Close it anyway?");
    }
}
