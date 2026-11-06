using System;
using System.IO;

namespace QuillSql.Vsix
{
    /// <summary>Appends to %LOCALAPPDATA%\QuillSQL\quillsql.log. Never throws.</summary>
    internal static class Log
    {
        private static readonly object Gate = new object();
        private static readonly string Path = System.IO.Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "quillsql.log");

        public static string FilePath => Path;

        public static void Write(string message)
        {
            try
            {
                lock (Gate)
                {
                    Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path));
                    File.AppendAllText(Path, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss.fff ") + message + Environment.NewLine);
                }
            }
            catch
            {
                // Logging must never break the editor.
            }
        }

        public static void Error(string context, Exception ex) => Write("ERROR " + context + ": " + ex);

        /// <summary>Per-keystroke detail, written only while a file named `verbose` sits next to the log.</summary>
        public static void Verbose(string message)
        {
            if (File.Exists(System.IO.Path.Combine(System.IO.Path.GetDirectoryName(Path), "verbose")))
            {
                Write(message);
            }
        }
    }
}
