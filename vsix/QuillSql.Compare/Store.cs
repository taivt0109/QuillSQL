using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace QuillSql.Compare
{
    /// <summary>
    /// Passwords for "Remember credentials", in Windows Credential Manager under
    /// `QuillSQL:server|user`. Never written to project files.
    /// </summary>
    public static class Credentials
    {
        private const int Generic = 1;
        private const int PersistLocalMachine = 2;

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct Credential
        {
            public int Flags;
            public int Type;
            public string TargetName;
            public string Comment;
            public long LastWritten;
            public int CredentialBlobSize;
            public IntPtr CredentialBlob;
            public int Persist;
            public int AttributeCount;
            public IntPtr Attributes;
            public string TargetAlias;
            public string UserName;
        }

        [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredRead(string target, int type, int flags, out IntPtr credential);

        [DllImport("advapi32.dll", EntryPoint = "CredWriteW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredWrite(ref Credential credential, int flags);

        [DllImport("advapi32.dll", EntryPoint = "CredDeleteW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredDelete(string target, int type, int flags);

        [DllImport("advapi32.dll")]
        private static extern void CredFree(IntPtr buffer);

        private static string Target(string server, string user) => "QuillSQL:" + (server ?? "").Trim().ToLowerInvariant() + "|" + (user ?? "").Trim();

        public static string Read(string server, string user)
        {
            if (!CredRead(Target(server, user), Generic, 0, out var ptr)) return null;
            try
            {
                var c = (Credential)Marshal.PtrToStructure(ptr, typeof(Credential));
                return c.CredentialBlobSize == 0 ? "" : Marshal.PtrToStringUni(c.CredentialBlob, c.CredentialBlobSize / 2);
            }
            finally
            {
                CredFree(ptr);
            }
        }

        public static void Write(string server, string user, string password)
        {
            var bytes = Encoding.Unicode.GetBytes(password ?? "");
            var blob = Marshal.AllocHGlobal(bytes.Length);
            try
            {
                Marshal.Copy(bytes, 0, blob, bytes.Length);
                var c = new Credential
                {
                    Type = Generic,
                    TargetName = Target(server, user),
                    UserName = user,
                    CredentialBlob = blob,
                    CredentialBlobSize = bytes.Length,
                    Persist = PersistLocalMachine,
                    Comment = "QuillSQL Compare",
                };
                if (!CredWrite(ref c, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            finally
            {
                Marshal.FreeHGlobal(blob);
            }
        }

        public static void Delete(string server, string user) => CredDelete(Target(server, user), Generic, 0);
    }

    /// <summary>%LOCALAPPDATA%\QuillSQL\compare.json: recent projects and "my defaults".</summary>
    public sealed class CompareSettings
    {
        public static string Folder => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL");
        private static string FilePath => Path.Combine(Folder, "compare.json");
        private const int MaxRecent = 10;

        public List<string> Recent { get; } = new List<string>();
        public List<string> RecentServers { get; } = new List<string>();
        public List<string> MySchemaDefaults { get; set; }
        public List<string> MyDataDefaults { get; set; }
        /// <summary>Open the last project when the window opens.</summary>
        public bool ReopenLast { get; set; } = true;
        /// <summary>Show My projects when the window opens.</summary>
        public bool ShowMyProjects { get; set; }
        public bool ViewReport { get; set; } = true;
        /// <summary>The folder My projects last looked in.</summary>
        public string ProjectsFolder { get; set; }
        /// <summary>Application options: the deployment script's transaction isolation level.</summary>
        public string IsolationLevel { get; set; } = "SERIALIZABLE";
        /// <summary>Data deployments: split the transaction into batches of this many MB.</summary>
        public bool SplitTransactions { get; set; }
        public int MaxTransactionSizeMB { get; set; } = 10;
        /// <summary>Where deployment scripts are saved, and whether the last folder used becomes it.</summary>
        public string ScriptFolder { get; set; } = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare", "Scripts");
        public bool UpdateScriptFolder { get; set; } = true;
        /// <summary>Encoding of saved deployment scripts and of CSV exports: utf8, utf8bom, unicode or ascii.</summary>
        public string ScriptEncoding { get; set; } = "utf8bom";
        public string CsvEncoding { get; set; } = "utf8bom";
        /// <summary>Where filter files are opened and saved.</summary>
        public string FilterFolder { get; set; } = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "QuillSQL Compare", "Filters");
        /// <summary>The program deployment scripts open in (QuillCompare.exe): "" = the program for .sql files, a path to Ssms.exe, or "custom".</summary>
        public string SqlEditor { get; set; } = "";
        public string CustomEditor { get; set; } = "";
        public string CustomEditorArguments { get; set; } = "\"%SF%\"";
        /// <summary>The deployment wizard's last choices.</summary>
        public JsonObject Wizard { get; set; } = new JsonObject();
        public List<string> RecentFolders { get; } = new List<string>();
        public List<string> RecentSnapshots { get; } = new List<string>();
        public List<string> RecentFilters { get; } = new List<string>();

        public static CompareSettings Load()
        {
            var s = new CompareSettings();
            try
            {
                if (File.Exists(FilePath) && Json.Parse(File.ReadAllText(FilePath)) is JsonObject o)
                {
                    s.Recent.AddRange(o.Strings("recent"));
                    s.RecentServers.AddRange(o.Strings("recentServers"));
                    if (o.ContainsKey("mySchemaDefaults")) s.MySchemaDefaults = o.Strings("mySchemaDefaults");
                    if (o.ContainsKey("myDataDefaults")) s.MyDataDefaults = o.Strings("myDataDefaults");
                    if (o.ContainsKey("reopenLast")) s.ReopenLast = o.Bool("reopenLast");
                    s.ShowMyProjects = o.Bool("showMyProjects");
                    if (o.ContainsKey("viewReport")) s.ViewReport = o.Bool("viewReport");
                    s.ProjectsFolder = o.Str("projectsFolder");
                    if (o.Str("isolationLevel") != null) s.IsolationLevel = o.Str("isolationLevel");
                    s.SplitTransactions = o.Bool("splitTransactions");
                    if (o.Long("maxTransactionSizeMB") > 0) s.MaxTransactionSizeMB = (int)o.Long("maxTransactionSizeMB");
                    if (o.Str("scriptFolder") != null) s.ScriptFolder = o.Str("scriptFolder");
                    if (o.ContainsKey("updateScriptFolder")) s.UpdateScriptFolder = o.Bool("updateScriptFolder");
                    if (o.Str("scriptEncoding") != null) s.ScriptEncoding = o.Str("scriptEncoding");
                    if (o.Str("csvEncoding") != null) s.CsvEncoding = o.Str("csvEncoding");
                    if (o.Str("filterFolder") != null) s.FilterFolder = o.Str("filterFolder");
                    s.SqlEditor = o.Str("sqlEditor") ?? "";
                    s.CustomEditor = o.Str("customEditor") ?? "";
                    if (o.Str("customEditorArguments") != null) s.CustomEditorArguments = o.Str("customEditorArguments");
                    s.Wizard = o.Obj("wizard") ?? new JsonObject();
                    s.RecentFolders.AddRange(o.Strings("recentFolders"));
                    s.RecentSnapshots.AddRange(o.Strings("recentSnapshots"));
                    s.RecentFilters.AddRange(o.Strings("recentFilters"));
                }
            }
            catch (Exception ex) when (ex is IOException || ex is FormatException || ex is UnauthorizedAccessException)
            {
            }
            return s;
        }

        public void Save()
        {
            try
            {
                Directory.CreateDirectory(Folder);
                var o = new JsonObject
                {
                    ["recent"] = Recent,
                    ["recentServers"] = RecentServers,
                    ["reopenLast"] = ReopenLast,
                    ["showMyProjects"] = ShowMyProjects,
                    ["viewReport"] = ViewReport,
                    ["isolationLevel"] = IsolationLevel,
                    ["splitTransactions"] = SplitTransactions,
                    ["maxTransactionSizeMB"] = MaxTransactionSizeMB,
                    ["scriptFolder"] = ScriptFolder,
                    ["updateScriptFolder"] = UpdateScriptFolder,
                    ["scriptEncoding"] = ScriptEncoding,
                    ["csvEncoding"] = CsvEncoding,
                    ["filterFolder"] = FilterFolder,
                    ["sqlEditor"] = SqlEditor,
                    ["customEditor"] = CustomEditor,
                    ["customEditorArguments"] = CustomEditorArguments,
                    ["wizard"] = Wizard,
                    ["recentFolders"] = RecentFolders,
                    ["recentSnapshots"] = RecentSnapshots,
                    ["recentFilters"] = RecentFilters,
                };
                if (MySchemaDefaults != null) o["mySchemaDefaults"] = MySchemaDefaults;
                if (MyDataDefaults != null) o["myDataDefaults"] = MyDataDefaults;
                if (ProjectsFolder != null) o["projectsFolder"] = ProjectsFolder;
                File.WriteAllText(FilePath, Json.Indented(o));
            }
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException)
            {
            }
        }

        public void AddRecent(string path)
        {
            Recent.RemoveAll(p => string.Equals(p, path, StringComparison.OrdinalIgnoreCase));
            Recent.Insert(0, path);
            if (Recent.Count > MaxRecent) Recent.RemoveRange(MaxRecent, Recent.Count - MaxRecent);
            Save();
        }

        /// <summary>Puts a path first in one of the recent lists (scripts folders, snapshots, filters).</summary>
        public void AddRecent(List<string> list, string path)
        {
            if (string.IsNullOrWhiteSpace(path)) return;
            list.RemoveAll(p => string.Equals(p, path, StringComparison.OrdinalIgnoreCase));
            list.Insert(0, path);
            if (list.Count > MaxRecent) list.RemoveRange(MaxRecent, list.Count - MaxRecent);
            Save();
        }

        /// <summary>The encoding chosen for scripts or CSV files.</summary>
        public static Encoding EncodingOf(string name)
        {
            switch (name)
            {
                case "utf8": return new UTF8Encoding(false);
                case "unicode": return new UnicodeEncoding(false, true);
                case "ascii": return Encoding.ASCII;
                default: return new UTF8Encoding(true);
            }
        }

        public void AddServer(string server)
        {
            if (string.IsNullOrWhiteSpace(server)) return;
            RecentServers.RemoveAll(p => string.Equals(p, server, StringComparison.OrdinalIgnoreCase));
            RecentServers.Insert(0, server);
            if (RecentServers.Count > 20) RecentServers.RemoveRange(20, RecentServers.Count - 20);
            Save();
        }
    }
}
