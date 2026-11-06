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

    internal sealed class MainWindow : Window, ICompareHost
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

        /// <summary>Without SSMS, a script opens in the program registered for .sql files, or Notepad.</summary>
        public void OpenScript(string sql, string title)
        {
            var path = Path.Combine(Path.GetTempPath(), "QuillSQL", (title ?? "script").Replace(' ', '_') + "_" + DateTime.Now.ToString("yyyyMMdd_HHmmss") + ".sql");
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            File.WriteAllText(path, sql, new System.Text.UTF8Encoding(true));
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(path) { UseShellExecute = true });
            }
            catch (System.ComponentModel.Win32Exception)
            {
                System.Diagnostics.Process.Start("notepad.exe", "\"" + path + "\"");
            }
        }
    }
}
