using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The suggestion picked last time for the same typed letters, so typing them again selects
    /// it first. Kept in %LOCALAPPDATA%\QuillSQL\picks.txt, one "letters TAB inserted text" a line.
    /// </summary>
    internal static class Picks
    {
        private const int Keep = 2000;
        private static readonly string FilePath = Path.Combine(Updater.Folder, "picks.txt");
        private static readonly object Gate = new object();
        private static Dictionary<string, string> _picks;

        private static Dictionary<string, string> All()
        {
            if (_picks != null)
            {
                return _picks;
            }
            _picks = new Dictionary<string, string>(StringComparer.Ordinal);
            try
            {
                if (File.Exists(FilePath))
                {
                    foreach (var line in File.ReadAllLines(FilePath, Encoding.UTF8))
                    {
                        var tab = line.IndexOf('\t');
                        if (tab > 0)
                        {
                            _picks[line.Substring(0, tab)] = line.Substring(tab + 1).Replace("\\n", "\n");
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                Log.Error("reading " + FilePath, ex);
            }
            return _picks;
        }

        /// <summary>The text inserted last time these letters were typed, or null.</summary>
        public static string Last(string typed)
        {
            lock (Gate)
            {
                return All().TryGetValue(typed.ToLowerInvariant(), out var insert) ? insert : null;
            }
        }

        public static void Remember(string typed, string insert)
        {
            if (string.IsNullOrEmpty(typed) || string.IsNullOrEmpty(insert) || insert.Length > 400)
            {
                return;
            }
            lock (Gate)
            {
                var picks = All();
                var key = typed.ToLowerInvariant();
                // The newest pick goes last, so the file keeps the most recent ones.
                picks.Remove(key);
                picks[key] = insert;
                try
                {
                    Directory.CreateDirectory(Updater.Folder);
                    File.WriteAllLines(FilePath, picks.Skip(Math.Max(0, picks.Count - Keep)).Select(p => p.Key + "\t" + p.Value.Replace("\r", "").Replace("\n", "\\n")), new UTF8Encoding(false));
                }
                catch (Exception ex)
                {
                    Log.Error("saving " + FilePath, ex);
                }
            }
        }
    }
}
