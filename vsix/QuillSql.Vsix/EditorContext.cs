using System;
using System.Linq;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using DTE = EnvDTE.DTE;
using TextDocument = EnvDTE.TextDocument;
using Window = System.Windows.Window;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    /// <summary>The active SQL editor: its text, caret and selection as UTF-16 offsets.</summary>
    internal sealed class EditorContext
    {
        /// <summary>Language services SSMS assigns to T-SQL buffers: SSMS 21, then older versions.</summary>
        public static readonly Guid[] SqlLanguageServices =
        {
            new Guid("C4D96929-A9B0-42CC-B3E0-ADAC0435D7F2"),
            new Guid("52692960-56BC-4989-B5D3-94C47A513E8D"),
        };

        public IVsTextView View { get; private set; }
        public IVsTextLines Lines { get; private set; }
        public string Text { get; private set; }
        public int Caret { get; private set; }
        public int SelectionStart { get; private set; }
        public int SelectionEnd { get; private set; }
        public bool HasSelection => SelectionEnd > SelectionStart;

        public static bool IsSql(IVsTextLines lines)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return lines != null && lines.GetLanguageServiceID(out var id) == VSConstants.S_OK && Array.IndexOf(SqlLanguageServices, id) >= 0;
        }

        /// <summary>
        /// The T-SQL editor of the active document window, or null. This is the query window even
        /// while a tool window such as Output has the keyboard focus.
        /// </summary>
        public static EditorContext Active()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (Package.GetGlobalService(typeof(SVsShellMonitorSelection)) is IVsMonitorSelection monitor
                && monitor.GetCurrentElementValue((uint)VSConstants.VSSELELEMID.SEID_DocumentFrame, out var frame) == VSConstants.S_OK
                && frame is IVsWindowFrame window
                && VsShellUtilities.GetTextView(window) is IVsTextView documentView)
            {
                return For(documentView);
            }
            if (Package.GetGlobalService(typeof(SVsTextManager)) is IVsTextManager manager
                && manager.GetActiveView(1, null, out var view) == VSConstants.S_OK && view != null)
            {
                return For(view);
            }
            return null;
        }

        public static EditorContext For(IVsTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (view.GetBuffer(out var lines) != VSConstants.S_OK || !IsSql(lines)
                || lines.GetLastLineIndex(out var lastLine, out var lastIndex) != VSConstants.S_OK
                || lines.GetLineText(0, 0, lastLine, lastIndex, out var text) != VSConstants.S_OK
                || view.GetCaretPos(out var line, out var column) != VSConstants.S_OK
                || lines.GetPositionOfLineIndex(line, column, out var caret) != VSConstants.S_OK)
            {
                return null;
            }
            var context = new EditorContext { View = view, Lines = lines, Text = text, Caret = caret, SelectionStart = caret, SelectionEnd = caret };
            if (view.GetSelection(out var l1, out var c1, out var l2, out var c2) == VSConstants.S_OK
                && lines.GetPositionOfLineIndex(l1, c1, out var p1) == VSConstants.S_OK
                && lines.GetPositionOfLineIndex(l2, c2, out var p2) == VSConstants.S_OK)
            {
                context.SelectionStart = Math.Min(p1, p2);
                context.SelectionEnd = Math.Max(p1, p2);
            }
            return context;
        }

        /// <summary>Replaces [start, end) with text as one undo step.</summary>
        public void Replace(int start, int end, string text)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Lines.GetLineIndexOfPosition(start, out var startLine, out var startColumn);
            Lines.GetLineIndexOfPosition(end, out var endLine, out var endColumn);
            var buffer = Marshal.StringToCoTaskMemUni(text);
            try
            {
                Lines.ReplaceLines(startLine, startColumn, endLine, endColumn, buffer, text.Length, null);
            }
            finally
            {
                Marshal.FreeCoTaskMem(buffer);
            }
        }

        /// <summary>
        /// Replaces the whole script with a rewritten one, touching only the part that changed so
        /// the caret and bookmarks elsewhere stay put. Returns false if nothing changed.
        /// </summary>
        public bool ReplaceAll(string rewritten)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (rewritten == Text)
            {
                return false;
            }
            var prefix = 0;
            var max = Math.Min(Text.Length, rewritten.Length);
            while (prefix < max && Text[prefix] == rewritten[prefix])
            {
                prefix++;
            }
            var suffix = 0;
            while (suffix < max - prefix && Text[Text.Length - 1 - suffix] == rewritten[rewritten.Length - 1 - suffix])
            {
                suffix++;
            }
            // Never split a surrogate pair or a CR LF.
            while (prefix > 0 && (char.IsLowSurrogate(Text[prefix]) || (Text[prefix] == '\n' && Text[prefix - 1] == '\r')))
            {
                prefix--;
            }
            while (suffix > 0 && (char.IsLowSurrogate(Text[Text.Length - suffix]) || (Text[Text.Length - suffix] == '\n' && Text.Length - suffix > 0 && Text[Text.Length - suffix - 1] == '\r')))
            {
                suffix--;
            }
            Replace(prefix, Text.Length - suffix, rewritten.Substring(prefix, rewritten.Length - suffix - prefix));
            return true;
        }

        public void Select(int start, int end)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Lines.GetLineIndexOfPosition(start, out var l1, out var c1);
            Lines.GetLineIndexOfPosition(end, out var l2, out var c2);
            View.SetSelection(l1, c1, l2, c2);
            View.EnsureSpanVisible(new TextSpan { iStartLine = l1, iStartIndex = c1, iEndLine = l2, iEndIndex = c2 });
        }

        public static DTE Dte
        {
            get
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                return Package.GetGlobalService(typeof(DTE)) as DTE;
            }
        }

        public static void Status(string message)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var dte = Dte;
            if (dte != null)
            {
                dte.StatusBar.Text = "QuillSQL: " + Loc.T(message);
            }
        }

        /// <summary>Opens a new query window on the current connection and puts the script in it.</summary>
        public static void OpenInNewQuery(string script)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var dte = Dte;
            if (dte == null)
            {
                return;
            }
            dte.ExecuteCommand("File.NewQuery");
            if (dte.ActiveDocument?.Object("TextDocument") is TextDocument document)
            {
                document.StartPoint.CreateEditPoint().Insert(script);
            }
        }

        /// <summary>
        /// Opens a new query window connected to `server` and `database` through SSMS's script factory,
        /// with SSMS's authentication type (0 Windows, 1 SQL Server, 2 to 9 Microsoft Entra) and user,
        /// and puts `script` in it; false when SSMS could not open it. No password is passed: a SQL
        /// Server login uses the one SSMS remembers, or SSMS asks.
        /// </summary>
        public static bool OpenConnected(string script, string server, string database, int authentication, string user, bool trustServerCertificate)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            const System.Reflection.BindingFlags Instance = System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.Public | System.Reflection.BindingFlags.NonPublic;
            Type find(string name) => AppDomain.CurrentDomain.GetAssemblies()
                .Select(a => { try { return a.GetType(name, false); } catch (Exception) { return null; } })
                .FirstOrDefault(t => t != null);
            try
            {
                var factory = find("Microsoft.SqlServer.Management.UI.VSIntegration.ServiceCache")
                    ?.GetProperty("ScriptFactory", System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.Public | System.Reflection.BindingFlags.NonPublic)?.GetValue(null);
                var infoType = find("Microsoft.SqlServer.Management.Smo.RegSvrEnum.UIConnectionInfo");
                var scriptType = find("Microsoft.SqlServer.Management.UI.VSIntegration.Editors.ScriptType");
                var create = factory?.GetType().GetMethods(Instance).FirstOrDefault(m => m.Name == "CreateNewBlankScript" && m.GetParameters().Length == 3);
                if (create == null || infoType == null || scriptType == null)
                {
                    return false;
                }
                var info = Activator.CreateInstance(infoType);
                void set(string name, object value) => infoType.GetProperty(name, Instance)?.SetValue(info, value);
                set("ServerName", server);
                // The database engine's server type.
                set("ServerType", new Guid("8c91a03d-f9b4-46c0-a305-b5dcc79ff907"));
                set("AuthenticationType", authentication);
                if (!string.IsNullOrEmpty(user))
                {
                    set("UserName", user);
                }
                if (infoType.GetProperty("AdvancedOptions", Instance)?.GetValue(info) is System.Collections.Specialized.NameValueCollection options)
                {
                    if (!string.IsNullOrEmpty(database))
                    {
                        options["DATABASE"] = database;
                    }
                    options["TRUST_SERVER_CERTIFICATE"] = trustServerCertificate ? "True" : "False";
                }
                create.Invoke(factory, new[] { Enum.Parse(scriptType, "Sql"), info, null });
                return string.IsNullOrEmpty(script) || InsertInActive(script);
            }
            catch (Exception ex) when (ex is System.Reflection.TargetInvocationException || ex is ArgumentException || ex is InvalidOperationException || ex is MissingMethodException || ex is COMException)
            {
                Log.Error("opening a connected query window", ex);
                return false;
            }
        }

        /// <summary>Puts the script into the active query window (one just opened); false when there is none.</summary>
        public static bool InsertInActive(string script)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!(Dte?.ActiveDocument?.Object("TextDocument") is TextDocument document)) return false;
            document.StartPoint.CreateEditPoint().Insert(script);
            return true;
        }

        /// <summary>A small modal prompt; returns null when cancelled.</summary>
        public static string Prompt(string title, string label, string initial)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var box = new TextBox { Text = initial ?? "", Margin = new Thickness(0, 8, 0, 0), MinWidth = 380, MinHeight = 26, Padding = new Thickness(4, 2, 4, 2), VerticalContentAlignment = VerticalAlignment.Center };
            var ok = Theme.DialogButton(Loc.T("OK"), isDefault: true);
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isCancel: true);
            var body = new StackPanel { Margin = new Thickness(20, 18, 20, 20) };
            body.Children.Add(new TextBlock { Text = Loc.T(label), TextWrapping = TextWrapping.Wrap, MaxWidth = 440, HorizontalAlignment = HorizontalAlignment.Left });
            body.Children.Add(box);
            var root = new DockPanel();
            var footer = Theme.Footer(null, ok, cancel);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(body);
            var window = Theme.Dialog(Loc.T(title), false);
            window.Content = root;
            window.SizeToContent = SizeToContent.WidthAndHeight;
            SetOwner(window);
            ok.Click += (s, e) => window.DialogResult = true;
            window.Loaded += (s, e) =>
            {
                box.Focus();
                box.SelectAll();
            };
            return window.ShowDialog() == true ? box.Text : null;
        }

        /// <summary>
        /// A modal yes/no question; true when the user chooses `action`. With `safeDefault`, Enter
        /// and the initial focus pick Cancel, for questions about something that cannot be undone.
        /// </summary>
        public static bool Confirm(string title, string message, string action, bool safeDefault = false)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var ok = Theme.DialogButton(Loc.T(action), isDefault: !safeDefault);
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isDefault: safeDefault, isCancel: true);
            // A warning sign for questions about something that cannot be undone, otherwise an info sign.
            var icon = Theme.Icon(safeDefault ? Theme.WarningGlyph : Theme.InfoGlyph, 28, Theme.Frozen(safeDefault ? Theme.Amber : Theme.Blue));
            icon.VerticalAlignment = VerticalAlignment.Top;
            icon.Margin = new Thickness(0, 0, 16, 0);
            var body = new DockPanel { Margin = new Thickness(20, 20, 24, 22) };
            DockPanel.SetDock(icon, Dock.Left);
            body.Children.Add(icon);
            body.Children.Add(new TextBlock { Text = Loc.T(message), TextWrapping = TextWrapping.Wrap, MaxWidth = 460, MinWidth = 260, LineHeight = 19, VerticalAlignment = VerticalAlignment.Center });
            var root = new DockPanel();
            var footer = Theme.Footer(null, ok, cancel);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(body);
            var window = Theme.Dialog(Loc.T(title), false);
            window.Content = root;
            window.SizeToContent = SizeToContent.WidthAndHeight;
            SetOwner(window);
            ok.Click += (s, e) => window.DialogResult = true;
            window.Loaded += (s, e) => (safeDefault ? cancel : ok).Focus();
            return window.ShowDialog() == true;
        }

        public static void SetOwner(Window window)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var dte = Dte;
            if (dte != null)
            {
                new WindowInteropHelper(window).Owner = (IntPtr)dte.MainWindow.HWnd;
            }
        }
    }
}
