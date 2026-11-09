using System;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Media;
using Microsoft.VisualStudio.PlatformUI;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Parameter hints: the signature of the function or procedure being called, shown above the
    /// line with the parameter at the caret in bold. Like the suggestion list it never takes focus.
    /// </summary>
    internal sealed class SignatureTip
    {
        private readonly Popup _popup;
        private readonly TextBlock _text;
        private readonly TextBlock _returns;
        private readonly Border _border;

        public SignatureTip()
        {
            _text = new TextBlock { FontFamily = Theme.Mono, FontSize = 12, TextWrapping = TextWrapping.Wrap, MaxWidth = 640 };
            _text.SetResourceReference(TextBlock.ForegroundProperty, EnvironmentColors.ToolTipTextBrushKey);
            _returns = new TextBlock { FontSize = 11, Opacity = 0.7, Margin = new Thickness(0, 2, 0, 0), TextWrapping = TextWrapping.Wrap, MaxWidth = 640 };
            _returns.SetResourceReference(TextBlock.ForegroundProperty, EnvironmentColors.ToolTipTextBrushKey);
            var stack = new StackPanel();
            stack.Children.Add(_text);
            stack.Children.Add(_returns);
            _border = new Border { BorderThickness = new Thickness(1), Padding = new Thickness(8, 4, 8, 5), Child = stack, UseLayoutRounding = true, SnapsToDevicePixels = true };
            _border.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.ToolTipBorderBrushKey);
            _border.SetResourceReference(Border.BackgroundProperty, EnvironmentColors.ToolTipBrushKey);
            TextOptions.SetTextFormattingMode(_border, TextFormattingMode.Display);
            _popup = new Popup { Child = _border, Placement = PlacementMode.Absolute, StaysOpen = true, Focusable = false, AllowsTransparency = false };
        }

        public bool IsOpen => _popup.IsOpen;

        /// <summary>The signature shown, as plain text.</summary>
        public string Text => _text.Text + (string.IsNullOrEmpty(_returns.Text) ? "" : " " + _returns.Text);

        /// <summary>Shows the signature with its bottom at `lineTop`, the top of the caret's line (physical pixels).</summary>
        public void Show(SignatureResult signature, IntPtr window, int screenX, int lineTop)
        {
            _text.Inlines.Clear();
            _text.Inlines.Add(new Run(signature.Name) { FontWeight = FontWeights.SemiBold });
            _text.Inlines.Add(new Run(" ("));
            var parameters = signature.Params ?? new string[0];
            for (var i = 0; i < parameters.Length; i++)
            {
                if (i > 0)
                {
                    _text.Inlines.Add(new Run(", "));
                }
                var run = new Run(parameters[i]);
                if (i == signature.Active)
                {
                    run.FontWeight = FontWeights.Bold;
                    run.TextDecorations = TextDecorations.Underline;
                }
                _text.Inlines.Add(run);
            }
            _text.Inlines.Add(new Run(")"));
            _returns.Text = string.IsNullOrEmpty(signature.Returns) ? "" : "→ " + signature.Returns;
            _returns.Visibility = string.IsNullOrEmpty(signature.Returns) ? Visibility.Collapsed : Visibility.Visible;

            // Popup offsets are device-independent; the editor reports physical pixels.
            var scale = GetDpiForWindow(window) / 96.0;
            if (scale <= 0)
            {
                scale = 1;
            }
            _border.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            _popup.HorizontalOffset = screenX / scale;
            _popup.VerticalOffset = Math.Max(0, lineTop / scale - _border.DesiredSize.Height - 2);
            _popup.IsOpen = true;
        }

        public void Close() => _popup.IsOpen = false;

        [DllImport("user32.dll")]
        private static extern uint GetDpiForWindow(IntPtr hwnd);
    }
}
