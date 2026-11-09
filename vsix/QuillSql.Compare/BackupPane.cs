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
    /// <summary>
    /// A backup as a data source, as the classic compare tool offers it: the backup files (several for a striped
    /// backup), their passwords, and the backup set to read, picked from what the files hold
    /// (`backup_sets`).
    /// </summary>
    internal sealed class BackupPane : StackPanel
    {
        public sealed class SetRow
        {
            public string Name { get; set; }
            public string Database { get; set; }
            public string Server { get; set; }
            public string Date { get; set; }
            public string Type { get; set; }
            public string Status { get; set; }
            public bool CanHandle { get; set; }
        }

        private readonly CompareSession _session;
        private readonly ListBox _files = new ListBox { Height = 90, SelectionMode = SelectionMode.Extended };
        private readonly PasswordBox _password = new PasswordBox();
        private readonly DataGrid _sets = new DataGrid { AutoGenerateColumns = false, CanUserAddRows = false, IsReadOnly = true, HeadersVisibility = DataGridHeadersVisibility.Column, SelectionMode = DataGridSelectionMode.Single, Height = 150 };
        private readonly TextBlock _status = Ui.Text("", brush: CompareTheme.Muted, wrap: TextWrapping.Wrap);
        private string _wanted;

        public BackupPane(CompareSession session)
        {
            _session = session;
            _sets.Columns.Add(new DataGridTextColumn { Header = "Database", Binding = new Binding("Database"), Width = new DataGridLength(1, DataGridLengthUnitType.Star) });
            _sets.Columns.Add(new DataGridTextColumn { Header = "Server", Binding = new Binding("Server"), Width = 110 });
            _sets.Columns.Add(new DataGridTextColumn { Header = "Date", Binding = new Binding("Date"), Width = 130 });
            _sets.Columns.Add(new DataGridTextColumn { Header = "Type", Binding = new Binding("Type"), Width = 50 });
            var rowStyle = new Style(typeof(DataGridRow), CompareTheme.Style(typeof(DataGridRow)));
            rowStyle.Setters.Add(new Setter(ToolTipProperty, new Binding("Status")));
            _sets.RowStyle = rowStyle;
            _sets.SelectionChanged += (s, e) =>
            {
                if (_sets.SelectedItem is SetRow r) _status.Text = r.Status.Length > 0 ? r.Status : (string.IsNullOrEmpty(r.Name) ? "The backup" : "Backup set " + r.Name) + " of " + r.Database + ", " + r.Date + ".";
            };
            var add = Ui.Button("Add backup set files…", async () =>
            {
                var d = new OpenFileDialog { Filter = "Backup file (*.bak, *.sqb)|*.bak;*.sqb|All Files (*.*)|*.*", Multiselect = true };
                if (Dialogs.Show(d, _session.Host.Owner) != true) return;
                foreach (var f in d.FileNames.SelectMany(n => n.Split(';')).Where(f => f.Length > 0))
                {
                    if (!_files.Items.Cast<string>().Contains(f, StringComparer.OrdinalIgnoreCase)) _files.Items.Add(f);
                }
                await Read();
            }, glyph: Glyph.Add);
            add.Margin = new Thickness(0);
            var remove = Ui.Button("Remove", async () =>
            {
                foreach (var f in _files.SelectedItems.Cast<string>().ToList()) _files.Items.Remove(f);
                await Read();
            }, glyph: Glyph.Delete);
            var clear = Ui.Button("Clear list", async () => { _files.Items.Clear(); await Read(); });
            var readButton = Ui.Button("Read backup sets", async () => await Read(), glyph: Glyph.Refresh);
            _password.LostFocus += async (s, e) => { if (_files.Items.Count > 0) await Read(); };
            Children.Add(Ui.Form(
                Ui.Field("Backup files (several for a striped backup)", _files),
                Ui.Field(null, Ui.Row(add, remove, clear)),
                Ui.Field("Passwords of encrypted backups (one per file, separated by commas)", _password),
                Ui.Field("Backup sets", _sets),
                Ui.Field(null, readButton)));
            Children.Add(_status);
        }

        public string Path => string.Join(";", _files.Items.Cast<string>());
        public string Passwords => _password.Password;
        public string BackupSet => (_sets.SelectedItem as SetRow)?.Name ?? _wanted ?? "";

        public void Load(JsonObject side, string passwords)
        {
            _files.Items.Clear();
            foreach (var f in (side.Str("type") == "backup" ? side.Str("path") ?? "" : "").Split(';').Where(f => f.Trim().Length > 0)) _files.Items.Add(f.Trim());
            _password.Password = passwords ?? "";
            _wanted = side.Str("backupSet");
            _sets.ItemsSource = null;
            _status.Text = _files.Items.Count == 0 ? "No files selected" : "";
        }

        /// <summary>Reads the sets in the files; .sqb backup files cannot be read yet.</summary>
        public async Task Read()
        {
            if (_files.Items.Count == 0)
            {
                _sets.ItemsSource = null;
                _status.Text = "No files selected";
                return;
            }
            _status.Text = "Retrieving backup sets...";
            var reply = await Ui.Try(_session.Host.Owner, async () => await _session.Engine.Request("backup_sets", new JsonObject
            {
                ["source"] = new JsonObject { ["type"] = "backup", ["path"] = Path, ["password"] = _password.Password },
            }) as List<object>);
            if (reply == null)
            {
                _status.Text = "The backup files could not be read.";
                return;
            }
            var rows = reply.OfType<JsonObject>().Select(s => new SetRow
            {
                Name = s.Str("name"),
                Database = s.Str("database"),
                Server = s.Str("server"),
                Date = DateTime.TryParse(s.Str("finish") ?? s.Str("start"), out var d) ? d.ToString("g") : s.Str("finish") ?? "",
                Type = (s.Str("type") ?? "").StartsWith("Diff", StringComparison.OrdinalIgnoreCase) ? "Diff" : "Full",
                CanHandle = s.Bool("canHandle"),
                Status = s.Bool("incomplete") ? "Backup set may be incomplete. To compare a differential backup you must add the corresponding full backup."
                    : s.Bool("encrypted") && !s.Bool("canHandle") ? "The backup set is encrypted: enter its password."
                    : !s.Bool("canHandle") ? "This backup set cannot be read."
                    : "",
            }).ToList();
            _sets.ItemsSource = rows;
            var pick = rows.FirstOrDefault(r => r.Name == _wanted) ?? rows.LastOrDefault(r => r.CanHandle) ?? rows.FirstOrDefault();
            _sets.SelectedItem = pick;
            if (pick != null) _wanted = pick.Name;
            if (rows.Count == 0) _status.Text = "The files hold no backup sets.";
        }
    }
}
