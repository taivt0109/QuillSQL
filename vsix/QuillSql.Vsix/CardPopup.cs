using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
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
        /// <summary>Holds the card inside the open popup, so the details can replace the title.</summary>
        private static Border _host;
        /// <summary>A pinned card stays open until it is closed with its button or Esc.</summary>
        private static bool _pinned;


        public static bool IsOpen => _open?.IsOpen == true;

        public static bool IsPinned => IsOpen && _pinned;

        public static void Close()
        {
            if (_open != null)
            {
                _open.IsOpen = false;
                _open = null;
                _host = null;
                _pinned = false;
            }
        }


        /// <summary>Ctrl+click on a name: its card, pinned.</summary>
        public static void ShowPinned(IWpfTextView view, int position)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var target = ConnectionResolver.TryGetActive();
            if (target == null || !target.CanQuery)
            {
                EditorContext.Status("connect the query window first: the card is read from the database.");
                return;
            }
            Lookup(view, position, target, true, true, () => EditorContext.Status("put the caret on a table, view, procedure or function."));
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
            Lookup(view, view.Caret.Position.BufferPosition.Position, target, false, false, () => EditorContext.Status("put the caret on a table, view, procedure or function."));
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
            // A pinned card stays until it is closed; hovering does not replace it.
            if (IsPinned || (IsOpen && _openSpan.Contains(position)))
            {
                return;
            }
            Lookup(view, position, target, true, false, otherwise);
        }

        private static void Lookup(IWpfTextView view, int position, ConnectionTarget target, bool mouse, bool pinned, Action notFound)
        {
            var key = Commands.MetadataKey(out _);
            var snapshot = view.TextSnapshot;
            var text = snapshot.GetText();
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var at = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + position);
                var found = at == null ? null : Json.Parse<ObjectAtReply>(at)?.Result;
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (view.IsClosed || view.TextSnapshot != snapshot)
                {
                    return;
                }
                if (found == null)
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
                // Show the title at once; a large table or a remote server can take a while to describe.
                var fullName = found.Schema + "." + found.Name;
                var details = ObjectScripts.Cached(target, fullName);
                var host = Open(view, details != null ? ObjectScripts.Card(details) : ObjectScripts.Loading(found.Schema, found.Name, found.Kind), new Span(start, end - start), mouse, pinned);
                if (details == null)
                {
                    await TaskScheduler.Default;
                    try
                    {
                        details = ObjectScripts.Load(target, fullName);
                    }
                    catch (Exception ex)
                    {
                        Log.Error("reading object details", ex);
                    }
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    if (host == null || _host != host)
                    {
                        return;
                    }
                    if (details == null)
                    {
                        Close();
                        notFound();
                        return;
                    }
                    host.Child = ObjectScripts.Card(details);
                }
                Log.Verbose("object card: " + details.FullName + "; " + ObjectScripts.StorageLine(details));
            }).FileAndForget("quillsql/objectcard");
        }

        private static Border Open(IWpfTextView view, UIElement card, Span span, bool mouse, bool pinned)
        {
            Close();
            var line = view.TextViewLines?.GetTextViewLineContainingBufferPosition(new SnapshotPoint(view.TextSnapshot, span.Start));
            if (line == null)
            {
                return null;
            }
            var left = mouse ? line.GetCharacterBounds(new SnapshotPoint(view.TextSnapshot, span.Start)).Left : view.Caret.Left;
            // The card with its buttons in the top right corner: pin (until pinned) and close.
            var host = new Border { Child = card };
            var pinButton = CornerButton("\uE718", Loc.T("Pin: keep the card open (or Ctrl+click a name)"));
            var closeButton = CornerButton("\uE711", Loc.T("Close (Esc)"));
            var corner = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(0, -4, -6, 0) };
            corner.Children.Add(pinButton);
            corner.Children.Add(closeButton);
            var layout = new Grid();
            layout.Children.Add(host);
            layout.Children.Add(corner);
            var frame = new Border { Child = layout, BorderThickness = new Thickness(1), Padding = new Thickness(10, 8, 10, 8) };
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
                if (_pinned)
                {
                    watch.Stop();
                    return;
                }
                var onName = name.Contains(Mouse.GetPosition(view.VisualElement));
                var onCard = new Rect(0, 0, frame.ActualWidth, frame.ActualHeight).Contains(Mouse.GetPosition(frame));
                away = onName || onCard ? 0 : away + 1;
                if (away >= 2 && _open == popup)
                {
                    Close();
                }
            };
            void pin()
            {
                _pinned = true;
                watch.Stop();
                popup.StaysOpen = true;
                pinButton.Visibility = Visibility.Collapsed;
                frame.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.ControlLinkTextBrushKey);
            }
            pinButton.MouseLeftButtonUp += (s, e) => pin();
            closeButton.MouseLeftButtonUp += (s, e) => Close();
            if (pinned)
            {
                pin();
            }
            else if (mouse)
            {
                watch.Start();
            }

            // Unpinned cards close when the text moves under them; pinned ones only when closed.
            void close(object s, EventArgs e)
            {
                if (!_pinned)
                {
                    Close();
                }
            }
            void closed(object s, EventArgs e) => Close();
            void scrolled(object s, TextViewLayoutChangedEventArgs e)
            {
                if (!_pinned && (e.VerticalTranslation || e.HorizontalTranslation))
                {
                    Close();
                }
            }
            // A pinned card would float over other applications; hide it while SSMS is in the background.
            void away_(object s, EventArgs e) => frame.Visibility = Visibility.Hidden;
            void back(object s, EventArgs e) => frame.Visibility = Visibility.Visible;
            var app = Application.Current;
            view.Caret.PositionChanged += close;
            view.TextBuffer.Changed += close;
            view.Closed += closed;
            view.LayoutChanged += scrolled;
            if (app != null)
            {
                app.Deactivated += away_;
                app.Activated += back;
            }
            popup.Closed += (s, e) =>
            {
                watch.Stop();
                view.LayoutChanged -= scrolled;
                view.Caret.PositionChanged -= close;
                view.TextBuffer.Changed -= close;
                view.Closed -= closed;
                if (app != null)
                {
                    app.Deactivated -= away_;
                    app.Activated -= back;
                }
            };
            _open = popup;
            _openSpan = span;
            _host = host;
            _pinned = pinned;
            popup.IsOpen = true;
            return host;
        }

        private static TextBlock CornerButton(string glyph, string tip)
        {
            var button = new TextBlock
            {
                Text = glyph,
                FontFamily = Theme.Icons,
                FontSize = 11,
                Padding = new Thickness(5, 4, 5, 4),
                Opacity = 0.6,
                Cursor = Cursors.Hand,
                Background = Brushes.Transparent,
                ToolTip = tip,
            };
            button.MouseEnter += (s, e) => button.Opacity = 1;
            button.MouseLeave += (s, e) => button.Opacity = 0.6;
            return button;
        }
    }
}
