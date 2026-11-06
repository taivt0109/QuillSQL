using System;
using System.Collections.Generic;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.OLE.Interop;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;
using IServiceProvider = System.IServiceProvider;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Attaches a <see cref="CompletionController"/> to every editor view: views open before
    /// the package loaded, views registered later, and any view that becomes active.
    /// </summary>
    internal sealed class EditorWatcher : IVsTextManagerEvents, IVsSelectionEvents
    {
        private readonly Dictionary<IVsTextView, CompletionController> _controllers = new Dictionary<IVsTextView, CompletionController>();
        private IConnectionPoint _point;
        private uint _cookie;
        private uint _selectionCookie;

        public static EditorWatcher Start()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var watcher = new EditorWatcher();
            if (Package.GetGlobalService(typeof(SVsTextManager)) is IVsTextManager manager && manager is IConnectionPointContainer container)
            {
                var iid = typeof(IVsTextManagerEvents).GUID;
                container.FindConnectionPoint(ref iid, out watcher._point);
                watcher._point?.Advise(watcher, out watcher._cookie);
            }
            if (Package.GetGlobalService(typeof(SVsShellMonitorSelection)) is IVsMonitorSelection monitor)
            {
                monitor.AdviseSelectionEvents(watcher, out watcher._selectionCookie);
            }
            if (Package.GetGlobalService(typeof(SVsUIShell)) is IVsUIShell shell
                && shell.GetDocumentWindowEnum(out var frames) == VSConstants.S_OK)
            {
                var one = new IVsWindowFrame[1];
                while (frames.Next(1, one, out var fetched) == VSConstants.S_OK && fetched == 1)
                {
                    watcher.AttachToFrame(one[0]);
                }
            }
            Log.Write("editor watcher started with " + watcher._controllers.Count + " open view(s)");
            return watcher;
        }

        private void AttachToFrame(IVsWindowFrame frame)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var view = frame == null ? null : VsShellUtilities.GetTextView(frame);
            if (view != null)
            {
                OnRegisterView(view);
            }
        }

        public int OnElementValueChanged(uint elementId, object oldValue, object newValue)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (elementId == (uint)VSConstants.VSSELELEMID.SEID_DocumentFrame)
            {
                AttachToFrame(newValue as IVsWindowFrame);
            }
            return VSConstants.S_OK;
        }

        public int OnSelectionChanged(IVsHierarchy oldHierarchy, uint oldItem, IVsMultiItemSelect oldMulti, ISelectionContainer oldContainer,
            IVsHierarchy newHierarchy, uint newItem, IVsMultiItemSelect newMulti, ISelectionContainer newContainer) => VSConstants.S_OK;

        public int OnCmdUIContextChanged(uint cookie, int active) => VSConstants.S_OK;

        public void OnRegisterView(IVsTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (view != null && !_controllers.ContainsKey(view))
                {
                    var controller = CompletionController.Attach(view);
                    if (controller != null)
                    {
                        _controllers[view] = controller;
                        Log.Verbose("attached to a view (" + _controllers.Count + " total) hwnd=" + view.GetWindowHandle().ToInt64());
                    }
                }
            }
            catch (Exception ex)
            {
                Log.Error("attaching to view", ex);
            }
        }

        public void OnUnregisterView(IVsTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (view != null && _controllers.TryGetValue(view, out var controller))
                {
                    _controllers.Remove(view);
                    controller.Detach();
                }
            }
            catch (Exception ex)
            {
                Log.Error("detaching from view", ex);
            }
        }

        public void OnRegisterMarkerType(int markerType) { }

        public void OnUserPreferencesChanged(VIEWPREFERENCES[] viewPrefs, FRAMEPREFERENCES[] framePrefs, LANGPREFERENCES[] langPrefs, FONTCOLORPREFERENCES[] colorPrefs) { }
    }
}
