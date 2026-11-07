using System;
using System.Collections.Specialized;
using System.IO;
using System.Linq;
using System.Reflection;
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
    public sealed class ComparePane : ToolWindowPane, ICompareHost, IConnectedScriptHost
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

        /// <summary>
        /// The script goes to a new query window connected to the target's server and database, so
        /// it cannot run on the source by mistake. When SSMS cannot be asked for one, the window
        /// opens on the current connection and the status bar says so.
        /// </summary>
        public void OpenScript(string sql, string title, JsonObject target)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (target != null && OpenConnected(sql, target)) return;
            EditorContext.OpenInNewQuery(sql);
            if (target != null) EditorContext.Status("the script's query window could not be connected to " + target.Str("server") + "; check the connection before running it");
        }

        private const BindingFlags Static = BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static;
        private const BindingFlags Instance = BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance;

        private static Type FindType(string fullName) =>
            AppDomain.CurrentDomain.GetAssemblies().Select(a => { try { return a.GetType(fullName, false); } catch (Exception) { return null; } }).FirstOrDefault(t => t != null);

        /// <summary>SSMS's sign-in kinds (UIConnectionInfo.AuthenticationType) for our authentication values.</summary>
        private static int SsmsAuthentication(string auth)
        {
            switch (auth)
            {
                case "sql": return 1;
                case "entraInteractive": case "entra": return 2;
                case "entraPassword": return 3;
                case "entraIntegrated": return 4;
                default: return 0;
            }
        }

        /// <summary>Opens a blank query through SSMS's script factory with a connection to the target, then fills it.</summary>
        private static bool OpenConnected(string sql, JsonObject target)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var factory = FindType("Microsoft.SqlServer.Management.UI.VSIntegration.ServiceCache")?.GetProperty("ScriptFactory", Static)?.GetValue(null);
                var infoType = FindType("Microsoft.SqlServer.Management.Smo.RegSvrEnum.UIConnectionInfo");
                var scriptType = FindType("Microsoft.SqlServer.Management.UI.VSIntegration.Editors.ScriptType");
                if (factory == null || infoType == null || scriptType == null) return false;
                var info = Activator.CreateInstance(infoType);
                void set(string name, object value) => infoType.GetProperty(name, Instance)?.SetValue(info, value);
                set("ServerName", target.Str("server"));
                // The database engine's server type.
                set("ServerType", new Guid("8c91a03d-f9b4-46c0-a305-b5dcc79ff907"));
                set("AuthenticationType", SsmsAuthentication(target.Str("authentication")));
                if (!string.IsNullOrEmpty(target.Str("user"))) set("UserName", target.Str("user"));
                if (!string.IsNullOrEmpty(target.Str("password"))) set("Password", target.Str("password"));
                if (infoType.GetProperty("AdvancedOptions", Instance)?.GetValue(info) is NameValueCollection options)
                {
                    options["DATABASE"] = target.Str("database");
                    options["ENCRYPT_CONNECTION"] = target.Bool("encrypt") ? "True" : "False";
                    options["TRUST_SERVER_CERTIFICATE"] = target.Bool("trustServerCertificate") ? "True" : "False";
                }
                var create = factory.GetType().GetMethods(Instance).FirstOrDefault(m => m.Name == "CreateNewBlankScript" && m.GetParameters().Length == 3);
                if (create == null) return false;
                create.Invoke(factory, new[] { Enum.Parse(scriptType, "Sql"), info, null });
                return EditorContext.InsertInActive(sql);
            }
            catch (Exception ex) when (ex is TargetInvocationException || ex is ArgumentException || ex is InvalidOperationException || ex is MissingMethodException || ex is COMException)
            {
                Log.Error("opening a connected query window", ex);
                return false;
            }
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
            // SSMS's sign-in kinds: 0 Windows, 1 SQL Server, 2 Microsoft Entra with MFA, 3 Entra
            // password, 4 Entra integrated; newer kinds sign in as with MFA.
            var authentication = auth == 0 ? "windows" : auth == 1 ? "sql" : auth == 3 ? "entraPassword" : auth == 4 ? "entraIntegrated" : "entraInteractive";
            var side = new JsonObject
            {
                ["type"] = "database",
                ["server"] = server,
                ["database"] = database ?? "",
                ["authentication"] = authentication,
                ["user"] = auth == 0 || auth == 4 ? "" : ConnectionResolver.Get(connectionInfo, "UserName") as string ?? "",
                ["encrypt"] = !string.Equals(options?["ENCRYPT_CONNECTION"], "False", StringComparison.OrdinalIgnoreCase),
                ["trustServerCertificate"] = string.Equals(options?["TRUST_SERVER_CERTIFICATE"], "True", StringComparison.OrdinalIgnoreCase),
            };
            if (auth == 1 || auth == 3) password = ConnectionResolver.Get(connectionInfo, "Password") as string;
            return side;
        }
    }
}
