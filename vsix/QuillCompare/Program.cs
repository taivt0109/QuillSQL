using System;
using System.IO;
using System.Linq;
using System.Windows;
using QuillSql.Compare;

namespace QuillCompare
{
    /// <summary>QuillCompare.exe [project.qsc | project.qdc | project.scp | project.sdc]</summary>
    public static class Program
    {
        [STAThread]
        public static int Main(string[] args)
        {
            var app = new Application { ShutdownMode = ShutdownMode.OnMainWindowClose };
            CompareTheme.UseWindows(translucent: true);
            app.Resources.MergedDictionaries.Add(CompareTheme.Resources);
            var engine = new Engine(Engine.FindQuill());
            var window = new MainWindow(engine, args.FirstOrDefault(a => File.Exists(a)));
            app.Exit += (s, e) => engine.Dispose();
            return app.Run(window);
        }
    }

    internal sealed class MainWindow : Window, ICompareHost, IConnectedScriptHost
    {
        private readonly CompareView _view;

        public MainWindow(Engine engine, string project)
        {
            Title = "QuillSQL Compare";
            Width = 1280;
            Height = 820;
            MinWidth = 900;
            MinHeight = 600;
            WindowStartupLocation = WindowStartupLocation.CenterScreen;
            Backdrop.Apply(this);
            Icon = new System.Windows.Media.Imaging.BitmapImage(new Uri("pack://application:,,,/quillsql-32.png"));
            _view = new CompareView(engine, this);
            _view.TitleChanged += t => Title = t;
            Content = _view;
            Loaded += async (s, e) =>
            {
                if (project != null) await _view.Open(project);
                else await _view.Start();
            };
            Closing += (s, e) => e.Cancel = !_view.CanClose();
        }

        Window ICompareHost.Owner => this;

        public void OpenScript(string sql, string title) => OpenScript(sql, title, null);

        /// <summary>
        /// Without SSMS, a script opens in the editor chosen in Application options: SSMS connected
        /// to the target (-S server -d database), a custom program (%SF% file, %SN% server,
        /// %DN% database, %UN% user), or the program registered for .sql files (else Notepad).
        /// </summary>
        public void OpenScript(string sql, string title, JsonObject target)
        {
            var settings = CompareSettings.Load();
            var path = Path.Combine(Path.GetTempPath(), "QuillSQL", (title ?? "script").Replace(' ', '_') + "_" + DateTime.Now.ToString("yyyyMMdd_HHmmss") + ".sql");
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            File.WriteAllText(path, sql, CompareSettings.EncodingOf(settings.ScriptEncoding));
            string q(string v) => "\"" + (v ?? "").Replace("\"", "\\\"") + "\"";
            try
            {
                if (settings.SqlEditor == "custom" && File.Exists(settings.CustomEditor))
                {
                    var args = (settings.CustomEditorArguments ?? "\"%SF%\"")
                        .Replace("%SF%", path)
                        .Replace("%SN%", target?.Str("server") ?? "")
                        .Replace("%DN%", target?.Str("database") ?? "")
                        .Replace("%UN%", target?.Str("user") ?? "");
                    System.Diagnostics.Process.Start(settings.CustomEditor, args);
                    return;
                }
                if (settings.SqlEditor.Length > 0 && File.Exists(settings.SqlEditor))
                {
                    var args = q(path);
                    if (target != null)
                    {
                        args = "-S " + q(target.Str("server")) + " -d " + q(target.Str("database")) + " " + args;
                        var auth = target.Str("authentication");
                        if (auth == "sql") args = "-U " + q(target.Str("user")) + " " + args;
                        else if (auth == "windows") args = "-E " + args;
                        else if (!string.IsNullOrEmpty(target.Str("user"))) args = "-U " + q(target.Str("user")) + " " + args;
                    }
                    System.Diagnostics.Process.Start(settings.SqlEditor, args);
                    return;
                }
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(path) { UseShellExecute = true });
            }
            catch (System.ComponentModel.Win32Exception)
            {
                System.Diagnostics.Process.Start("notepad.exe", "\"" + path + "\"");
            }
        }
    }
}
