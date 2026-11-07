using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    /// <summary>One open query window: its frame, tab name and connection.</summary>
    internal sealed class QueryWindow
    {
        public IVsWindowFrame Frame;
        public IVsTextView View;
        /// <summary>The whole tab caption, such as "SQLQuery1.sql - server.database (user (54))*".</summary>
        public string Caption;
        public ConnectionTarget Connection;

        /// <summary>The script, selection and caret, read now.</summary>
        public EditorContext Editor
        {
            get
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                return EditorContext.For(View);
            }
        }

        /// <summary>Whether text is selected, without reading the script.</summary>
        public bool HasSelection
        {
            get
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                return View.GetSelection(out var l1, out var c1, out var l2, out var c2) == VSConstants.S_OK && (l1 != l2 || c1 != c2);
            }
        }

        /// <summary>The file part of the caption, such as "SQLQuery1.sql".</summary>
        public string Name
        {
            get
            {
                var at = Caption.IndexOf(" - ", StringComparison.Ordinal);
                return (at > 0 ? Caption.Substring(0, at) : Caption).TrimEnd('*').Trim();
            }
        }

        /// <summary>The metadata key of the window's connection, loading its metadata if needed.</summary>
        public string Key()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (Connection == null || !Connection.CanQuery)
            {
                return "";
            }
            MetadataCache.EnsureLoaded(Connection);
            return Connection.Key;
        }
    }

    internal static class QueryWindows
    {
        /// <summary>The open T-SQL query windows, in tab order.</summary>
        public static List<QueryWindow> All()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var windows = new List<QueryWindow>();
            if (!(Package.GetGlobalService(typeof(SVsUIShell)) is IVsUIShell shell) || shell.GetDocumentWindowEnum(out var frames) != VSConstants.S_OK)
            {
                return windows;
            }
            var one = new IVsWindowFrame[1];
            while (frames.Next(1, one, out var fetched) == VSConstants.S_OK && fetched == 1)
            {
                var window = Read(one[0]);
                if (window != null)
                {
                    windows.Add(window);
                }
            }
            return windows;
        }

        /// <summary>The query window the user has open, or null.</summary>
        public static QueryWindow Active()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return Package.GetGlobalService(typeof(SVsShellMonitorSelection)) is IVsMonitorSelection monitor
                && monitor.GetCurrentElementValue((uint)VSConstants.VSSELELEMID.SEID_DocumentFrame, out var frame) == VSConstants.S_OK
                && frame is IVsWindowFrame active
                ? Read(active) : null;
        }

        /// <summary>The window of `frame` read again, or null when it was closed.</summary>
        public static QueryWindow Refresh(IVsWindowFrame frame)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return frame == null ? null : All().FirstOrDefault(w => ReferenceEquals(w.Frame, frame));
        }

        private static QueryWindow Read(IVsWindowFrame frame)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var view = VsShellUtilities.GetTextView(frame);
                if (view == null || view.GetBuffer(out var lines) != VSConstants.S_OK || !EditorContext.IsSql(lines))
                {
                    return null;
                }
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_Caption, out var caption);
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView);
                return new QueryWindow
                {
                    Frame = frame,
                    View = view,
                    Caption = caption as string ?? "",
                    Connection = docView == null ? null : ConnectionResolver.ForDocView(docView),
                };
            }
            catch (Exception ex)
            {
                Log.Error("reading a query window", ex);
                return null;
            }
        }
    }
}
