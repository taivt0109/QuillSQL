using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>The smaller windows of the Compare window: export, reports, renames, column mapping and My projects.</summary>
    internal static class Dialogs
    {
        private static DockPanel Body(string title, string lead, UIElement content)
        {
            var heading = Ui.Title(title, 18);
            var text = Ui.Text(lead, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            text.Margin = new Thickness(0, 4, 0, 14);
            var top = Ui.Column(heading, text);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            DockPanel.SetDock(top, Dock.Top);
            body.Children.Add(top);
            body.Children.Add(content);
            return body;
        }

        private static StackPanel Buttons(params UIElement[] buttons)
        {
            var row = Ui.Row(buttons);
            row.HorizontalAlignment = HorizontalAlignment.Right;
            row.Margin = new Thickness(0);
            return row;
        }

        /// <summary>
        /// Shows a file dialog. For UI tests, when QUILL_TEST_DIALOGS names a folder holding
        /// `file-dialog.txt`, that file's text is the answer instead (empty = cancel) and it is removed.
        /// </summary>
        public static bool? Show(FileDialog d, Window owner)
        {
            var folder = Environment.GetEnvironmentVariable("QUILL_TEST_DIALOGS");
            var answer = string.IsNullOrEmpty(folder) ? null : Path.Combine(folder, "file-dialog.txt");
            if (answer != null && File.Exists(answer))
            {
                var path = File.ReadAllText(answer).Trim();
                File.Delete(answer);
                if (path.Length == 0) return false;
                d.FileName = path;
                return true;
            }
            return owner != null ? d.ShowDialog(owner) : d.ShowDialog();
        }
        private static readonly List<WeakReference<ContextMenu>> Menus = new List<WeakReference<ContextMenu>>();
        private static System.Windows.Threading.DispatcherTimer _menuTimer;

        /// <summary>
        /// A context menu UI tests can use without a mouse or keyboard: with QUILL_TEST_DIALOGS set,
        /// the text of `menu.txt` in that folder names a menu item to click (the file is removed).
        /// </summary>
        public static void RegisterMenu(ContextMenu menu)
        {
            var folder = Environment.GetEnvironmentVariable("QUILL_TEST_DIALOGS");
            if (string.IsNullOrEmpty(folder)) return;
            Menus.Add(new WeakReference<ContextMenu>(menu));
            if (_menuTimer != null) return;
            _menuTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromMilliseconds(300) };
            _menuTimer.Tick += (s, e) =>
            {
                var file = Path.Combine(folder, "menu.txt");
                if (!File.Exists(file)) return;
                var name = File.ReadAllText(file).Trim();
                File.Delete(file);
                foreach (var weak in Menus)
                {
                    if (!weak.TryGetTarget(out var m) || !(m.PlacementTarget is UIElement target ? target.IsVisible : true)) continue;
                    var item = m.Items.OfType<MenuItem>().FirstOrDefault(i => (i.Header as string) == name && i.IsEnabled);
                    if (item == null) continue;
                    item.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
                    return;
                }
            };
            _menuTimer.Start();
        }
        /// <summary>A folder chosen through a save dialog (WPF has no folder picker).</summary>
        public static string PickFolder(Window owner, string start, string title)
        {
            var d = new SaveFileDialog { Title = title, FileName = "Choose this folder", Filter = "Folders|*.none", InitialDirectory = Directory.Exists(start) ? start : null, CheckPathExists = true, OverwritePrompt = false };
            return Dialogs.Show(d, owner) == true ? Path.GetDirectoryName(d.FileName) : null;
        }

        public static void OpenFolder(string folder)
        {
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", "\"" + folder + "\"") { UseShellExecute = true });
            }
            catch (System.ComponentModel.Win32Exception)
            {
            }
        }

        public static void OpenFile(string path)
        {
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(path) { UseShellExecute = true });
            }
            catch (System.ComponentModel.Win32Exception)
            {
            }
        }

        // ---- Export CSV files (data) ----

        private sealed class ExportRow
        {
            public bool Export { get; set; } = true;
            public string Name { get; set; }
            public int Index { get; set; }
            public bool HasDifferences { get; set; }
        }

        public static async Task ExportCsv(CompareSession session, DataResults results, bool identicalStored)
        {
            var owner = session.Host.Owner;
            var rows = results.Tables.Where(t => t.Comparable).Select(t => new ExportRow { Name = t.Name, Index = t.Index, HasDifferences = t.AllDifferent > 0 }).ToList();
            var w = Ui.Dialog(owner, "Export CSV files", null, 600, 640);
            w.MinWidth = 380;
            w.MinHeight = 460;
            var grid = new DataGrid { ItemsSource = rows, AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column };
            grid.Columns.Add(Ui.CheckColumn("Export", "Export", 70));
            grid.Columns.Add(new DataGridTextColumn { Header = "Name", Binding = new Binding("Name"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            void pick(Func<ExportRow, bool> f)
            {
                foreach (var r in rows) r.Export = f(r);
                grid.Items.Refresh();
            }
            var selectors = Ui.Row(
                new Button { Content = "Select all", Style = CompareTheme.Style("Q.SubtleButton"), ToolTip = "Selects all tables and views in the list below." },
                new Button { Content = "none", Style = CompareTheme.Style("Q.SubtleButton"), ToolTip = "Deselects all tables and views in the list below." },
                new Button { Content = "with differences", Style = CompareTheme.Style("Q.SubtleButton"), ToolTip = "Selects only the tables and views with differences in their rows." });
            ((Button)selectors.Children[0]).Click += (s, e) => pick(r => true);
            ((Button)selectors.Children[1]).Click += (s, e) => pick(r => false);
            ((Button)selectors.Children[2]).Click += (s, e) => pick(r => r.HasDifferences);
            var identical = Ui.Check("Identical", identicalStored);
            identical.IsEnabled = identicalStored;
            if (!identicalStored) identical.ToolTip = "Identical rows were not kept: turn on 'Show identical values in results' and compare again.";
            var different = Ui.Check("Different", true);
            var only1 = Ui.Check("Only in " + DataResults.Plain(results.SourceName), true);
            var only2 = Ui.Check("Only in " + DataResults.Plain(results.TargetName), true);
            var folder = new TextBox { Text = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare", "Reports") };
            var browse = Ui.Button("Browse…", () => { var f = PickFolder(w, folder.Text, "Select folder for exports"); if (f != null) folder.Text = f; });
            var folderRow = new DockPanel();
            DockPanel.SetDock(browse, Dock.Right);
            folderRow.Children.Add(browse);
            folderRow.Children.Add(folder);
            var bottom = Ui.Form(Ui.Field("Rows to include", Ui.Row(identical, different, only1, only2)), Ui.Field("Export to folder", folderRow));
            bottom.Margin = new Thickness(0, 12, 0, 0);
            var content = new DockPanel();
            DockPanel.SetDock(selectors, Dock.Top);
            DockPanel.SetDock(bottom, Dock.Bottom);
            content.Children.Add(selectors);
            content.Children.Add(bottom);
            content.Children.Add(Ui.Card(grid, 4));
            var generate = Ui.Button("Generate", () => { }, primary: true, minWidth: 110);
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            generate.Click += async (s, e) =>
            {
                var dir = folder.Text.Trim();
                if (dir.Length == 0) return;
                if (!Directory.Exists(dir))
                {
                    if (!Ui.Confirm(w, "The folder " + dir + " does not exist.\nWould you like to create it now?")) return;
                    Directory.CreateDirectory(dir);
                }
                var existing = Directory.GetFiles(dir, "*.csv");
                if (existing.Length > 0)
                {
                    var choice = Ui.Message(w, "Export CSV files", "There are existing CSV files in your chosen export directory: " + dir + "\n\nWould you like to delete all existing CSV files or overwrite each file?", Ui.Tone.Question, "Overwrite", "Delete all", "Cancel");
                    if (choice < 0 || choice == 2) return;
                    if (choice == 1) foreach (var f in existing) File.Delete(f);
                }
                generate.IsEnabled = false;
                var started = DateTime.UtcNow.AddSeconds(-2);
                var ok = await Ui.Try(w, async () => (object)await session.Engine.Request("data_export", new JsonObject
                {
                    ["session"] = results.Session,
                    ["folder"] = dir,
                    ["kinds"] = new List<object> { identical.IsChecked == true, different.IsChecked == true, only1.IsChecked == true, only2.IsChecked == true },
                    ["tables"] = rows.Where(r => r.Export).Select(r => (object)r.Index).ToList(),
                }));
                generate.IsEnabled = true;
                if (ok == null) return;
                // The files written are re-encoded as Application options say.
                var encoding = CompareSettings.EncodingOf(CompareSettings.Load().CsvEncoding);
                foreach (var f in Directory.GetFiles(dir, "*.csv").Where(f => File.GetLastWriteTimeUtc(f) >= started))
                {
                    try
                    {
                        File.WriteAllText(f, File.ReadAllText(f), encoding);
                    }
                    catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException)
                    {
                    }
                }
                w.Close();
                OpenFolder(dir);
            };
            w.Content = Ui.WithFooter(Body("Export CSV files", "Choose the tables and views you want to export. Separate .csv files will be created for each table or view.", content), Buttons(generate, cancel));
            w.ShowDialog();
        }

        // ---- Application options ----

        private static readonly (string Label, string Value)[] Encodings = { ("UTF-8 with byte order mark", "utf8bom"), ("UTF-8", "utf8"), ("Unicode (UTF-16)", "unicode"), ("ASCII", "ascii") };

        /// <summary>SQL Server Management Studio installations: (label, Ssms.exe).</summary>
        public static List<(string Label, string Path)> FindSsms()
        {
            var found = new List<(string, string)>();
            var roots = new[] { Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86), Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles) };
            foreach (var version in new[] { "22", "21", "20", "19", "18" })
            {
                foreach (var root in roots.Where(r => !string.IsNullOrEmpty(r)))
                {
                    foreach (var sub in new[] { @"Common7\IDE\Ssms.exe", @"Release\Common7\IDE\SSMS.exe" })
                    {
                        var path = System.IO.Path.Combine(root, "Microsoft SQL Server Management Studio " + version, sub);
                        if (File.Exists(path) && !found.Any(f => string.Equals(f.Item2, path, StringComparison.OrdinalIgnoreCase))) found.Add(("SQL Server Management Studio " + version, path));
                    }
                }
            }
            return found;
        }

        /// <summary>"Application options": deployment scripts, the SQL editor, encodings and default folders.</summary>
        public static void ApplicationOptions(Window owner, bool standalone = true)
        {
            var settings = CompareSettings.Load();
            var w = Ui.Dialog(owner, "Application options", null, 640, 760);
            var levels = new[] { ("Read uncommitted", "READ UNCOMMITTED"), ("Read committed", "READ COMMITTED"), ("Repeatable read", "REPEATABLE READ"), ("Snapshot", "SNAPSHOT"), ("Serializable", "SERIALIZABLE") };
            var level = new ComboBox { ItemsSource = levels.Select(l => l.Item1).ToList(), SelectedIndex = Math.Max(0, Array.FindIndex(levels, l => l.Item2 == settings.IsolationLevel)) };
            var split = Ui.Check("Split transactions / batch operations (data deployments)", settings.SplitTransactions);
            var size = new TextBox { Text = settings.MaxTransactionSizeMB.ToString(), Width = 80, HorizontalAlignment = HorizontalAlignment.Left, IsEnabled = settings.SplitTransactions };
            split.Checked += (s, e) => size.IsEnabled = true;
            split.Unchecked += (s, e) => size.IsEnabled = false;
            var status = Ui.Text("", brush: CompareTheme.High);
            DockPanel folderRow(TextBox box, string title)
            {
                var b = Ui.Button("Browse…", () => { var f = PickFolder(w, box.Text, title); if (f != null) box.Text = f; });
                var row = new DockPanel();
                DockPanel.SetDock(b, Dock.Right);
                row.Children.Add(b);
                row.Children.Add(box);
                return row;
            }
            ComboBox encodingBox(string current) => new ComboBox { ItemsSource = Encodings.Select(e => e.Label).ToList(), SelectedIndex = Math.Max(0, Array.FindIndex(Encodings, e => e.Value == current)) };
            var scriptFolder = new TextBox { Text = settings.ScriptFolder };
            var updateFolder = Ui.Check("Update the save location to the last folder used", settings.UpdateScriptFolder);
            var scriptEncoding = encodingBox(settings.ScriptEncoding);
            var csvEncoding = encodingBox(settings.CsvEncoding);
            var filterFolder = new TextBox { Text = settings.FilterFolder };
            // The SQL editor (QuillCompare.exe; in SSMS scripts open in a query window connected to the target).
            var ssms = FindSsms();
            var editors = new List<string> { "The program registered for .sql files" };
            editors.AddRange(ssms.Select(s => s.Label));
            editors.Add("Custom editor");
            var editor = new ComboBox { ItemsSource = editors };
            var at = ssms.FindIndex(s => string.Equals(s.Path, settings.SqlEditor, StringComparison.OrdinalIgnoreCase));
            editor.SelectedIndex = settings.SqlEditor == "custom" ? editors.Count - 1 : at >= 0 ? at + 1 : 0;
            var exe = new TextBox { Text = settings.CustomEditor };
            var exeBrowse = Ui.Button("Browse…", () =>
            {
                var d = new OpenFileDialog { Filter = "Programs (*.exe)|*.exe|All files|*.*", FileName = exe.Text };
                if (Show(d, w) == true) exe.Text = d.FileName;
            });
            var exeRow = new DockPanel();
            DockPanel.SetDock(exeBrowse, Dock.Right);
            exeRow.Children.Add(exeBrowse);
            exeRow.Children.Add(exe);
            var args = new TextBox { Text = settings.CustomEditorArguments };
            var placeholders = Ui.Text("%SF% script file, %SN% server, %DN% database, %UN% user name of the target.", 12, brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
            var custom = Ui.Form(Ui.Field("Executable", exeRow), Ui.Field("Arguments", args), Ui.Field(null, placeholders));
            void showCustom() => custom.Visibility = editor.SelectedIndex == editors.Count - 1 ? Visibility.Visible : Visibility.Collapsed;
            editor.SelectionChanged += (s, e) => showCustom();
            showCustom();
            var editorPart = Ui.Column(Ui.Form(Ui.Field("Open deployment scripts in", editor)), custom);
            if (!standalone) editorPart.Visibility = Visibility.Collapsed;
            var heading = new Func<string, TextBlock>(t => { var h = Ui.Text(t, 15, bold: true); h.Margin = new Thickness(0, 6, 0, 8); return h; });
            var form = Ui.Column(
                heading("Deployment scripts"),
                Ui.Form(
                    Ui.Field("Transaction isolation level", level),
                    Ui.Field(null, split),
                    Ui.Field("Maximum transaction / batch size (MB)", size),
                    Ui.Field("Save deployment scripts in", folderRow(scriptFolder, "Default deployment script folder")),
                    Ui.Field(null, updateFolder),
                    Ui.Field("Deployment script encoding", scriptEncoding)),
                heading("SQL editor"),
                editorPart,
                heading("Files"),
                Ui.Form(
                    Ui.Field("Filter files folder", folderRow(filterFolder, "Default filter folder")),
                    Ui.Field("CSV export encoding", csvEncoding),
                    Ui.Field(null, status)));
            var ok = Ui.Button("OK", () =>
            {
                if (!int.TryParse(size.Text.Trim(), out var mb) || mb <= 0)
                {
                    status.Text = "The maximum transaction size must be a positive whole number.";
                    return;
                }
                if (editor.SelectedIndex == editors.Count - 1 && !File.Exists(exe.Text.Trim()))
                {
                    status.Text = "Choose the custom editor's program.";
                    return;
                }
                settings.IsolationLevel = levels[Math.Max(0, level.SelectedIndex)].Item2;
                settings.SplitTransactions = split.IsChecked == true;
                settings.MaxTransactionSizeMB = mb;
                settings.ScriptFolder = scriptFolder.Text.Trim();
                settings.UpdateScriptFolder = updateFolder.IsChecked == true;
                settings.ScriptEncoding = Encodings[Math.Max(0, scriptEncoding.SelectedIndex)].Value;
                settings.CsvEncoding = Encodings[Math.Max(0, csvEncoding.SelectedIndex)].Value;
                settings.FilterFolder = filterFolder.Text.Trim();
                settings.SqlEditor = editor.SelectedIndex == 0 ? "" : editor.SelectedIndex == editors.Count - 1 ? "custom" : ssms[editor.SelectedIndex - 1].Path;
                settings.CustomEditor = exe.Text.Trim();
                settings.CustomEditorArguments = args.Text;
                settings.Save();
                w.Close();
            }, primary: true, minWidth: 100);
            ok.IsDefault = true;
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            var scroll = new ScrollViewer { Content = form, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
            w.Content = Ui.WithFooter(Body("Application options", "How deployment scripts are written and opened, and where files go. Kept for you, not in the project.", scroll), Buttons(ok, cancel));
            w.ShowDialog();
        }

        // ---- New database ----

        public sealed class DatabaseFile
        {
            public string Name { get; set; }
            public string Kind { get; set; }
            public string Path { get; set; }
            public int SizeMB { get; set; } = 8;
            public int GrowthMB { get; set; } = 64;
        }

        /// <summary>
        /// "New database": name, collation (the server's default marked) and the data and log files.
        /// Returns the CREATE DATABASE statement, or null when cancelled.
        /// </summary>
        public static string NewDatabase(Window owner, string server, string suggested, JsonObject info, out string name)
        {
            name = null;
            var w = Ui.Dialog(owner, "New database", null, 760, 600);
            var nameBox = new TextBox { Text = suggested ?? "" };
            var instanceDefault = info.Str("default") ?? "";
            var collations = new List<string> { "<instance default: " + instanceDefault + ">" };
            collations.AddRange(info.Strings("collations"));
            var collation = new ComboBox { ItemsSource = collations, SelectedIndex = 0, IsEditable = true };
            var dataPath = info.Str("dataPath") ?? "";
            var logPath = info.Str("logPath") ?? dataPath;
            var files = new System.Collections.ObjectModel.ObservableCollection<DatabaseFile>();
            // The server's folders may be Linux paths.
            string join(string dir, string file) => dir.EndsWith("/") || dir.EndsWith("\\") ? dir + file : dir + (dir.Contains("/") && !dir.Contains("\\") ? "/" : "\\") + file;
            void defaults()
            {
                files.Clear();
                var n = nameBox.Text.Trim();
                if (dataPath.Length == 0) return;
                files.Add(new DatabaseFile { Name = n, Kind = "Data", Path = join(dataPath, n + ".mdf") });
                files.Add(new DatabaseFile { Name = n + "_log", Kind = "Log", Path = join(logPath, n + "_log.ldf") });
            }
            defaults();
            var changed = false;
            nameBox.TextChanged += (s, e) => { if (!changed) defaults(); };
            var grid = new DataGrid { ItemsSource = files, AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column, MinHeight = 140 };
            grid.Columns.Add(new DataGridTextColumn { Header = "Logical name", Binding = new Binding("Name"), Width = 150 });
            grid.Columns.Add(new DataGridComboBoxColumn { Header = "Type", SelectedItemBinding = new Binding("Kind"), ItemsSource = new[] { "Data", "Log", "Filestream" }, Width = 90 });
            grid.Columns.Add(new DataGridTextColumn { Header = "File name", Binding = new Binding("Path"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            grid.Columns.Add(new DataGridTextColumn { Header = "Size (MB)", Binding = new Binding("SizeMB"), Width = 80 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Growth (MB)", Binding = new Binding("GrowthMB"), Width = 90 });
            grid.CellEditEnding += (s, e) => changed = true;
            var add = Ui.Button("Add file", () =>
            {
                changed = true;
                var n = nameBox.Text.Trim() + "_" + (files.Count + 1);
                files.Add(new DatabaseFile { Name = n, Kind = "Data", Path = join(dataPath, n + ".ndf") });
            }, glyph: Glyph.Add);
            var remove = Ui.Button("Remove file", () => { if (grid.SelectedItem is DatabaseFile f) { changed = true; files.Remove(f); } }, glyph: Glyph.Delete);
            var restore = Ui.Button("Restore defaults", () => { changed = false; collation.SelectedIndex = 0; defaults(); });
            var status = Ui.Text("", brush: CompareTheme.High, wrap: TextWrapping.Wrap);
            var content = new DockPanel();
            var top = Ui.Form(Ui.Field("Name", nameBox), Ui.Field("Collation", collation), Ui.Field("Files", Ui.Row(add, remove, restore)));
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(status, Dock.Bottom);
            content.Children.Add(top);
            content.Children.Add(status);
            content.Children.Add(Ui.Card(grid, 4));
            string sql = null;
            var create = Ui.Button("Create", () =>
            {
                grid.CommitEdit(DataGridEditingUnit.Row, true);
                var n = nameBox.Text.Trim();
                if (n.Length == 0) { status.Text = "Enter a name for the database."; return; }
                if (files.Count > 0 && (files.All(f => f.Kind != "Data") || files.All(f => f.Kind != "Log"))) { status.Text = "A database needs a data file and a log file."; return; }
                sql = CreateDatabaseSql(n, collation.SelectedIndex == 0 ? null : collation.Text.Trim(), changed || files.Count == 0 ? files.ToList() : null);
                w.Close();
            }, primary: true, minWidth: 100);
            create.IsDefault = true;
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            w.Content = Ui.WithFooter(Body("New database", "An empty database on " + server + ". The files are on the server, in its folders.", content), Buttons(create, cancel));
            w.ShowDialog();
            if (sql != null) name = nameBox.Text.Trim();
            return sql;
        }

        /// <summary>CREATE DATABASE with a collation (null: the server's) and files (null: the server's defaults).</summary>
        public static string CreateDatabaseSql(string name, string collation, List<DatabaseFile> files)
        {
            string q(string s) => "[" + s.Replace("]", "]]") + "]";
            string lit(string s) => "N'" + (s ?? "").Replace("'", "''") + "'";
            string spec(DatabaseFile f) => "(NAME = " + lit(f.Name) + ", FILENAME = " + lit(f.Path) + (f.Kind == "Filestream" ? "" : ", SIZE = " + Math.Max(1, f.SizeMB) + "MB, FILEGROWTH = " + Math.Max(0, f.GrowthMB) + "MB") + ")";
            var sql = "CREATE DATABASE " + q(name);
            if (files != null && files.Count > 0)
            {
                var data = files.Where(f => f.Kind == "Data").ToList();
                var stream = files.Where(f => f.Kind == "Filestream").ToList();
                var log = files.Where(f => f.Kind == "Log").ToList();
                sql += "\nON PRIMARY\n" + string.Join(",\n", data.Select(spec));
                if (stream.Count > 0) sql += ",\nFILEGROUP " + q(name + "_FileStream") + " CONTAINS FILESTREAM\n" + string.Join(",\n", stream.Select(spec));
                sql += "\nLOG ON\n" + string.Join(",\n", log.Select(spec));
            }
            if (!string.IsNullOrEmpty(collation)) sql += "\nCOLLATE " + collation;
            return sql;
        }

        // ---- Source control revision ----

        /// <summary>The folder's history in source control; returns the chosen revision, or null.</summary>
        public static string ChooseRevision(Window owner, string folder, List<JsonObject> revisions)
        {
            string chosen = null;
            var w = Ui.Dialog(owner, "Choose revision", null, 820, 520);
            var grid = new DataGrid { ItemsSource = revisions.Select(r => new { Revision = r.Str("id"), Date = r.Str("date"), Author = r.Str("author"), Message = r.Str("message") }).ToList(), AutoGenerateColumns = false, IsReadOnly = true, SelectionMode = DataGridSelectionMode.Single, HeadersVisibility = DataGridHeadersVisibility.Column };
            grid.Columns.Add(new DataGridTextColumn { Header = "Revision", Binding = new Binding("Revision"), Width = 110 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Date", Binding = new Binding("Date"), Width = 130 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Author", Binding = new Binding("Author"), Width = 140 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Message", Binding = new Binding("Message"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            void pick()
            {
                if (grid.SelectedIndex < 0) return;
                chosen = revisions[grid.SelectedIndex].Str("id");
                w.Close();
            }
            grid.MouseDoubleClick += (s, e) => pick();
            var latest = Ui.Button("Latest (HEAD)", () => { chosen = "HEAD"; w.Close(); });
            var ok = Ui.Button("OK", pick, primary: true, minWidth: 100);
            ok.IsDefault = true;
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            var lead = revisions.Count == 0 ? "The folder has no history yet." : "The latest revisions of " + folder + ", newest first, and its tags.";
            w.Content = Ui.WithFooter(Body("Choose revision", lead, Ui.Card(grid, 4)), Buttons(latest, ok, cancel));
            w.ShowDialog();
            return chosen;
        }

        // ---- Microsoft Entra sign-in with MFA ----

        /// <summary>The code to enter at Microsoft's sign-in page, shown while the engine waits; close it when the request ends.</summary>
        public static Window DeviceCode(Window owner, string uri, string code, string message)
        {
            var w = Ui.Dialog(owner, "Sign in to Microsoft Entra", null, 520, 0);
            w.SizeToContent = SizeToContent.Height;
            var codeText = Ui.Title(code ?? "", 26);
            codeText.Margin = new Thickness(0, 6, 0, 10);
            var lead = Ui.Text(message ?? ("Open " + uri + " in a browser and enter the code."), wrap: TextWrapping.Wrap);
            var copy = Ui.Button("Copy code", () => { try { Clipboard.SetText(code ?? ""); } catch (System.Runtime.InteropServices.COMException) { } }, glyph: Glyph.Copy);
            var open = Ui.Button("Open sign-in page", () => { try { Clipboard.SetText(code ?? ""); } catch (System.Runtime.InteropServices.COMException) { } OpenFile(uri); }, primary: true, glyph: Glyph.Forward);
            var close = Ui.Button("Hide", () => w.Close());
            close.IsCancel = true;
            var body = Ui.Column(lead, codeText, Ui.Text("QuillSQL Compare goes on once you have signed in.", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap));
            body.Margin = new Thickness(24, 20, 24, 16);
            w.Content = Ui.WithFooter(body, Buttons(copy, open, close));
            w.Show();
            return w;
        }

        // ---- Unsaved changes ----

        public enum SaveChoice { Save, DontSave, Cancel }

        /// <summary>"Save project before closing?" Yes, No or Cancel.</summary>
        public static SaveChoice AskToSave(Window owner, string project, string doing = "closing it")
        {
            var i = Ui.Message(owner, "QuillSQL Compare", "Save the changes to " + project + " before " + doing + "?", Ui.Tone.Question, "Save", "Don't save", "Cancel");
            return i == 0 ? SaveChoice.Save : i == 1 ? SaveChoice.DontSave : SaveChoice.Cancel;
        }

        // ---- Create snapshot (schema) ----

        /// <summary>Saves the schema of the source or the target as a snapshot file.</summary>
        public static async Task CreateSnapshot(CompareSession session)
        {
            var owner = session.Host.Owner;
            var choice = Ui.Message(owner, "Create snapshot", "Create a snapshot of which data source?", Ui.Tone.Question, "Source", "Target", "Cancel");
            if (choice < 0 || choice == 2) return;
            var side = session.WithPasswords().Obj(choice == 0 ? "source" : "target");
            if (side == null || side.Str("type") == "empty")
            {
                Ui.Error(owner, "Choose a database, snapshot or scripts folder first.");
                return;
            }
            var d = new SaveFileDialog { Filter = "QuillSQL snapshot (*.json)|*.json", DefaultExt = "json", FileName = (side.Str("database") ?? "snapshot") + ".json" };
            if (Show(d, owner) != true) return;
            var done = await Ui.Try(owner, () => session.Engine.RequestObject("snapshot", new JsonObject { ["source"] = side, ["path"] = d.FileName }));
            if (done != null) Ui.Message(owner, "Create snapshot", "Saved the snapshot " + d.FileName + " (" + Ui.Plural(done.Long("tables"), "table", "tables") + ").", Ui.Tone.Info, "OK");
        }

        /// <summary>A script to read, copy, save or open: "View object deployment script".</summary>
        /// <summary>Opens a script, connected to the target when the host can.</summary>
        public static void OpenScript(ICompareHost host, string sql, string title, JsonObject target)
        {
            if (host is IConnectedScriptHost connected) connected.OpenScript(sql, title, target);
            else host.OpenScript(sql, title);
        }

        public static void ShowScript(ICompareHost host, string title, string sql, JsonObject target = null)
        {
            var w = Ui.Dialog(host.Owner, title, null, 900, 620);
            var code = Ui.Code();
            code.Text = string.IsNullOrEmpty(sql) ? "-- Nothing to deploy." : sql;
            var copy = Ui.Button("Copy", () => Clipboard.SetText(code.Text), glyph: Glyph.Copy);
            var save = Ui.Button("Save script…", () =>
            {
                var d = new SaveFileDialog { Filter = "SQL script (*.sql)|*.sql", FileName = "script.sql" };
                if (Show(d, w) == true) File.WriteAllText(d.FileName, code.Text, CompareSettings.EncodingOf(CompareSettings.Load().ScriptEncoding));
            }, glyph: Glyph.Save);
            var open = Ui.Button("Open in editor", () => { OpenScript(host, code.Text, title, target); w.Close(); }, glyph: Glyph.Edit);
            var close = Ui.Button("Close", () => w.Close(), primary: true);
            close.IsCancel = true;
            w.Content = Ui.WithFooter(Body(title, "The script that deploys this object on its own, with what it needs.", Ui.Card(code, 0)), Buttons(copy, save, open, close));
            w.ShowDialog();
        }

        // ---- Create report (schema) ----

        public static async Task SchemaReport(CompareSession session, SchemaResults results)
        {
            var owner = session.Host.Owner;
            var settings = CompareSettings.Load();
            var w = Ui.Dialog(owner, "Create report", null, 560, 0);
            w.SizeToContent = SizeToContent.Height;
            var formats = new[] { ("Interactive HTML", "Html", ".html"), ("Classic HTML", "Classic", ".html"), ("XML for Excel", "Excel", ".xml"), ("Excel workbook", "Xlsx", ".xlsx"), ("XML", "Xml", ".xml") };
            var format = new ComboBox { ItemsSource = formats.Select(f => f.Item1).ToList(), SelectedIndex = 0 };
            var name = new TextBox { Text = "SchemaCompareReport" };
            var folder = new TextBox { Text = Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory) };
            var browse = Ui.Button("Browse…", () => { var f = PickFolder(w, folder.Text, "Choose folder for comparison report"); if (f != null) folder.Text = f; });
            var folderRow = new DockPanel();
            DockPanel.SetDock(browse, Dock.Right);
            folderRow.Children.Add(browse);
            folderRow.Children.Add(folder);
            var withDifferences = new RadioButton { Content = "Objects with differences", IsChecked = true, Margin = new Thickness(0, 2, 0, 2) };
            var selected = new RadioButton { Content = "Objects selected for deployment", Margin = new Thickness(0, 2, 0, 2) };
            var identical = Ui.Check("Also include identical objects", false);
            var view = Ui.Check("View report on completion", settings.ViewReport);
            var form = Ui.Form(
                Ui.Field("Report format", format),
                Ui.Field("Name", name),
                Ui.Field("Create in", folderRow),
                Ui.Field("Include", Ui.Column(withDifferences, selected, identical)),
                Ui.Field(null, view));
            var generate = Ui.Button("Generate", () => { }, primary: true, minWidth: 110);
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            generate.Click += async (s, e) =>
            {
                var f = formats[Math.Max(0, format.SelectedIndex)];
                if (!Directory.Exists(folder.Text) || name.Text.Trim().Length == 0 || name.Text.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0)
                {
                    Ui.Error(w, "Choose a folder that exists and a file name without \\ / : * ? \" < > |.");
                    return;
                }
                var path = Path.Combine(folder.Text, name.Text.Trim() + f.Item3);
                if (File.Exists(path) && !Ui.Confirm(w, "The file " + path + " already exists. Do you want to overwrite it?")) return;
                var request = new JsonObject { ["session"] = results.Session, ["path"] = path, ["format"] = f.Item2, ["includeIdentical"] = identical.IsChecked == true };
                if (selected.IsChecked == true) request["selected"] = results.Selected;
                generate.IsEnabled = false;
                var ok = await Ui.Try(w, async () => (object)await session.Engine.Request("schema_report", request));
                generate.IsEnabled = true;
                if (ok == null) return;
                settings.ViewReport = view.IsChecked == true;
                settings.Save();
                w.Close();
                if (view.IsChecked == true) OpenFile(path);
            };
            w.Content = Ui.WithFooter(Body("Create report", "A report of the comparison: every object that differs and its SQL, line against line.", form), Buttons(generate, cancel));
            w.ShowDialog();
            await Task.CompletedTask;
        }

        /// <summary>The data comparison as an HTML page.</summary>
        public static async Task DataReport(CompareSession session, DataResults results)
        {
            var d = new SaveFileDialog { Filter = "HTML report (*.html)|*.html", FileName = "DataCompareReport.html" };
            if (Dialogs.Show(d, session.Host.Owner) != true) return;
            var ok = await Ui.Try(session.Host.Owner, async () => (object)await session.Engine.Request("data_report", new JsonObject { ["session"] = results.Session, ["path"] = d.FileName }));
            if (ok != null) OpenFile(d.FileName);
        }

        // ---- Suggested table renames ----

        private sealed class RenameRow
        {
            public bool Map { get; set; } = true;
            public string Source { get; set; }
            public string Target { get; set; }
            public string Reason { get; set; }
            public JsonObject Data { get; set; }
        }

        private static string QName(JsonObject n) => n == null ? "" : "[" + n.Str("schema") + "].[" + n.Str("name") + "]";

        /// <summary>Lets the user confirm suggested renames; returns the ones to map, or null.</summary>
        public static List<JsonObject> ConfirmRenames(Window owner, List<JsonObject> renames)
        {
            List<JsonObject> chosen = null;
            var rows = renames.Select(r => new RenameRow
            {
                Source = QName(r.Obj("source")),
                Target = QName(r.Obj("target")),
                Reason = r.Str("reason") == "identical" ? "identical except for the name" : r.Str("reason") == "script" ? "similar definition (" + r.Long("similarity") + "%)" : "similar name (" + r.Long("similarity") + "% alike)",
                Data = r,
                Map = r.Str("reason") != "name",
            }).ToList();
            var w = Ui.Dialog(owner, "Table renames", null, 820, 480);
            var grid = new DataGrid { ItemsSource = rows, AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column };
            grid.Columns.Add(Ui.CheckColumn("Map", "Map", 60));
            grid.Columns.Add(new DataGridTextColumn { Header = "Table in source", Binding = new Binding("Source"), IsReadOnly = true, Width = 220 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Table in target", Binding = new Binding("Target"), IsReadOnly = true, Width = 220 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Why", Binding = new Binding("Reason"), IsReadOnly = true, Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            var map = Ui.Button("Map and compare", () => { chosen = rows.Where(r => r.Map).Select(r => r.Data).ToList(); w.Close(); }, primary: true, minWidth: 140);
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            w.Content = Ui.WithFooter(Body("Table renames", "These tables exist on one side only, but look like the same table renamed. Mapped tables are compared with each other, and deployment renames the table instead of dropping and creating it.", Ui.Card(grid, 4)), Buttons(map, cancel));
            w.ShowDialog();
            return chosen;
        }

        // ---- Column mapping (data) ----

        public sealed class ColumnPair
        {
            public string Source { get; set; }
            public string Target { get; set; }
            public List<string> Choices { get; set; }
        }

        /// <summary>Pairs each source column with a target column; returns `old=new, …` for the pairs whose names differ, or null.</summary>
        public static string MapColumns(Window owner, string table, List<string> sourceColumns, List<string> targetColumns, string current)
        {
            var given = (current ?? "").Split(',').Select(p => p.Split('=')).Where(p => p.Length == 2).ToDictionary(p => p[0].Trim().Trim('[', ']'), p => p[1].Trim().Trim('[', ']'), StringComparer.OrdinalIgnoreCase);
            var choices = new List<string> { "" };
            choices.AddRange(targetColumns);
            var rows = sourceColumns.Select(c => new ColumnPair
            {
                Source = c,
                Target = given.TryGetValue(c, out var t) ? t : targetColumns.FirstOrDefault(x => string.Equals(x, c, StringComparison.OrdinalIgnoreCase)) ?? "",
                Choices = choices,
            }).ToList();
            string result = null;
            var w = Ui.Dialog(owner, "Column mapping", null, 620, 520);
            var grid = new DataGrid { ItemsSource = rows, AutoGenerateColumns = false, CanUserAddRows = false, HeadersVisibility = DataGridHeadersVisibility.Column };
            grid.Columns.Add(new DataGridTextColumn { Header = "Column in source", Binding = new Binding("Source"), IsReadOnly = true, Width = 240 });
            var target = new DataGridComboBoxColumn { Header = "Column in target", Width = new DataGridLength(1, DataGridLengthUnitType.Star), SelectedItemBinding = new Binding("Target") { UpdateSourceTrigger = UpdateSourceTrigger.PropertyChanged } };
            target.ElementStyle = new Style(typeof(ComboBox)) { Setters = { new Setter(ItemsControl.ItemsSourceProperty, new Binding("Choices")) } };
            target.EditingElementStyle = new Style(typeof(ComboBox)) { Setters = { new Setter(ItemsControl.ItemsSourceProperty, new Binding("Choices")) } };
            grid.Columns.Add(target);
            var ok = Ui.Button("OK", () =>
            {
                grid.CommitEdit(DataGridEditingUnit.Row, true);
                result = string.Join(", ", rows.Where(r => !string.IsNullOrEmpty(r.Target) && !string.Equals(r.Source, r.Target, StringComparison.OrdinalIgnoreCase)).Select(r => r.Source + "=" + r.Target));
                w.Close();
            }, primary: true, minWidth: 100);
            var cancel = Ui.Button("Cancel", () => w.Close(), minWidth: 90);
            cancel.IsCancel = true;
            w.Content = Ui.WithFooter(Body("Column mapping", "Columns pair by name. Pair a column of " + table + " with a differently named column of the target here.", Ui.Card(grid, 4)), Buttons(ok, cancel));
            w.ShowDialog();
            return result;
        }

        // ---- My projects ----

        public sealed class ProjectChoice
        {
            public string Path;
            public bool CompareNow;
            public bool? NewData;
        }

        private sealed class ProjectRow
        {
            public string Path { get; set; }
            public string File { get; set; }
            public string Source { get; set; } = "…";
            public string Target { get; set; } = "";
            public string Kind { get; set; }
            public DateTime Modified { get; set; }
        }

        public static string DefaultProjectsFolder => System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare", "Projects");

        public static ProjectChoice MyProjects(Engine engine, Window owner)
        {
            ProjectChoice choice = null;
            var settings = CompareSettings.Load();
            var folders = new List<string>();
            void addFolder(string f)
            {
                if (!string.IsNullOrEmpty(f) && !folders.Any(x => string.Equals(x, f, StringComparison.OrdinalIgnoreCase))) folders.Add(f);
            }
            addFolder(settings.ProjectsFolder);
            foreach (var r in settings.Recent) addFolder(System.IO.Path.GetDirectoryName(r));
            addFolder(DefaultProjectsFolder);
            var w = Ui.Dialog(owner, "My projects", null, 980, 600);
            var lookIn = new ComboBox { IsEditable = true, ItemsSource = folders, Text = folders.FirstOrDefault() ?? DefaultProjectsFolder, Width = 520 };
            var browse = Ui.IconButton(Glyph.Folder, "Choose a folder", () => { var f = PickFolder(w, lookIn.Text, "Look in"); if (f != null) lookIn.Text = f; }, subtle: false);
            browse.Margin = new Thickness(8, 0, 16, 0);
            var filter = Ui.SearchBox("Filter", 200);
            var lookLabel = Ui.Text("Look in", brush: CompareTheme.Muted);
            lookLabel.Margin = new Thickness(0, 0, 8, 0);
            var top = Ui.Row(lookLabel, lookIn, browse, filter);
            top.Margin = new Thickness(0, 0, 0, 10);
            var rows = new List<ProjectRow>();
            var view = new ListCollectionView(rows);
            var grid = new DataGrid { ItemsSource = view, AutoGenerateColumns = false, CanUserAddRows = false, IsReadOnly = true, HeadersVisibility = DataGridHeadersVisibility.Column, SelectionMode = DataGridSelectionMode.Single };
            grid.Columns.Add(new DataGridTextColumn { Header = "Data source", Binding = new Binding("Source"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            grid.Columns.Add(new DataGridTextColumn { Header = "", Binding = new Binding { Source = "→" }, Width = 32 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Data source", Binding = new Binding("Target"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            grid.Columns.Add(new DataGridTextColumn { Header = "Kind", Binding = new Binding("Kind"), Width = 80 });
            grid.Columns.Add(new DataGridTextColumn { Header = "Date modified", Binding = new Binding("Modified") { StringFormat = "g" }, Width = 140 });
            grid.Columns.Add(new DataGridTextColumn { Header = "File name", Binding = new Binding("File"), Width = 200 });
            view.Filter = o =>
            {
                var r = (ProjectRow)o;
                var f = filter.Text.Trim();
                return f.Length == 0 || (r.File + " " + r.Source + " " + r.Target).IndexOf(f, StringComparison.OrdinalIgnoreCase) >= 0;
            };
            filter.TextChanged += (s, e) => view.Refresh();
            var status = Ui.Text("", brush: CompareTheme.Muted);
            string side(JsonObject s) => s == null ? "" : s.Str("type") == "snapshot" || s.Str("type") == "scripts" || s.Str("type") == "backup" ? System.IO.Path.GetFileName((s.Str("path") ?? "").Split(';')[0]) : s.Str("type") == "empty" ? "Empty database" : s.Str("server") + " / " + s.Str("database");
            async Task load()
            {
                rows.Clear();
                var dir = lookIn.Text.Trim();
                if (Directory.Exists(dir))
                {
                    foreach (var f in new[] { "*.qsc", "*.qdc", "*.scp", "*.sdc" }.SelectMany(p => Directory.GetFiles(dir, p)).OrderByDescending(System.IO.File.GetLastWriteTime))
                    {
                        var ext = System.IO.Path.GetExtension(f).ToLowerInvariant();
                        rows.Add(new ProjectRow { Path = f, File = System.IO.Path.GetFileName(f), Modified = System.IO.File.GetLastWriteTime(f), Kind = ext == ".qdc" || ext == ".sdc" ? "Data" : "Schema" });
                    }
                }
                view.Refresh();
                status.Text = rows.Count == 0 ? "No projects in this folder." : Ui.Plural(rows.Count, "project", "projects");
                foreach (var r in rows.ToList())
                {
                    try
                    {
                        var p = await engine.RequestObject("project_load", new JsonObject { ["path"] = r.Path });
                        r.Source = side(p.Obj("source"));
                        r.Target = side(p.Obj("target"));
                    }
                    catch (EngineException ex)
                    {
                        r.Source = "Could not open: " + ex.Message;
                    }
                }
                view.Refresh();
            }
            lookIn.LostFocus += async (s, e) => await load();
            lookIn.SelectionChanged += async (s, e) => { if (lookIn.SelectedItem is string f) { lookIn.Text = f; await load(); } };
            ProjectRow current() => grid.SelectedItem as ProjectRow;
            void finish(bool compare)
            {
                if (current() == null) return;
                choice = new ProjectChoice { Path = current().Path, CompareNow = compare };
                w.Close();
            }
            grid.MouseDoubleClick += (s, e) => finish(false);
            var menu = new ContextMenu();
            menu.Items.Add(Ui.MenuItem("Compare now", () => finish(true), Glyph.Play));
            menu.Items.Add(Ui.MenuItem("Edit project", () => finish(false), Glyph.Edit));
            grid.ContextMenu = menu;
            RegisterMenu(menu);
            CompareTheme.Attach(menu);
            var newMenu = new ContextMenu();
            newMenu.Items.Add(Ui.MenuItem("Schema comparison", () => { choice = new ProjectChoice { NewData = false }; w.Close(); }, Glyph.List));
            newMenu.Items.Add(Ui.MenuItem("Data comparison", () => { choice = new ProjectChoice { NewData = true }; w.Close(); }, Glyph.Document));
            var newButton = Ui.MenuButton(Glyph.Add, "New…", newMenu);
            var startup = Ui.Check("Show this screen on startup", settings.ShowMyProjects, on => { var st = CompareSettings.Load(); st.ShowMyProjects = on; st.Save(); });
            var edit = Ui.Button("Edit…", () => finish(false), minWidth: 90, glyph: Glyph.Edit);
            var compareNow = Ui.Button("Compare now", () => finish(true), primary: true, minWidth: 120, glyph: Glyph.Play);
            var close = Ui.Button("Close", () => w.Close(), minWidth: 90);
            close.IsCancel = true;
            var footer = new DockPanel();
            var right = Buttons(newButton, edit, compareNow, close);
            DockPanel.SetDock(right, Dock.Right);
            footer.Children.Add(right);
            footer.Children.Add(startup);
            var content = new DockPanel();
            DockPanel.SetDock(top, Dock.Top);
            DockPanel.SetDock(status, Dock.Bottom);
            status.Margin = new Thickness(0, 8, 0, 0);
            content.Children.Add(top);
            content.Children.Add(status);
            content.Children.Add(Ui.Card(grid, 4));
            w.Content = Ui.WithFooter(Body("My projects", "Projects saved in a folder. Open one to edit it, or compare it straight away.", content), footer);
            w.Loaded += async (s, e) => await load();
            w.ShowDialog();
            if (Directory.Exists(lookIn.Text.Trim()))
            {
                var st = CompareSettings.Load();
                st.ProjectsFolder = lookIn.Text.Trim();
                st.Save();
            }
            return choice;
        }
    }
}
