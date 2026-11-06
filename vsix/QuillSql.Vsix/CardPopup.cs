using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Threading;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Text;
using Microsoft.VisualStudio.Text.Editor;
using Microsoft.VisualStudio.Threading;
using Span = Microsoft.VisualStudio.Text.Span;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The object card, in a popup of its own: SSMS closes Quick Info sessions started by a shortcut
    /// and answers mouse hovers with its own tooltip, so neither can carry the card. Opened from the
    /// keyboard it sits at the caret; opened by the mouse it sits under the name and closes when the
    /// mouse leaves both. It also closes with Esc, a click elsewhere, an edit or a caret move.
    /// </summary>
    internal static class CardPopup
    {
        private static Popup _open;
        private static Span _openSpan;

        public static bool IsOpen => _open?.IsOpen == true;

        public static void Close()
        {
            if (_open != null)
            {
                _open.IsOpen = false;
                _open = null;
            }
        }

        private static bool IsWordChar(char c) => char.IsLetterOrDigit(c) || c == '_' || c == '#' || c == '$';

        /// <summary>Reads the object at the caret and shows its card; false when there is no query window.</summary>
        public static bool Show(IWpfTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (view == null || EditorContext.Active() == null)
            {
                return false;
            }
            var target = ConnectionResolver.TryGetActive();
            if (target == null || !target.CanQuery)
            {
                EditorContext.Status("open a query window connected with Windows or SQL Server authentication first.");
                return true;
            }
            Lookup(view, view.Caret.Position.BufferPosition.Position, target, false, () => EditorContext.Status("put the caret on a table, view, procedure or function."));
            return true;
        }

        /// <summary>
        /// Shows the card of the object under the mouse at `position`; calls `otherwise` when the
        /// name there is not a table, view, procedure or function.
        /// </summary>
        public static void ShowAtMouse(IWpfTextView view, int position, Action otherwise)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var target = ConnectionResolver.TryGetActive();
            if (!Settings.Current.ObjectCards || target == null || !target.CanQuery)
            {
                otherwise();
                return;
            }
            if (IsOpen && _openSpan.Contains(position))
            {
                return;
            }
            Lookup(view, position, target, true, otherwise);
        }

        private static void Lookup(IWpfTextView view, int position, ConnectionTarget target, bool mouse, Action notFound)
        {
            var key = Commands.MetadataKey(out _);
            var snapshot = view.TextSnapshot;
            var text = snapshot.GetText();
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var at = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + position);
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
                if (view.IsClosed || view.TextSnapshot != snapshot)
                {
                    return;
                }
                if (details == null)
                {
                    notFound();
                    return;
                }
                var start = position;
                while (start > 0 && IsWordChar(text[start - 1]))
                {
                    start--;
                }
                var end = position;
                while (end < text.Length && IsWordChar(text[end]))
                {
                    end++;
                }
                Log.Verbose("object card: " + details.FullName + "; " + ObjectScripts.StorageLine(details));
                Open(view, ObjectScripts.Card(details), new Span(start, end - start), mouse);
            }).FileAndForget("quillsql/objectcard");
        }

        private static void Open(IWpfTextView view, UIElement card, Span span, bool mouse)
        {
            Close();
            var line = view.TextViewLines?.GetTextViewLineContainingBufferPosition(new SnapshotPoint(view.TextSnapshot, span.Start));
            if (line == null)
            {
                return;
            }
            var left = mouse ? line.GetCharacterBounds(new SnapshotPoint(view.TextSnapshot, span.Start)).Left : view.Caret.Left;
            var frame = new Border { Child = card, BorderThickness = new Thickness(1), Padding = new Thickness(10, 8, 10, 8) };
            frame.SetResourceReference(Border.BackgroundProperty, EnvironmentColors.ToolTipBrushKey);
            frame.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.ToolTipBorderBrushKey);
            frame.SetResourceReference(TextElement.ForegroundProperty, EnvironmentColors.ToolTipTextBrushKey);
            var popup = new Popup
            {
                Child = frame,
                PlacementTarget = view.VisualElement,
                Placement = PlacementMode.Relative,
                HorizontalOffset = left - view.ViewportLeft,
                VerticalOffset = line.Bottom - view.ViewportTop + 2,
                StaysOpen = false,
                AllowsTransparency = true,
            };

            // Opened by the mouse: close once the mouse has been away from both the name and the
            // card for a moment. The open popup holds the mouse, so its position is polled rather
            // than followed through the editor's mouse events.
            var name = new Rect(left - view.ViewportLeft, line.Top - view.ViewportTop,
                Math.Max(1, line.GetCharacterBounds(new SnapshotPoint(view.TextSnapshot, Math.Max(span.Start, span.End - 1))).Right - left), line.Height + 4);
            var away = 0;
            var watch = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(200) };
            watch.Tick += (s, e) =>
            {
                var onName = name.Contains(Mouse.GetPosition(view.VisualElement));
                var onCard = new Rect(0, 0, frame.ActualWidth, frame.ActualHeight).Contains(Mouse.GetPosition(frame));
                away = onName || onCard ? 0 : away + 1;
                if (away >= 2 && _open == popup)
                {
                    Close();
                }
            };
            if (mouse)
            {
                watch.Start();
            }

            void close(object s, EventArgs e) => Close();
            void scrolled(object s, TextViewLayoutChangedEventArgs e)
            {
                if (e.VerticalTranslation || e.HorizontalTranslation)
                {
                    Close();
                }
            }
            view.Caret.PositionChanged += close;
            view.TextBuffer.Changed += close;
            view.Closed += close;
            view.LayoutChanged += scrolled;
            popup.Closed += (s, e) =>
            {
                watch.Stop();
                view.LayoutChanged -= scrolled;
                view.Caret.PositionChanged -= close;
                view.TextBuffer.Changed -= close;
                view.Closed -= close;
            };
            _open = popup;
            _openSpan = span;
            popup.IsOpen = true;
        }
    }
}
