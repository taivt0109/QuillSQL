using System;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>Tools > QuillSQL > About QuillSQL: the version installed, where it lives, and links.</summary>
    internal static class AboutWindow
    {
        public static void Show(AsyncPackage package)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var window = Theme.Dialog(Loc.T("About QuillSQL"), false);
            var body = new StackPanel { Margin = new Thickness(22, 20, 24, 18), MinWidth = 420 };
            body.Children.Add(new TextBlock { Text = "QuillSQL " + QuillSqlPackage.Version, FontSize = 18, FontWeight = FontWeights.SemiBold });
            body.Children.Add(new TextBlock { Text = Loc.T("T-SQL formatting, suggestions, refactoring and analysis for SSMS."), Opacity = 0.75, Margin = new Thickness(0, 4, 0, 14), TextWrapping = TextWrapping.Wrap });

            var ssms = Process.GetCurrentProcess().MainModule.FileVersionInfo;
            var extension = Path.GetDirectoryName(typeof(AboutWindow).Assembly.Location);
            // The instance folder (21.0_cc31859b, 22.0_db90ce57Exp) is two levels above the extension.
            var instance = new DirectoryInfo(extension).Parent?.Parent?.Name ?? "";
            row(body, Loc.T("SSMS"), ssms.ProductVersion + (instance.Length > 0 ? "  (" + instance + ")" : ""));
            row(body, Loc.T("Installed in"), extension);
            link(body, Loc.T("Settings and log"), Updater.Folder);
            link(body, "GitHub", "https://github.com/" + Updater.Repository);
            link(body, Loc.T("Release notes"), "https://github.com/" + Updater.Repository + "/releases");

            var check = Theme.DialogButton(Loc.T("Check for Updates"));
            check.Click += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                window.Close();
                Updater.Start(package, true);
            };
            var close = Theme.DialogButton(Loc.T("Close"), isDefault: true, isCancel: true);
            close.Click += (s, e) => window.Close();
            var root = new DockPanel();
            var footer = Theme.Footer(null, check, close);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(body);
            window.Content = root;
            window.SizeToContent = SizeToContent.WidthAndHeight;
            EditorContext.SetOwner(window);
            window.ShowDialog();
        }

        private static void row(Panel body, string label, string value)
        {
            var line = new DockPanel { Margin = new Thickness(0, 3, 0, 3) };
            var name = new TextBlock { Text = label, Width = 130, Opacity = 0.7 };
            DockPanel.SetDock(name, Dock.Left);
            line.Children.Add(name);
            line.Children.Add(new TextBox { Text = value, IsReadOnly = true, BorderThickness = new Thickness(0), Background = System.Windows.Media.Brushes.Transparent, Foreground = (System.Windows.Media.Brush)body.GetValue(TextElement.ForegroundProperty), TextWrapping = TextWrapping.Wrap, MaxWidth = 460 });
            body.Children.Add(line);
        }

        private static void link(Panel body, string label, string target)
        {
            var line = new DockPanel { Margin = new Thickness(0, 3, 0, 3) };
            var name = new TextBlock { Text = label, Width = 130, Opacity = 0.7 };
            DockPanel.SetDock(name, Dock.Left);
            line.Children.Add(name);
            var hyperlink = new Hyperlink(new Run(target));
            hyperlink.Click += (s, e) =>
            {
                try
                {
                    Process.Start(new ProcessStartInfo(target) { UseShellExecute = true });
                }
                catch (Exception ex)
                {
                    Log.Error("opening " + target, ex);
                }
            };
            line.Children.Add(new TextBlock(hyperlink) { TextWrapping = TextWrapping.Wrap, MaxWidth = 460 });
            body.Children.Add(line);
        }
    }
}
