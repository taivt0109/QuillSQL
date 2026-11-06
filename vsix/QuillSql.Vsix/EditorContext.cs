using System;
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
                dte.StatusBar.Text = "QuillSQL: " + message;
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

        /// <summary>A small modal prompt; returns null when cancelled.</summary>
        public static string Prompt(string title, string label, string initial)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var box = new TextBox { Text = initial ?? "", Margin = new Thickness(0, 6, 0, 10), MinWidth = 320 };
            var ok = new Button { Content = "OK", IsDefault = true, MinWidth = 80, Margin = new Thickness(0, 0, 8, 0) };
            var cancel = new Button { Content = "Cancel", IsCancel = true, MinWidth = 80 };
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
            buttons.Children.Add(ok);
            buttons.Children.Add(cancel);
            var panel = new StackPanel { Margin = new Thickness(12) };
            panel.Children.Add(new TextBlock { Text = label, TextWrapping = TextWrapping.Wrap, MaxWidth = 420 });
            panel.Children.Add(box);
            panel.Children.Add(buttons);
            var window = new Window
            {
                Title = title,
                Content = panel,
                SizeToContent = SizeToContent.WidthAndHeight,
                ResizeMode = ResizeMode.NoResize,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
            };
            SetOwner(window);
            ok.Click += (s, e) => window.DialogResult = true;
            window.Loaded += (s, e) =>
            {
                box.Focus();
                box.SelectAll();
            };
            return window.ShowDialog() == true ? box.Text : null;
        }

        /// <summary>A modal yes/no question; true when the user chooses `action`.</summary>
        public static bool Confirm(string title, string message, string action)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var ok = new Button { Content = action, IsDefault = true, MinWidth = 90, Margin = new Thickness(0, 0, 8, 0) };
            var cancel = new Button { Content = "Cancel", IsCancel = true, MinWidth = 80 };
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 12, 0, 0) };
            buttons.Children.Add(ok);
            buttons.Children.Add(cancel);
            var panel = new StackPanel { Margin = new Thickness(14) };
            panel.Children.Add(new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap, MaxWidth = 440 });
            panel.Children.Add(buttons);
            var window = new Window
            {
                Title = title,
                Content = panel,
                SizeToContent = SizeToContent.WidthAndHeight,
                ResizeMode = ResizeMode.NoResize,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
            };
            SetOwner(window);
            ok.Click += (s, e) => window.DialogResult = true;
            window.Loaded += (s, e) => ok.Focus();
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
