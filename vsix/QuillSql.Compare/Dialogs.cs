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
                var ok = await Ui.Try(w, async () => (object)await session.Engine.Request("data_export", new JsonObject
                {
                    ["session"] = results.Session,
                    ["folder"] = dir,
                    ["kinds"] = new List<object> { identical.IsChecked == true, different.IsChecked == true, only1.IsChecked == true, only2.IsChecked == true },
                    ["tables"] = rows.Where(r => r.Export).Select(r => (object)r.Index).ToList(),
                }));
                generate.IsEnabled = true;
                if (ok == null) return;
                w.Close();
                OpenFolder(dir);
            };
            w.Content = Ui.WithFooter(Body("Export CSV files", "Choose the tables and views you want to export. Separate .csv files will be created for each table or view.", content), Buttons(generate, cancel));
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
            string side(JsonObject s) => s == null ? "" : s.Str("type") == "snapshot" || s.Str("type") == "scripts" ? System.IO.Path.GetFileName(s.Str("path")) : s.Str("type") == "empty" ? "Empty database" : s.Str("server") + " / " + s.Str("database");
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
