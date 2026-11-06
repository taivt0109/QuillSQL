using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net.Http;
using System.Runtime.Serialization;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Microsoft.VisualStudio.Imaging;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class GitHubRelease
    {
        [DataMember(Name = "tag_name")] public string Tag { get; set; }
        [DataMember(Name = "html_url")] public string Page { get; set; }
        [DataMember(Name = "draft")] public bool Draft { get; set; }
        [DataMember(Name = "prerelease")] public bool Prerelease { get; set; }
        [DataMember(Name = "assets")] public GitHubAsset[] Assets { get; set; }
    }

    [DataContract]
    internal sealed class GitHubAsset
    {
        [DataMember(Name = "name")] public string Name { get; set; }
        [DataMember(Name = "browser_download_url")] public string Url { get; set; }
    }

    /// <summary>
    /// Updates from the project's GitHub releases. Once a day it looks at the latest release; a
    /// newer one is downloaded and checked against the release's SHA256SUMS.txt, and the info bar
    /// offers to install it. SSMS cannot replace a loaded extension, so a small script waits for
    /// SSMS to close and runs VSIXInstaller then.
    /// </summary>
    internal sealed class Updater : IVsInfoBarUIEvents
    {
        public const string Repository = "taivt0109/QuillSQL";
        public static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL");
        private static readonly TimeSpan Interval = TimeSpan.FromDays(1);
        private readonly AsyncPackage _package;
        private string _version;
        private string _vsix;
        private string _page;
        private uint _cookie;

        private Updater(AsyncPackage package) => _package = package;

        /// <summary>The releases API; QUILLSQL_UPDATE_URL points it at a mirror.</summary>
        private static string LatestUrl =>
            Environment.GetEnvironmentVariable("QUILLSQL_UPDATE_URL") ?? "https://api.github.com/repos/" + Repository + "/releases/latest";

        private static string Stamp => Path.Combine(Folder, "update-check.txt");

        /// <summary>Checks in the background, at most once a day unless `now`.</summary>
        public static void Start(AsyncPackage package, bool now)
        {
            var updater = new Updater(package);
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                try
                {
                    if (!now)
                    {
                        if (!Settings.Current.AutoUpdate)
                        {
                            return;
                        }
                        if (File.Exists(Stamp) && DateTime.UtcNow - File.GetLastWriteTimeUtc(Stamp) < Interval)
                        {
                            return;
                        }
                        // Let SSMS finish starting first.
                        await Task.Delay(TimeSpan.FromSeconds(20), package.DisposalToken);
                    }
                    await updater.CheckAsync(now);
                }
                catch (OperationCanceledException)
                {
                    // SSMS is closing.
                }
                catch (Exception ex)
                {
                    Log.Error("checking for updates", ex);
                    if (now)
                    {
                        await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                        EditorContext.Status("could not check for updates: " + ex.Message);
                    }
                }
            });
        }

        private static Version Parse(string tag) =>
            Version.TryParse((tag ?? "").TrimStart('v', 'V').Split('-')[0], out var v) ? v : new Version(0, 0);

        private static HttpClient Client()
        {
            var client = new HttpClient { Timeout = TimeSpan.FromMinutes(5) };
            client.DefaultRequestHeaders.UserAgent.ParseAdd("QuillSQL/" + QuillSqlPackage.Version);
            client.DefaultRequestHeaders.Accept.ParseAdd("application/vnd.github+json");
            return client;
        }

        private async Task CheckAsync(bool explicitCheck)
        {
            Directory.CreateDirectory(Folder);
            File.WriteAllText(Stamp, DateTime.UtcNow.ToString("s"));
            GitHubRelease release;
            using (var client = Client())
            {
                release = Json.Parse<GitHubRelease>(await client.GetStringAsync(LatestUrl));
            }
            var latest = Parse(release?.Tag);
            var current = Parse(QuillSqlPackage.Version);
            var skipped = File.Exists(SkipFile) ? File.ReadAllText(SkipFile).Trim() : "";
            Log.Write("update check: installed " + current + ", latest " + latest + " (" + release?.Tag + ")");
            if (release == null || release.Draft || release.Prerelease || latest <= current || (!explicitCheck && skipped == latest.ToString()))
            {
                if (explicitCheck)
                {
                    await _package.JoinableTaskFactory.SwitchToMainThreadAsync();
                    EditorContext.Status("version " + current + " is the latest.");
                }
                return;
            }
            var vsix = release.Assets?.FirstOrDefault(a => a.Name.EndsWith(".vsix", StringComparison.OrdinalIgnoreCase));
            var sums = release.Assets?.FirstOrDefault(a => a.Name.Equals("SHA256SUMS.txt", StringComparison.OrdinalIgnoreCase));
            if (vsix == null || sums == null)
            {
                Log.Write("update " + latest + " has no .vsix or SHA256SUMS.txt; not offered");
                return;
            }
            var folder = Path.Combine(Folder, "updates", latest.ToString());
            Directory.CreateDirectory(folder);
            var file = Path.Combine(folder, vsix.Name);
            using (var client = Client())
            {
                var expected = (await client.GetStringAsync(sums.Url))
                    .Split('\n')
                    .Select(l => l.Trim().Split(new[] { ' ', '*' }, StringSplitOptions.RemoveEmptyEntries))
                    .Where(p => p.Length == 2 && p[1].Equals(vsix.Name, StringComparison.OrdinalIgnoreCase))
                    .Select(p => p[0].ToLowerInvariant())
                    .FirstOrDefault();
                if (expected == null)
                {
                    Log.Write("update " + latest + ": SHA256SUMS.txt does not list " + vsix.Name + "; not offered");
                    return;
                }
                if (!File.Exists(file) || Sha256(file) != expected)
                {
                    var bytes = await client.GetByteArrayAsync(vsix.Url);
                    File.WriteAllBytes(file, bytes);
                }
                var actual = Sha256(file);
                if (actual != expected)
                {
                    File.Delete(file);
                    Log.Write("update " + latest + ": checksum mismatch (expected " + expected + ", got " + actual + "); discarded");
                    return;
                }
            }
            Log.Write("update " + latest + " downloaded and verified: " + file);
            _version = latest.ToString();
            _vsix = file;
            _page = release.Page;
            await _package.JoinableTaskFactory.SwitchToMainThreadAsync();
            ShowInfoBar();
        }

        private static string SkipFile => Path.Combine(Folder, "update-skip.txt");

        private static string Sha256(string file)
        {
            using (var sha = SHA256.Create())
            using (var stream = File.OpenRead(file))
            {
                return string.Concat(sha.ComputeHash(stream).Select(b => b.ToString("x2")));
            }
        }

        private void ShowInfoBar()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!(Package.GetGlobalService(typeof(SVsShell)) is IVsShell shell)
                || shell.GetProperty((int)__VSSPROPID7.VSSPROPID_MainWindowInfoBarHost, out var hostObject) != 0
                || !(hostObject is IVsInfoBarHost host)
                || !(Package.GetGlobalService(typeof(SVsInfoBarUIFactory)) is IVsInfoBarUIFactory factory))
            {
                EditorContext.Status("version " + _version + " is available; run Tools > QuillSQL > Check for Updates.");
                return;
            }
            var model = new InfoBarModel(
                new[] { new InfoBarTextSpan(Loc.T("QuillSQL " + _version + " is available (you have " + QuillSqlPackage.Version + "). ")) },
                new IVsInfoBarActionItem[]
                {
                    new InfoBarHyperlink(Loc.T("Install when SSMS closes"), "install"),
                    new InfoBarHyperlink(Loc.T("What's new"), "notes"),
                    new InfoBarHyperlink(Loc.T("Skip this version"), "skip"),
                },
                KnownMonikers.StatusInformation,
                true);
            var element = factory.CreateInfoBar(model);
            element.Advise(this, out _cookie);
            host.AddInfoBar(element);
            Log.Write("update " + _version + " offered in the info bar");
        }

        public void OnActionItemClicked(IVsInfoBarUIElement element, IVsInfoBarActionItem item)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                switch (item.ActionContext as string)
                {
                    case "install":
                        ScheduleInstall();
                        element.Close();
                        EditorContext.Status("version " + _version + " will be installed when you close SSMS.");
                        break;
                    case "notes":
                        Process.Start(new ProcessStartInfo(_page ?? "https://github.com/" + Repository + "/releases") { UseShellExecute = true });
                        break;
                    case "skip":
                        File.WriteAllText(SkipFile, _version);
                        element.Close();
                        break;
                }
            }
            catch (Exception ex)
            {
                Log.Error("update action", ex);
                EditorContext.Status("update failed: " + ex.Message);
            }
        }

        public void OnClosed(IVsInfoBarUIElement element)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            element.Unadvise(_cookie);
        }

        /// <summary>
        /// Starts a hidden PowerShell script that waits for this SSMS to exit, installs the update
        /// into the same SSMS instance and clears that instance's cache of editor components.
        /// </summary>
        private void ScheduleInstall()
        {
            var ide = Path.GetDirectoryName(Process.GetCurrentProcess().MainModule.FileName);
            var installer = Path.Combine(ide, "VSIXInstaller.exe");
            // This assembly lives in <instance>\Extensions\<random>\, for example 21.0_cc31859bExp.
            var instance = new DirectoryInfo(Path.GetDirectoryName(typeof(Updater).Assembly.Location)).Parent?.Parent;
            var suffix = instance == null ? "" : System.Text.RegularExpressions.Regex.Match(instance.Name, @"^\d+\.\d+_[0-9a-f]{8}(.*)$").Groups[1].Value;
            var script = Path.Combine(Folder, "updates", "install-update.ps1");
            File.WriteAllText(script, @"param([int]$WaitFor, [string]$Installer, [string]$Vsix, [string]$RootSuffix, [string]$Cache, [string]$Log)
Wait-Process -Id $WaitFor -ErrorAction SilentlyContinue
$arguments = @('/quiet')
if ($RootSuffix) { $arguments += ""/rootSuffix:$RootSuffix"" }
$arguments += ('""' + $Vsix + '""')
$p = Start-Process -FilePath $Installer -ArgumentList $arguments -Wait -PassThru
if ($Cache) { Remove-Item -LiteralPath $Cache -Recurse -Force -ErrorAction SilentlyContinue }
Add-Content -LiteralPath $Log -Value ((Get-Date -Format 'yyyy-MM-dd HH:mm:ss.fff') + "" update installed from $Vsix, VSIXInstaller exit code $($p.ExitCode)"")
");
            string quote(string s) => "\"" + s + "\"";
            var cache = instance == null ? "" : Path.Combine(instance.FullName, "ComponentModelCache");
            var arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File " + quote(script)
                + " -WaitFor " + Process.GetCurrentProcess().Id
                + " -Installer " + quote(installer)
                + " -Vsix " + quote(_vsix)
                + (suffix.Length > 0 ? " -RootSuffix " + quote(suffix) : "")
                + " -Cache " + quote(cache)
                + " -Log " + quote(Log.FilePath);
            Process.Start(new ProcessStartInfo("powershell.exe", arguments) { UseShellExecute = false, CreateNoWindow = true });
            Log.Write("update " + _version + " scheduled for when SSMS closes" + (suffix.Length > 0 ? " (instance " + suffix + ")" : ""));
        }
    }
}
