using System;
using System.ComponentModel.Composition;
using System.Windows;
using System.Windows.Input;
using System.Windows.Shapes;
using Microsoft.VisualStudio.Editor;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Text;
using Microsoft.VisualStudio.Text.Editor;
using Microsoft.VisualStudio.Threading;
using Microsoft.VisualStudio.Utilities;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>
    /// With Ctrl held, the table, view, procedure or function under the mouse is underlined like a
    /// link; Ctrl+click opens its card pinned, so it stays open to be read and explored.
    /// </summary>
    [Export(typeof(IWpfTextViewCreationListener))]
    [ContentType("text")]
    [TextViewRole(PredefinedTextViewRoles.Document)]
    internal sealed class ObjectLinkListener : IWpfTextViewCreationListener
    {
        public const string LayerName = "QuillSqlObjectLink";

        [Export(typeof(AdornmentLayerDefinition))]
        [Name(LayerName)]
        [Order(After = PredefinedAdornmentLayers.Text)]
        internal AdornmentLayerDefinition Layer = null;

        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;

        public void TextViewCreated(IWpfTextView view) => new ObjectLinks(view, Adapters);
    }

    internal sealed class ObjectLinks
    {
        private readonly IWpfTextView _view;
        private readonly IVsEditorAdaptersFactoryService _adapters;
        private readonly IAdornmentLayer _layer;
        /// <summary>The name under the mouse being looked up, and the one underlined.</summary>
        private Span? _looking, _linked;

        public ObjectLinks(IWpfTextView view, IVsEditorAdaptersFactoryService adapters)
        {
            _view = view;
            _adapters = adapters;
            _layer = view.GetAdornmentLayer(ObjectLinkListener.LayerName);
            var element = view.VisualElement;
            element.PreviewMouseMove += OnMouseMove;
            element.MouseLeave += (s, e) => Clear();
            element.PreviewMouseLeftButtonDown += OnMouseDown;
            element.PreviewKeyDown += OnKey;
            element.PreviewKeyUp += OnKey;
            element.LostKeyboardFocus += (s, e) => Clear();
            view.LayoutChanged += (s, e) =>
            {
                if (e.VerticalTranslation || e.HorizontalTranslation)
                {
                    Clear();
                }
            };
            view.TextBuffer.Changed += (s, e) => Clear();
        }

        private static bool Ctrl => (Keyboard.Modifiers & ModifierKeys.Control) == ModifierKeys.Control
            && (Keyboard.Modifiers & (ModifierKeys.Alt | ModifierKeys.Shift)) == 0;

        private static bool IsNameChar(char c) => char.IsLetterOrDigit(c) || c == '_' || c == '#' || c == '$' || c == '.' || c == '[' || c == ']';

        private bool Active()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return Settings.Current.Enabled && Settings.Current.ObjectCards && SqlBuffers.IsSql(_view.TextBuffer, _adapters);
        }

        /// <summary>The buffer position under the mouse, or null when it is not over text.</summary>
        private int? PositionAt(Point p)
        {
            var line = _view.TextViewLines?.GetTextViewLineContainingYCoordinate(p.Y + _view.ViewportTop);
            var at = line?.GetBufferPositionFromXCoordinate(p.X + _view.ViewportLeft);
            return at?.Position;
        }

        /// <summary>The whole name around `position`, such as dbo.Orders or [sales].[Order Lines] without spaces.</summary>
        private Span? NameAt(int position)
        {
            var snapshot = _view.TextSnapshot;
            var start = position;
            while (start > 0 && IsNameChar(snapshot[start - 1]))
            {
                start--;
            }
            var end = position;
            while (end < snapshot.Length && IsNameChar(snapshot[end]))
            {
                end++;
            }
            while (end > start && snapshot[end - 1] == '.')
            {
                end--;
            }
            return end > start ? new Span(start, end - start) : (Span?)null;
        }

        private void OnMouseMove(object sender, MouseEventArgs e) => Update(e.GetPosition(_view.VisualElement));

        private void OnKey(object sender, KeyEventArgs e)
        {
            if (e.Key == Key.LeftCtrl || e.Key == Key.RightCtrl)
            {
                if (e.IsDown)
                {
                    Update(Mouse.GetPosition(_view.VisualElement));
                }
                else
                {
                    Clear();
                }
            }
        }

        private void Update(Point mouse)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (!Ctrl || !_view.VisualElement.IsMouseOver || !Active())
                {
                    Clear();
                    return;
                }
                var position = PositionAt(mouse);
                var name = position == null ? null : NameAt(position.Value);
                if (name == null)
                {
                    Clear();
                    return;
                }
                if (name == _linked || name == _looking)
                {
                    return;
                }
                Clear();
                _looking = name;
                Lookup(name.Value, position.Value);
            }
            catch (Exception ex)
            {
                Log.Error("object link", ex);
            }
        }

        /// <summary>Asks the engine whether the name is an object; underlines it if so and still wanted.</summary>
        private void Lookup(Span name, int position)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var key = Commands.MetadataKey(out _);
            var snapshot = _view.TextSnapshot;
            var text = snapshot.GetText();
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var reply = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + position);
                var found = reply == null ? null : Json.Parse<ObjectAtReply>(reply)?.Result;
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (found == null || _looking != name || _view.TextSnapshot != snapshot || !Ctrl)
                {
                    return;
                }
                _looking = null;
                Underline(new SnapshotSpan(snapshot, name));
            }).FileAndForget("quillsql/objectlink");
        }

        private void Underline(SnapshotSpan span)
        {
            var geometry = _view.TextViewLines?.GetMarkerGeometry(span);
            if (geometry == null)
            {
                return;
            }
            var bounds = geometry.Bounds;
            var line = new Rectangle { Width = bounds.Width, Height = 1, IsHitTestVisible = false };
            line.SetResourceReference(Shape.FillProperty, EnvironmentColors.ControlLinkTextBrushKey);
            System.Windows.Controls.Canvas.SetLeft(line, bounds.Left);
            System.Windows.Controls.Canvas.SetTop(line, bounds.Bottom - 2);
            _layer.RemoveAllAdornments();
            _layer.AddAdornment(AdornmentPositioningBehavior.TextRelative, span, null, line, null);
            _linked = span.Span;
            _view.VisualElement.Cursor = Cursors.Hand;
        }

        private void Clear()
        {
            _looking = null;
            if (_linked != null)
            {
                _linked = null;
                _layer.RemoveAllAdornments();
                _view.VisualElement.ClearValue(FrameworkElement.CursorProperty);
            }
        }

        private void OnMouseDown(object sender, MouseButtonEventArgs e)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!Ctrl || !Active())
            {
                return;
            }
            if (_linked == null)
            {
                // Clicked before the name was underlined (Ctrl pressed without moving the mouse):
                // look it up now, and pin its card if it is an object. The click itself goes on.
                var clicked = PositionAt(e.GetPosition(_view.VisualElement));
                var name = clicked == null ? null : NameAt(clicked.Value);
                if (name != null)
                {
                    var view = _view;
                    var key = Commands.MetadataKey(out _);
                    var text = view.TextSnapshot.GetText();
                    ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
                    {
                        await TaskScheduler.Default;
                        var reply = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + clicked.Value);
                        var found = reply == null ? null : Json.Parse<ObjectAtReply>(reply)?.Result;
                        await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                        if (found != null && !view.IsClosed)
                        {
                            CardPopup.Close();
                            CardPopup.ShowPinned(view, clicked.Value);
                        }
                    }).FileAndForget("quillsql/objectlink");
                }
                return;
            }
            var position = PositionAt(e.GetPosition(_view.VisualElement));
            if (position == null || !_linked.Value.Contains(position.Value) && _linked.Value.End != position.Value)
            {
                return;
            }
            // The click opens the card instead of moving the caret or selecting a word.
            e.Handled = true;
            var at = _linked.Value.Start + (position.Value - _linked.Value.Start);
            Clear();
            try
            {
                CardPopup.Close();
                CardPopup.ShowPinned(_view, at);
            }
            catch (Exception ex)
            {
                Log.Error("opening a pinned card", ex);
            }
        }
    }
}
