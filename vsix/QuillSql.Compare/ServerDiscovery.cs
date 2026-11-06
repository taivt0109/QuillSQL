using System;
using System.Collections.Generic;
using System.Data;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>
    /// SQL Server instances to offer in the Server box, found as SQL Compare finds them: the SQL
    /// Server Browser (a UDP broadcast to port 1434), the .NET SQL client's enumerator, local
    /// instances in the registry, LocalDB, and servers registered in SSMS and Azure Data Studio.
    /// Each search gives up after the timeout; the result is kept for the session.
    /// </summary>
    internal static class ServerDiscovery
    {
        private static Task<List<string>> _found;

        public static Task<List<string>> Find() => _found ?? (_found = Task.Run(() => Discover(TimeSpan.FromSeconds(5))));

        public static async Task<List<string>> Discover(TimeSpan timeout)
        {
            var searches = new Func<Task<IEnumerable<string>>>[]
            {
                () => Browser(timeout),
                () => Task.Run(Enumerator),
                () => Task.Run(Registry),
                () => Task.Run(LocalDb),
                () => Task.Run(Ssms),
                () => Task.Run(AzureDataStudio),
            };
            var tasks = searches.Select(async s =>
            {
                try
                {
                    var work = s();
                    return await Task.WhenAny(work, Task.Delay(timeout)) == work ? work.Result : Enumerable.Empty<string>();
                }
                catch (Exception ex) when (!(ex is OutOfMemoryException))
                {
                    Debug.WriteLine("server discovery: " + ex.Message);
                    return Enumerable.Empty<string>();
                }
            });
            var all = (await Task.WhenAll(tasks)).SelectMany(x => x).Where(x => !string.IsNullOrWhiteSpace(x));
            return Local(all).Distinct(StringComparer.InvariantCultureIgnoreCase).OrderBy(x => x, StringComparer.InvariantCultureIgnoreCase).ToList();
        }

        /// <summary>This machine's name becomes (local).</summary>
        private static IEnumerable<string> Local(IEnumerable<string> names)
        {
            var self = new Regex("^" + Regex.Escape(Environment.MachineName) + @"(\\[^\\]+)?$", RegexOptions.IgnoreCase);
            return names.Select(n => { var m = self.Match(n); return m.Success ? "(local)" + m.Groups[1].Value : n; });
        }

        /// <summary>One byte 2 broadcast to UDP 1434; each SQL Server Browser answers with its instances.</summary>
        private static async Task<IEnumerable<string>> Browser(TimeSpan timeout)
        {
            var found = new List<string>();
            using (var udp = new UdpClient { EnableBroadcast = true })
            {
                await udp.SendAsync(new byte[] { 2 }, 1, new IPEndPoint(IPAddress.Broadcast, 1434));
                var watch = Stopwatch.StartNew();
                while (watch.Elapsed < timeout)
                {
                    var receive = udp.ReceiveAsync();
                    if (await Task.WhenAny(receive, Task.Delay(TimeSpan.FromSeconds(1))) != receive) break;
                    found.AddRange(ParseBrowser(receive.Result.Buffer));
                }
            }
            return found;
        }

        /// <summary>The answer after its 3-byte header: `ServerName;A;InstanceName;B;…;;` per instance.</summary>
        public static IEnumerable<string> ParseBrowser(byte[] answer)
        {
            if (answer.Length <= 3) yield break;
            var text = Encoding.ASCII.GetString(answer, 3, answer.Length - 3);
            foreach (var instance in text.Split(new[] { ";;" }, StringSplitOptions.RemoveEmptyEntries))
            {
                var parts = instance.Split(';');
                var props = new Dictionary<string, string>();
                for (var i = 0; i + 1 < parts.Length; i += 2) props[parts[i]] = parts[i + 1];
                if (!props.TryGetValue("ServerName", out var server)) continue;
                yield return props.TryGetValue("InstanceName", out var name) && name != "MSSQLSERVER" ? server + "\\" + name : server;
            }
        }

        private static IEnumerable<string> Enumerator()
        {
            var table = System.Data.Sql.SqlDataSourceEnumerator.Instance.GetDataSources();
            return table.Rows.Cast<DataRow>().Select(r => string.IsNullOrEmpty(r["InstanceName"].ToString()) ? r["ServerName"].ToString() : r["ServerName"] + "\\" + r["InstanceName"]).ToList();
        }

        private static IEnumerable<RegistryKey> Hives() => new[] { RegistryView.Registry64, RegistryView.Registry32 }.Select(v => RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, v));

        /// <summary>Instances installed here: `(local)\NAME`, `(local)` for the default instance.</summary>
        private static IEnumerable<string> Registry()
        {
            var found = new List<string>();
            foreach (var hive in Hives())
            {
                using (var key = hive.OpenSubKey(@"SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL"))
                {
                    if (key == null) continue;
                    found.AddRange(key.GetValueNames().Where(v => !string.IsNullOrWhiteSpace(v)).Select(v => "(local)" + (v.Equals("MSSQLSERVER", StringComparison.OrdinalIgnoreCase) ? "" : "\\" + v)));
                }
            }
            return found;
        }

        private static readonly string[] Versions = { "90", "100", "110", "120", "130", "140", "150", "160", "170" };

        /// <summary>LocalDB instances, from `SqlLocalDB.exe i`.</summary>
        private static IEnumerable<string> LocalDb()
        {
            var tools = new List<string>();
            foreach (var hive in Hives())
            {
                foreach (var v in Versions)
                {
                    if (hive.OpenSubKey(@"SOFTWARE\Microsoft\Microsoft SQL Server\" + v + @"\Tools\Setup")?.GetValue("SQLPath") is string sql) tools.Add(Path.Combine(sql, "Binn", "SqlLocalDB.exe"));
                    if (hive.OpenSubKey(@"SOFTWARE\Microsoft\Microsoft SQL Server\" + v + @"\Tools\ClientSetup")?.GetValue("Path") is string client) tools.Add(Path.Combine(client, "SqlLocalDB.exe"));
                }
            }
            var found = new List<string>();
            foreach (var exe in tools.Distinct(StringComparer.OrdinalIgnoreCase).Where(File.Exists))
            {
                using (var p = Process.Start(new ProcessStartInfo(exe, "i") { CreateNoWindow = true, UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true }))
                {
                    if (p == null) continue;
                    var output = p.StandardOutput.ReadToEnd();
                    p.WaitForExit(5000);
                    found.AddRange(output.Split(new[] { '\r', '\n' }, StringSplitOptions.RemoveEmptyEntries).Select(l => "(localdb)\\" + l.Trim()));
                }
            }
            return found;
        }

        /// <summary>Servers registered in SSMS (`RegSrvr.xml`), but not `(local)` or `.`.</summary>
        private static IEnumerable<string> Ssms()
        {
            var found = new List<string>();
            var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
            foreach (var v in Versions)
            {
                var file = Path.Combine(appData, "Microsoft", "Microsoft SQL Server", v, "Tools", "Shell", "RegSrvr.xml");
                if (!File.Exists(file)) continue;
                try
                {
                    var doc = System.Xml.Linq.XDocument.Load(file);
                    found.AddRange(doc.Descendants().Where(e => e.Name.LocalName == "ServerName").Select(e => e.Value.ToUpperInvariant()).Where(s => s != "(LOCAL)" && s != "."));
                }
                catch (System.Xml.XmlException)
                {
                }
            }
            return found;
        }

        /// <summary>MSSQL connections saved in Azure Data Studio.</summary>
        private static IEnumerable<string> AzureDataStudio()
        {
            var file = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "azuredatastudio", "User", "settings.json");
            if (!File.Exists(file)) return Enumerable.Empty<string>();
            if (!(Json.Parse(File.ReadAllText(file)) is JsonObject settings)) return Enumerable.Empty<string>();
            return settings.List("datasource.connections").OfType<JsonObject>().Where(c => c.Str("providerName") == "MSSQL").Select(c => c.Obj("options")?.Str("server")).Where(s => s != null).ToList();
        }

        /// <summary>The Server box's list: "Recently used", then "Found on network" once the search ends.</summary>
        public static void Attach(ComboBox box)
        {
            void fill(List<string> found)
            {
                var text = box.Text;
                box.Items.Clear();
                var recent = CompareSettings.Load().RecentServers;
                if (recent.Count > 0)
                {
                    box.Items.Add(Header("Recently used"));
                    foreach (var s in recent) box.Items.Add(s);
                }
                box.Items.Add(Header("Found on network"));
                if (found == null) box.Items.Add(Header("Searching…", muted: true));
                else if (found.Count == 0) box.Items.Add(Header("No servers found", muted: true));
                else foreach (var s in found.Where(f => !recent.Contains(f, StringComparer.OrdinalIgnoreCase))) box.Items.Add(s);
                box.Text = text;
            }
            box.DropDownOpened += async (s, e) =>
            {
                var search = Find();
                fill(search.IsCompleted ? search.Result : null);
                if (search.IsCompleted) return;
                var found = await search;
                if (box.IsDropDownOpen) fill(found);
            };
            fill(_found != null && _found.IsCompleted ? _found.Result : null);
        }

        private static ComboBoxItem Header(string text, bool muted = false) => new ComboBoxItem
        {
            Content = text,
            IsEnabled = false,
            FontWeight = muted ? FontWeights.Normal : FontWeights.SemiBold,
            FontSize = muted ? 12 : 12.5,
        };
    }
}
