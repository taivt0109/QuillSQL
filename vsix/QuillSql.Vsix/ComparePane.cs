using System;
using System.Collections.Specialized;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using QuillSql.Compare;

namespace QuillSql.Vsix
{
    /// <summary>A Schema or Data Compare window, opened as a document tab. Several can be open.</summary>
    [Guid("2d8a5f1e-3c47-4b6a-9e12-7f0c4d8b2a61")]
    public sealed class ComparePane : ToolWindowPane, ICompareHost
    {
        private static Engine _engine;
        private static int _next;

        public ComparePane() : base(null)
        {
            Caption = "QuillSQL Compare";
            ApplyTheme();
            _engine = _engine ?? new Engine(Path.Combine(Path.GetDirectoryName(typeof(ComparePane).Assembly.Location) ?? ".", "quill.exe"));
            View = new CompareView(_engine, this);
            View.TitleChanged += t => Caption = t.Replace(" - QuillSQL ", " - ");
            Content = View;
        }

        internal CompareView View { get; }

        Window ICompareHost.Owner => Application.Current?.MainWindow;

        public void OpenScript(string sql, string title)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            EditorContext.OpenInNewQuery(sql);
        }

        /// <summary>The Compare windows follow the SSMS color theme.</summary>
        private static void ApplyTheme()
        {
            try
            {
                System.Windows.Media.Color color(object key) => ((System.Windows.Media.SolidColorBrush)Application.Current.FindResource(key)).Color;
                var background = color(EnvironmentColors.ToolWindowBackgroundBrushKey);
                var text = color(EnvironmentColors.ToolWindowTextBrushKey);
                var accent = color(EnvironmentColors.SystemHighlightBrushKey);
                var dark = (0.2126 * background.R + 0.7152 * background.G + 0.0722 * background.B) / 255 < 0.5;
                CompareTheme.Use(dark, accent, background, text);
            }
            catch (ResourceReferenceKeyNotFoundException)
            {
            }
        }

        /// <summary>Opens a new Compare tab; `source` pre-fills database 1 (from Object Explorer).</summary>
        internal static void Open(AsyncPackage package, bool data, JsonObject source = null, string password = null)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                try
                {
                    var pane = await package.FindToolWindowAsync(typeof(ComparePane), ++_next, true, package.DisposalToken) as ComparePane;
                    await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                    if (pane?.Frame is IVsWindowFrame frame)
                    {
                        Microsoft.VisualStudio.ErrorHandler.ThrowOnFailure(frame.Show());
                    }
                    if (pane == null) return;
                    if (source != null) pane.View.New(data, source, password);
                    else if (data) pane.View.New(true);
                    else await pane.View.Start();
                }
                catch (Exception ex)
                {
                    Log.Error("opening a Compare window", ex);
                }
            });
        }

        /// <summary>A project side from an SSMS connection (the Object Explorer node's).</summary>
        internal static JsonObject SourceFrom(object connectionInfo, string database, out string password)
        {
            password = null;
            var server = ConnectionResolver.Get(connectionInfo, "ServerName") as string;
            if (string.IsNullOrEmpty(server)) return null;
            var auth = Convert.ToInt32(ConnectionResolver.Get(connectionInfo, "AuthenticationType") ?? 0);
            var options = ConnectionResolver.Get(connectionInfo, "AdvancedOptions") as NameValueCollection;
            var side = new JsonObject
            {
                ["type"] = "database",
                ["server"] = server,
                ["database"] = database ?? "",
                ["authentication"] = auth == 1 ? "sql" : auth == 0 ? "windows" : "entra",
                ["user"] = auth == 1 ? ConnectionResolver.Get(connectionInfo, "UserName") as string ?? "" : "",
                ["encrypt"] = !string.Equals(options?["ENCRYPT_CONNECTION"], "False", StringComparison.OrdinalIgnoreCase),
                ["trustServerCertificate"] = string.Equals(options?["TRUST_SERVER_CERTIFICATE"], "True", StringComparison.OrdinalIgnoreCase),
            };
            if (auth == 1) password = ConnectionResolver.Get(connectionInfo, "Password") as string;
            return side;
        }
    }
}
