using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.Serialization;
using System.Text;
using System.Windows.Threading;
using Microsoft.Data.SqlClient;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Reopens the query windows of an SSMS that closed without being shut down (a crash, a killed
    /// process, a power cut). Each SSMS writes its open query windows to session-PID.json every
    /// 20 seconds when they change: the file, the text not saved yet, and the connection (server,
    /// database, sign-in kind and user; never a password). A normal shutdown deletes the file, so
    /// a file left by an SSMS that is no longer running means its windows were lost.
    /// </summary>
    internal static class SessionKeeper
    {
        private const long MaxText = 5 * 1024 * 1024;
        private static readonly DispatcherTimer Timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(20) };
        private static string _last;
        /// <summary>Held so the shutdown event stays connected.</summary>
        private static EnvDTE.DTEEvents _shellEvents;

        /// <summary>The end-to-end tests keep their windows apart from the user's.</summary>
        private static bool Testing => !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("QUILLSQL_TEST_PIPE"));

        private static string Folder => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", Testing ? "Sessions-test" : "Sessions");

        private static string Own => Path.Combine(Folder, "session-" + Process.GetCurrentProcess().Id + ".json");

        [DataContract]
        internal sealed class Tab
        {
            [DataMember(Name = "caption")] public string Caption { get; set; }
            /// <summary>The saved file, if the window has one.</summary>
            [DataMember(Name = "file")] public string File { get; set; }
            /// <summary>The text, when it is not the file's (a new query, or changes not saved).</summary>
            [DataMember(Name = "text")] public string Text { get; set; }
            [DataMember(Name = "server")] public string Server { get; set; }
            [DataMember(Name = "database")] public string Database { get; set; }
            [DataMember(Name = "authentication")] public int Authentication { get; set; }
            [DataMember(Name = "user")] public string User { get; set; }
            [DataMember(Name = "trust")] public bool TrustServerCertificate { get; set; }
        }

        public static void Start()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!Settings.Current.RestoreTabs)
            {
                return;
            }
            Directory.CreateDirectory(Folder);
            _shellEvents = EditorContext.Dte?.Events.DTEEvents;
            if (_shellEvents != null)
            {
                // The shutdown can still be cancelled (a file not saved): the next save writes it again.
                _shellEvents.OnBeginShutdown += Forget;
            }
            Timer.Tick += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                try
                {
                    Save();
                }
                catch (Exception ex)
                {
                    Timer.Stop();
                    Log.Error("keeping the open query windows (stopped)", ex);
                }
            };
            Timer.Start();
            if (Testing)
            {
                // The test reopens through the hook; a question would wait for a click.
                return;
            }
            // Ask once the shell has settled, not while SSMS is still starting.
            var ask = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
            ask.Tick += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                ask.Stop();
                try
                {
                    OfferLost();
                }
                catch (Exception ex)
                {
                    Log.Error("reopening query windows", ex);
                }
            };
            ask.Start();
        }

        /// <summary>The package closes: SSMS shut down normally, nothing was lost.</summary>
        public static void Stop()
        {
            Timer.Stop();
            Forget();
        }

        private static void Forget()
        {
            _last = null;
            try
            {
                File.Delete(Own);
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }

        public static void Save()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var tabs = OpenTabs();
            var json = Serialize(tabs);
            if (json == _last)
            {
                return;
            }
            _last = json;
            if (tabs.Count == 0)
            {
                File.Delete(Own);
                return;
            }
            var temp = Own + ".tmp";
            File.WriteAllText(temp, json, new UTF8Encoding(false));
            File.Copy(temp, Own, true);
            File.Delete(temp);
        }

        private static List<Tab> OpenTabs()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var tabs = new List<Tab>();
            if (!(Package.GetGlobalService(typeof(SVsUIShell)) is IVsUIShell shell) || shell.GetDocumentWindowEnum(out var frames) != VSConstants.S_OK)
            {
                return tabs;
            }
            var one = new IVsWindowFrame[1];
            while (frames.Next(1, one, out var fetched) == VSConstants.S_OK && fetched == 1)
            {
                var frame = one[0];
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView);
                if (!(docView is IVsCodeWindow code) || code.GetBuffer(out var lines) != VSConstants.S_OK || !EditorContext.IsSql(lines))
                {
                    continue;
                }
                lines.GetLastLineIndex(out var lastLine, out var lastColumn);
                if (lines.GetLineText(0, 0, lastLine, lastColumn, out var text) != VSConstants.S_OK || text.Length > MaxText)
                {
                    continue;
                }
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_Caption, out var caption);
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_pszMkDocument, out var moniker);
                var file = moniker as string;
                var saved = !string.IsNullOrEmpty(file) && Path.IsPathRooted(file) && File.Exists(file);
                var tab = new Tab
                {
                    Caption = caption as string,
                    File = saved ? file : null,
                    // A saved file that has not changed reopens from disk.
                    Text = saved && SameAsFile(file, text) ? null : text,
                };
                var target = ConnectionResolver.ForDocView(docView);
                if (target?.CanQuery == true)
                {
                    var parts = (target.Key ?? "").Split('|');
                    var builder = new SqlConnectionStringBuilder(target.ConnectionString);
                    tab.Server = builder.DataSource;
                    tab.Database = builder.InitialCatalog;
                    tab.Authentication = int.TryParse(parts.ElementAtOrDefault(2), out var kind) ? kind : 0;
                    tab.User = parts.ElementAtOrDefault(3);
                    tab.TrustServerCertificate = builder.TrustServerCertificate;
                }
                // An empty new query is not worth reopening.
                if (tab.File != null || !string.IsNullOrWhiteSpace(tab.Text))
                {
                    tabs.Add(tab);
                }
            }
            return tabs;
        }

        private static bool SameAsFile(string file, string text)
        {
            try
            {
                return new FileInfo(file).Length < MaxText && File.ReadAllText(file) == text;
            }
            catch (IOException)
            {
                return false;
            }
        }

        /// <summary>Offers the windows of SSMS sessions that ended without shutting down.</summary>
        private static void OfferLost() => ReopenLost(true);

        /// <summary>Reopens the windows of SSMS sessions that ended without shutting down, after asking if `ask`; how many.</summary>
        public static int ReopenLost(bool ask)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var running = new HashSet<int>(Process.GetProcessesByName("SSMS").Select(p => p.Id));
            var lost = new List<string>();
            foreach (var path in Directory.GetFiles(Folder, "session-*.json"))
            {
                var name = Path.GetFileNameWithoutExtension(path).Substring("session-".Length);
                if (int.TryParse(name, out var pid) && !running.Contains(pid))
                {
                    lost.Add(path);
                }
            }
            if (lost.Count == 0)
            {
                return 0;
            }
            var tabs = new List<Tab>();
            foreach (var path in lost)
            {
                try
                {
                    tabs.AddRange(Json.Parse<Tab[]>(File.ReadAllText(path)) ?? new Tab[0]);
                }
                catch (Exception ex) when (ex is IOException || ex is SerializationException)
                {
                    Log.Write("could not read " + path + ": " + ex.Message);
                }
            }
            var restored = Path.Combine(Folder, "restored");
            Directory.CreateDirectory(restored);
            // Keep the files beside, so a window that does not reopen can still be found.
            foreach (var path in lost)
            {
                var keep = Path.Combine(restored, Path.GetFileName(path));
                File.Copy(path, keep, true);
                File.Delete(path);
            }
            if (tabs.Count == 0)
            {
                return 0;
            }
            Log.Write("found " + tabs.Count + " query window(s) of an SSMS that did not shut down");
            var names = string.Join(Environment.NewLine, tabs.Take(8).Select(t => "  " + (t.Caption ?? t.File ?? "?"))) + (tabs.Count > 8 ? Environment.NewLine + "  ..." : "");
            if (ask && !EditorContext.Confirm("Reopen query windows",
                Loc.T("SSMS closed without shutting down. Reopen these query windows?") + Environment.NewLine + Environment.NewLine + names,
                "Reopen"))
            {
                return 0;
            }
            foreach (var tab in tabs)
            {
                Reopen(tab);
            }
            return tabs.Count;
        }

        private static void Reopen(Tab tab)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (tab.File != null && tab.Text == null && File.Exists(tab.File))
                {
                    EditorContext.Dte?.ItemOperations.OpenFile(tab.File);
                    return;
                }
                if (!string.IsNullOrEmpty(tab.Server) && EditorContext.OpenConnected(tab.Text ?? "", tab.Server, tab.Database, tab.Authentication, tab.User, tab.TrustServerCertificate))
                {
                    return;
                }
                EditorContext.OpenInNewQuery(tab.Text ?? "");
            }
            catch (Exception ex)
            {
                Log.Error("reopening " + (tab.Caption ?? tab.File), ex);
            }
        }

        private static string Serialize(List<Tab> tabs)
        {
            var sb = new StringBuilder("[");
            for (var i = 0; i < tabs.Count; i++)
            {
                var t = tabs[i];
                sb.Append(i == 0 ? "" : ",").Append("{\"caption\":").Append(Json.Str(t.Caption))
                    .Append(",\"file\":").Append(Json.Str(t.File))
                    .Append(",\"text\":").Append(Json.Str(t.Text))
                    .Append(",\"server\":").Append(Json.Str(t.Server))
                    .Append(",\"database\":").Append(Json.Str(t.Database))
                    .Append(",\"authentication\":").Append(t.Authentication)
                    .Append(",\"user\":").Append(Json.Str(t.User))
                    .Append(",\"trust\":").Append(Json.Bool(t.TrustServerCertificate)).Append('}');
            }
            return sb.Append(']').ToString();
        }
    }
}
