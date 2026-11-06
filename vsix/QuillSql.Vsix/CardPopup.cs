using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Text.Editor;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The object card opened from the keyboard (Show Object Card). SSMS closes Quick Info sessions
    /// started by a shortcut, so the card is shown in a popup of its own at the caret; it closes
    /// with Esc, a click elsewhere, an edit or a caret move.
    /// </summary>
    internal static class CardPopup
    {
        private static Popup _open;

        public static bool IsOpen => _open?.IsOpen == true;

        public static void Close()
        {
            if (_open != null)
            {
                _open.IsOpen = false;
                _open = null;
            }
        }

        /// <summary>Reads the object at the caret and shows its card; false when there is no query window.</summary>
        public static bool Show(IWpfTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (view == null || editor == null)
            {
                return false;
            }
            var key = Commands.MetadataKey(out _);
            var target = ConnectionResolver.TryGetActive();
            if (target == null || !target.CanQuery)
            {
                EditorContext.Status("open a query window connected with Windows or SQL Server authentication first.");
                return true;
            }
            var text = editor.Text;
            var caret = editor.Caret;
            var version = view.TextSnapshot.Version.VersionNumber;
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var at = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + caret);
                var found = at == null ? null : Json.Parse<ObjectAtReply>(at)?.Result;
                ObjectDetails details = null;
                if (found != null)
                {
                    try
                    {
                        details = ObjectScripts.Load(target, found.Schema + "." + found.Name);
                    }
                    catch (Exception ex)
                    {
                        Log.Error("reading object details", ex);
                    }
                }
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (details == null)
                {
                    EditorContext.Status("put the caret on a table, view, procedure or function.");
                    return;
                }
                if (view.IsClosed || view.TextSnapshot.Version.VersionNumber != version)
                {
                    return;
                }
                Log.Verbose("object card: " + details.FullName + "; " + ObjectScripts.StorageLine(details));
                Open(view, ObjectScripts.Card(details));
            }).FileAndForget("quillsql/objectcard");
            return true;
        }

        private static void Open(IWpfTextView view, UIElement card)
        {
            Close();
            var frame = new Border { Child = card, BorderThickness = new Thickness(1), Padding = new Thickness(10, 8, 10, 8) };
            frame.SetResourceReference(Border.BackgroundProperty, EnvironmentColors.ToolTipBrushKey);
            frame.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.ToolTipBorderBrushKey);
            frame.SetResourceReference(TextElement.ForegroundProperty, EnvironmentColors.ToolTipTextBrushKey);
            var popup = new Popup
            {
                Child = frame,
                PlacementTarget = view.VisualElement,
                Placement = PlacementMode.Relative,
                HorizontalOffset = view.Caret.Left - view.ViewportLeft,
                VerticalOffset = view.Caret.Bottom - view.ViewportTop + 2,
                StaysOpen = false,
                AllowsTransparency = true,
            };
            void close(object s, EventArgs e) => Close();
            view.Caret.PositionChanged += close;
            view.TextBuffer.Changed += close;
            view.Closed += close;
            popup.Closed += (s, e) =>
            {
                view.Caret.PositionChanged -= close;
                view.TextBuffer.Changed -= close;
                view.Closed -= close;
            };
            _open = popup;
            popup.IsOpen = true;
        }
    }
}
