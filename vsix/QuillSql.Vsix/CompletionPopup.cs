using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
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
        private readonly Border _definition;
        private readonly TextBlock _definitionText;
        private bool _showDefinition;

        public event Action Committed;

        public CompletionPopup()
        {
            _list = new ListBox
            {
                Focusable = false,
                MaxHeight = 300,
                MinWidth = 300,
                MaxWidth = 560,
                BorderThickness = new Thickness(0),
                Padding = new Thickness(0, 2, 0, 2),
                FontSize = 12,
                ItemTemplate = BuildTemplate(),
                ItemContainerStyle = Theme.ListItemStyle(new Thickness(4, 2, 8, 2), TreeViewColors.SelectedItemActiveBrushKey, TreeViewColors.SelectedItemActiveTextBrushKey, EnvironmentColors.CommandBarMenuItemMouseOverBrushKey),
            };
            _list.SetResourceReference(Control.BackgroundProperty, EnvironmentColors.CommandBarMenuBackgroundGradientBrushKey);
            _list.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.CommandBarTextActiveBrushKey);
            _list.SetResourceReference(TextBlock.FontFamilyProperty, Microsoft.VisualStudio.Shell.VsFonts.EnvironmentFontFamilyKey);
            ScrollViewer.SetHorizontalScrollBarVisibility(_list, ScrollBarVisibility.Disabled);
            VirtualizingPanel.SetVirtualizationMode(_list, VirtualizationMode.Recycling);
            _list.MouseDoubleClick += (s, e) => Committed?.Invoke();

            // The definition of the selected object, beside the list.
            _definitionText = new TextBlock
            {
                FontFamily = Theme.Mono,
                FontSize = 12,
                Margin = new Thickness(10, 6, 10, 6),
                TextWrapping = TextWrapping.NoWrap,
            };
            _definitionText.SetResourceReference(TextBlock.ForegroundProperty, EnvironmentColors.CommandBarTextActiveBrushKey);
            _definition = new Border
            {
                BorderThickness = new Thickness(1, 0, 0, 0),
                MaxWidth = 460,
                MaxHeight = 300,
                Visibility = Visibility.Collapsed,
                Child = new Border
                {
                    Background = Theme.Faint,
                    Child = new ScrollViewer { Content = _definitionText, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled },
                },
            };
            _definition.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.CommandBarMenuSeparatorBrushKey);
            _definition.SetResourceReference(Border.BackgroundProperty, EnvironmentColors.CommandBarMenuBackgroundGradientBrushKey);
            _list.SelectionChanged += (s, e) => UpdateDefinition();

            var layout = new StackPanel { Orientation = Orientation.Horizontal };
            layout.Children.Add(_list);
            layout.Children.Add(_definition);
            var border = new Border { BorderThickness = new Thickness(1), Child = layout, UseLayoutRounding = true, SnapsToDevicePixels = true };
            border.SetResourceReference(Border.BorderBrushProperty, EnvironmentColors.CommandBarMenuBorderBrushKey);
            border.SetResourceReference(Border.BackgroundProperty, EnvironmentColors.CommandBarMenuBackgroundGradientBrushKey);
            TextOptions.SetTextFormattingMode(border, TextFormattingMode.Display);
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

        /// <summary>A row: colored kind badge, the name, then the detail dimmed at the right.</summary>
        private static DataTemplate BuildTemplate()
        {
            var panel = new FrameworkElementFactory(typeof(DockPanel));

            var badge = new FrameworkElementFactory(typeof(Border));
            badge.SetValue(DockPanel.DockProperty, Dock.Left);
            badge.SetValue(FrameworkElement.MinWidthProperty, 16.0);
            badge.SetValue(FrameworkElement.HeightProperty, 16.0);
            badge.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            badge.SetValue(Border.PaddingProperty, new Thickness(2, 0, 2, 0));
            badge.SetValue(FrameworkElement.MarginProperty, new Thickness(0, 0, 7, 0));
            badge.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            badge.SetBinding(Border.BackgroundProperty, new Binding(nameof(Row.GlyphBrush)));
            var glyph = new FrameworkElementFactory(typeof(TextBlock));
            glyph.SetBinding(TextBlock.TextProperty, new Binding(nameof(Row.Glyph)));
            glyph.SetValue(TextBlock.ForegroundProperty, Brushes.White);
            glyph.SetValue(TextBlock.FontSizeProperty, 9.5);
            glyph.SetValue(TextBlock.FontWeightProperty, FontWeights.Bold);
            glyph.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            glyph.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            badge.AppendChild(glyph);
            panel.AppendChild(badge);

            var detail = new FrameworkElementFactory(typeof(TextBlock));
            detail.SetValue(DockPanel.DockProperty, Dock.Right);
            detail.SetBinding(TextBlock.TextProperty, new Binding("Item.Detail"));
            detail.SetValue(FrameworkElement.MarginProperty, new Thickness(16, 0, 0, 0));
            detail.SetValue(FrameworkElement.MaxWidthProperty, 220.0);
            detail.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            detail.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            detail.SetValue(UIElement.OpacityProperty, 0.6);
            panel.AppendChild(detail);

            var label = new FrameworkElementFactory(typeof(TextBlock));
            label.SetBinding(TextBlock.TextProperty, new Binding("Item.Label"));
            label.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
            label.SetValue(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis);
            label.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            panel.AppendChild(label);

            return new DataTemplate { VisualTree = panel };
        }

        private void UpdateDefinition()
        {
            var info = _showDefinition ? Selected?.Info : null;
            _definitionText.Text = info ?? "";
            _definition.Visibility = string.IsNullOrEmpty(info) ? Visibility.Collapsed : Visibility.Visible;
        }

        public void Show(IList<CompletionItem> items, IntPtr window, int screenX, int screenY, bool showDefinition)
        {
            _showDefinition = showDefinition;
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
            private static readonly Brush Table = Theme.Frozen(Theme.Blue);
            private static readonly Brush View = Theme.Frozen(Color.FromRgb(0x1E, 0x95, 0xA3));
            private static readonly Brush Column = Theme.Frozen(Theme.Green);
            private static readonly Brush Routine = Theme.Frozen(Theme.Purple);
            private static readonly Brush Variable = Theme.Frozen(Color.FromRgb(0xD3, 0x72, 0x1C));
            private static readonly Brush Snippet = Theme.Frozen(Color.FromRgb(0xB0, 0x86, 0x10));
            private static readonly Brush Join = Theme.Frozen(Color.FromRgb(0x3A, 0x6F, 0xC4));
            private static readonly Brush Name = Theme.Frozen(Theme.Slate);
            private static readonly Brush Keyword = Theme.Frozen(Color.FromRgb(0x80, 0x80, 0x80));

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
                        case "table": case "table_function": case "cte": case "synonym": return Table;
                        case "view": return View;
                        case "column": return Column;
                        case "procedure": case "function": return Routine;
                        case "variable": case "global_variable": case "parameter": return Variable;
                        case "snippet": case "body": return Snippet;
                        case "join": case "join_guess": return Join;
                        case "schema": case "alias": return Name;
                        default: return Keyword;
                    }
                }
            }
        }
    }
}
