using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using Microsoft.VisualStudio.PlatformUI;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The suggestion list. It never takes keyboard focus: the editor keeps the caret and
    /// the command filter routes navigation keys here.
    /// </summary>
    internal sealed class CompletionPopup
    {
        private readonly Popup _popup;
        private readonly ListBox _list;

        public event Action Committed;

        public CompletionPopup()
        {
            _list = new ListBox
            {
                Focusable = false,
                MaxHeight = 280,
                MinWidth = 280,
                BorderThickness = new Thickness(0),
                FontFamily = new FontFamily("Consolas"),
                FontSize = 12,
                ItemTemplate = BuildTemplate(),
            };
            _list.SetResourceReference(Control.BackgroundProperty, EnvironmentColors.ToolWindowBackgroundBrushKey);
            _list.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            ScrollViewer.SetHorizontalScrollBarVisibility(_list, ScrollBarVisibility.Disabled);
            _list.MouseDoubleClick += (s, e) => Committed?.Invoke();

            var border = new Border { BorderThickness = new Thickness(1), Child = _list };
            border.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.ToolWindowBorderBrushKey);
            _popup = new Popup
            {
                Child = border,
                Placement = PlacementMode.Absolute,
                StaysOpen = true,
                Focusable = false,
                AllowsTransparency = false,
            };
        }

        public bool IsOpen => _popup.IsOpen;

        public CompletionItem Selected => (_list.SelectedItem as Row)?.Item;

        public int Count => _list.Items.Count;

        public IEnumerable<string> Labels => _list.Items.Cast<Row>().Select(r => r.Item.Label);

        private static DataTemplate BuildTemplate()
        {
            var panel = new FrameworkElementFactory(typeof(StackPanel));
            panel.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);

            var glyph = new FrameworkElementFactory(typeof(TextBlock));
            glyph.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(Row.Glyph)));
            glyph.SetBinding(TextBlock.ForegroundProperty, new System.Windows.Data.Binding(nameof(Row.GlyphBrush)));
            glyph.SetValue(FrameworkElement.WidthProperty, 22.0);
            glyph.SetValue(TextBlock.FontWeightProperty, FontWeights.Bold);
            panel.AppendChild(glyph);

            var label = new FrameworkElementFactory(typeof(TextBlock));
            label.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding("Item.Label"));
            panel.AppendChild(label);

            var detail = new FrameworkElementFactory(typeof(TextBlock));
            detail.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding("Item.Detail"));
            detail.SetValue(FrameworkElement.MarginProperty, new Thickness(14, 0, 0, 0));
            detail.SetValue(UIElement.OpacityProperty, 0.6);
            panel.AppendChild(detail);

            return new DataTemplate { VisualTree = panel };
        }

        public void Show(IList<CompletionItem> items, IntPtr window, int screenX, int screenY)
        {
            var rows = items.Select(i => new Row(i)).ToList();
            _list.ItemsSource = rows;
            _list.SelectedIndex = 0;
            _list.ScrollIntoView(_list.SelectedItem);
            // Popup offsets are device-independent; the editor reports physical pixels.
            var scale = GetDpiForWindow(window) / 96.0;
            if (scale <= 0)
            {
                scale = 1;
            }
            _popup.HorizontalOffset = screenX / scale;
            _popup.VerticalOffset = screenY / scale;
            _popup.IsOpen = true;
        }

        public void Move(int delta)
        {
            if (_list.Items.Count == 0)
            {
                return;
            }
            var index = Math.Max(0, Math.Min(_list.Items.Count - 1, _list.SelectedIndex + delta));
            _list.SelectedIndex = index;
            _list.ScrollIntoView(_list.SelectedItem);
        }

        public void Close() => _popup.IsOpen = false;

        [DllImport("user32.dll")]
        private static extern uint GetDpiForWindow(IntPtr hwnd);

        private sealed class Row
        {
            public Row(CompletionItem item) => Item = item;

            public CompletionItem Item { get; }

            public string Glyph
            {
                get
                {
                    switch (Item.Kind)
                    {
                        case "table": return "T";
                        case "view": return "V";
                        case "column": return "C";
                        case "procedure": return "P";
                        case "function": return "ƒ";
                        case "table_function": return "TF";
                        case "schema": return "S";
                        case "alias": return "A";
                        case "cte": return "W";
                        case "variable": case "global_variable": return "@";
                        case "synonym": return "SN";
                        case "snippet": return "{}";
                        case "join": case "join_guess": return "⋈";
                        case "body": return "…";
                        case "parameter": return "@";
                        default: return "K";
                    }
                }
            }

            public Brush GlyphBrush
            {
                get
                {
                    switch (Item.Kind)
                    {
                        case "table": case "view": case "table_function": case "cte": case "synonym": return Brushes.SteelBlue;
                        case "column": return Brushes.SeaGreen;
                        case "procedure": case "function": return Brushes.MediumPurple;
                        case "variable": case "global_variable": case "parameter": return Brushes.DarkOrange;
                        case "snippet": case "body": return Brushes.Goldenrod;
                        case "join": case "join_guess": return Brushes.SteelBlue;
                        default: return Brushes.Gray;
                    }
                }
            }
        }
    }
}
