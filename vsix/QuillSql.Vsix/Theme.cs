using System;
using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>Shared styling that follows the Visual Studio color theme (Light, Blue and Dark).</summary>
    internal static class Theme
    {
        public static readonly FontFamily Mono = new FontFamily("Consolas");
        public static readonly FontFamily Icons = new FontFamily("Segoe Fluent Icons, Segoe MDL2 Assets");

        public const string WarningGlyph = "";
        public const string InfoGlyph = "";
        public const string SearchGlyph = "";
        public const string CopyGlyph = "";
        public const string OpenGlyph = "";

        // Grey overlays read on light and dark backgrounds alike.
        public static readonly Brush Faint = Frozen(Color.FromArgb(0x10, 0x80, 0x80, 0x80));
        public static readonly Brush Subtle = Frozen(Color.FromArgb(0x24, 0x80, 0x80, 0x80));
        public static readonly Brush Strong = Frozen(Color.FromArgb(0x3C, 0x80, 0x80, 0x80));
        public static readonly Brush Line = Frozen(Color.FromArgb(0x48, 0x80, 0x80, 0x80));

        public static readonly Color Blue = Color.FromRgb(0x2B, 0x7B, 0xD6);
        public static readonly Color Amber = Color.FromRgb(0xD9, 0x8E, 0x04);
        public static readonly Color Green = Color.FromRgb(0x3D, 0x9A, 0x50);
        public static readonly Color Purple = Color.FromRgb(0x8A, 0x5C, 0xC7);
        public static readonly Color Slate = Color.FromRgb(0x6E, 0x76, 0x81);

        private static ControlTemplate _flatButton;

        public static SolidColorBrush Frozen(Color color)
        {
            var brush = new SolidColorBrush(color);
            brush.Freeze();
            return brush;
        }

        public static Color Tint(Color color, byte alpha) => Color.FromArgb(alpha, color.R, color.G, color.B);

        /// <summary>A dialog window with the theme's colors, font and control styles.</summary>
        public static DialogWindow Dialog(string title, bool resizable)
        {
            var window = new DialogWindow
            {
                Title = title,
                HasHelpButton = false,
                HasMinimizeButton = false,
                HasMaximizeButton = resizable,
                ResizeMode = resizable ? ResizeMode.CanResize : ResizeMode.NoResize,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
                UseLayoutRounding = true,
            };
            ThemedDialogStyleLoader.SetUseDefaultThemedDialogStyles(window, true);
            window.SetResourceReference(Control.BackgroundProperty, ThemedDialogColors.WindowPanelBrushKey);
            window.SetResourceReference(Control.ForegroundProperty, ThemedDialogColors.WindowPanelTextBrushKey);
            Font(window);
            return window;
        }

        /// <summary>The environment font, with crisp text.</summary>
        public static void Font(FrameworkElement element)
        {
            element.SetResourceReference(TextElement.FontFamilyProperty, VsFonts.EnvironmentFontFamilyKey);
            element.SetResourceReference(TextElement.FontSizeProperty, VsFonts.EnvironmentFontSizeKey);
            TextOptions.SetTextFormattingMode(element, TextFormattingMode.Display);
        }

        /// <summary>A dialog button: at least 86 wide and 24 high.</summary>
        public static Button DialogButton(string text, bool isDefault = false, bool isCancel = false) => new Button
        {
            Content = text,
            IsDefault = isDefault,
            IsCancel = isCancel,
            MinWidth = 86,
            MinHeight = 24,
            Padding = new Thickness(12, 2, 12, 2),
        };

        /// <summary>The bar at the bottom of a dialog: optional content on the left, buttons on the right.</summary>
        public static Border Footer(UIElement left, params Button[] buttons)
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
            for (var i = 0; i < buttons.Length; i++)
            {
                buttons[i].Margin = new Thickness(i == 0 ? 0 : 8, 0, 0, 0);
                row.Children.Add(buttons[i]);
            }
            var dock = new DockPanel { LastChildFill = true };
            DockPanel.SetDock(row, Dock.Right);
            dock.Children.Add(row);
            if (left != null)
            {
                dock.Children.Add(left);
            }
            var bar = new Border { Child = dock, Padding = new Thickness(16, 12, 16, 12), BorderThickness = new Thickness(0, 1, 0, 0) };
            bar.SetResourceReference(Border.BackgroundProperty, ThemedDialogColors.WizardFooterBrushKey);
            bar.SetResourceReference(Border.BorderBrushProperty, ThemedDialogColors.SectionDividerBrushKey);
            return bar;
        }

        public static TextBlock Icon(string glyph, double size, Brush brush = null)
        {
            var icon = new TextBlock { Text = glyph, FontFamily = Icons, FontSize = size, VerticalAlignment = VerticalAlignment.Center };
            if (brush != null)
            {
                icon.Foreground = brush;
            }
            return icon;
        }

        public static TextBlock Dim(string text, double opacity = 0.7) => new TextBlock { Text = text ?? "", Opacity = opacity, TextWrapping = TextWrapping.Wrap };

        /// <summary>White text on a solid rounded color: readable in every theme.</summary>
        public static Border Badge(string text, Color color, double fontSize = 10) => new Border
        {
            Background = Frozen(color),
            CornerRadius = new CornerRadius(3),
            Padding = new Thickness(5, 0, 5, 1),
            VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock { Text = text, Foreground = Brushes.White, FontSize = fontSize, FontWeight = FontWeights.SemiBold },
        };

        /// <summary>A rounded tinted box around a short piece of content.</summary>
        public static Border Pill(UIElement content, Color color) => new Border
        {
            Background = Frozen(Tint(color, 0x22)),
            BorderBrush = Frozen(Tint(color, 0x70)),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(3),
            Padding = new Thickness(6, 2, 8, 2),
            HorizontalAlignment = HorizontalAlignment.Left,
            Child = content,
        };

        /// <summary>A key or key combination drawn as a keyboard key.</summary>
        public static Border Keycap(string keys) => new Border
        {
            Background = Faint,
            BorderBrush = Line,
            BorderThickness = new Thickness(1, 1, 1, 2),
            CornerRadius = new CornerRadius(3),
            Padding = new Thickness(6, 0, 6, 1),
            Margin = new Thickness(0, 1, 4, 1),
            VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock { Text = keys, FontSize = 11 },
        };

        /// <summary>A small borderless button for cards and toolbars; it keeps the surrounding text color.</summary>
        public static Button FlatButton(string text, string glyph = null)
        {
            if (_flatButton == null)
            {
                var border = new FrameworkElementFactory(typeof(Border));
                border.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
                border.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
                border.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
                border.SetValue(Border.BorderBrushProperty, new TemplateBindingExtension(Control.BorderBrushProperty));
                border.SetValue(Border.BorderThicknessProperty, new TemplateBindingExtension(Control.BorderThicknessProperty));
                var content = new FrameworkElementFactory(typeof(ContentPresenter));
                content.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
                border.AppendChild(content);
                _flatButton = new ControlTemplate(typeof(Button)) { VisualTree = border };
                var hover = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
                hover.Setters.Add(new Setter(Control.BackgroundProperty, Subtle));
                var pressed = new Trigger { Property = ButtonBase.IsPressedProperty, Value = true };
                pressed.Setters.Add(new Setter(Control.BackgroundProperty, Strong));
                _flatButton.Triggers.Add(hover);
                _flatButton.Triggers.Add(pressed);
                _flatButton.Seal();
            }
            var panel = new StackPanel { Orientation = Orientation.Horizontal };
            if (glyph != null)
            {
                var icon = Icon(glyph, 11);
                icon.Margin = new Thickness(0, 0, 6, 0);
                panel.Children.Add(icon);
            }
            panel.Children.Add(new TextBlock { Text = text, VerticalAlignment = VerticalAlignment.Center });
            return new Button
            {
                OverridesDefaultStyle = true,
                Template = _flatButton,
                Content = panel,
                Padding = new Thickness(8, 3, 8, 3),
                Background = Faint,
                BorderBrush = Line,
                BorderThickness = new Thickness(1),
                Cursor = Cursors.Hand,
                FocusVisualStyle = null,
            };
        }

        /// <summary>
        /// List rows with padding and the theme's selection colors. The selection keeps its
        /// active colors when the list has no keyboard focus.
        /// </summary>
        public static Style ListItemStyle(Thickness padding, object selectedKey, object selectedTextKey, object hoverKey)
        {
            var border = new FrameworkElementFactory(typeof(Border));
            border.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
            border.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
            border.AppendChild(new FrameworkElementFactory(typeof(ContentPresenter)));
            var style = new Style(typeof(ListBoxItem));
            style.Setters.Add(new Setter(Control.TemplateProperty, new ControlTemplate(typeof(ListBoxItem)) { VisualTree = border }));
            style.Setters.Add(new Setter(Control.PaddingProperty, padding));
            style.Setters.Add(new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch));
            style.Setters.Add(new Setter(Control.BackgroundProperty, Brushes.Transparent));
            style.Setters.Add(new Setter(FrameworkElement.FocusVisualStyleProperty, null));
            if (hoverKey != null)
            {
                var hover = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
                hover.Setters.Add(new Setter(Control.BackgroundProperty, new DynamicResourceExtension(hoverKey)));
                style.Triggers.Add(hover);
            }
            var selected = new Trigger { Property = ListBoxItem.IsSelectedProperty, Value = true };
            selected.Setters.Add(new Setter(Control.BackgroundProperty, new DynamicResourceExtension(selectedKey)));
            selected.Setters.Add(new Setter(Control.ForegroundProperty, new DynamicResourceExtension(selectedTextKey)));
            style.Triggers.Add(selected);
            return style;
        }

        /// <summary>A list box in dialog colors, with roomy rows.</summary>
        public static ListBox List(Thickness padding)
        {
            var list = new ListBox
            {
                ItemContainerStyle = ListItemStyle(padding, ThemedDialogColors.SelectedItemActiveBrushKey, ThemedDialogColors.SelectedItemActiveTextBrushKey, ThemedDialogColors.ListItemMouseOverBrushKey),
                BorderThickness = new Thickness(1),
            };
            list.SetResourceReference(Control.BackgroundProperty, ThemedDialogColors.ListBoxBrushKey);
            list.SetResourceReference(Control.ForegroundProperty, ThemedDialogColors.ListBoxTextBrushKey);
            list.SetResourceReference(Control.BorderBrushProperty, ThemedDialogColors.ListBoxBorderBrushKey);
            ScrollViewer.SetHorizontalScrollBarVisibility(list, ScrollBarVisibility.Disabled);
            return list;
        }

        /// <summary>Dialog colors for a data grid: themed header, grid lines and selection.</summary>
        public static void Grid(DataGrid grid)
        {
            grid.SetResourceReference(Control.BackgroundProperty, ThemedDialogColors.ListBoxBrushKey);
            grid.SetResourceReference(Control.ForegroundProperty, ThemedDialogColors.ListBoxTextBrushKey);
            grid.SetResourceReference(Control.BorderBrushProperty, ThemedDialogColors.ListBoxBorderBrushKey);
            grid.SetResourceReference(DataGrid.HorizontalGridLinesBrushProperty, ThemedDialogColors.GridLineBrushKey);
            grid.SetResourceReference(DataGrid.VerticalGridLinesBrushProperty, ThemedDialogColors.GridLineBrushKey);
            grid.RowBackground = Brushes.Transparent;
            grid.AlternatingRowBackground = Faint;
            grid.GridLinesVisibility = DataGridGridLinesVisibility.Horizontal;
            grid.RowHeight = 26;

            var header = new Style(typeof(System.Windows.Controls.Primitives.DataGridColumnHeader));
            header.Setters.Add(new Setter(Control.BackgroundProperty, new DynamicResourceExtension(ThemedDialogColors.GridHeadingBackgroundBrushKey)));
            header.Setters.Add(new Setter(Control.ForegroundProperty, new DynamicResourceExtension(ThemedDialogColors.GridHeadingTextBrushKey)));
            header.Setters.Add(new Setter(Control.BorderBrushProperty, new DynamicResourceExtension(ThemedDialogColors.GridLineBrushKey)));
            header.Setters.Add(new Setter(Control.BorderThicknessProperty, new Thickness(0, 0, 1, 1)));
            header.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(8, 5, 8, 5)));
            header.Setters.Add(new Setter(Control.FontWeightProperty, FontWeights.SemiBold));
            grid.ColumnHeaderStyle = header;

            var cell = new Style(typeof(DataGridCell));
            cell.Setters.Add(new Setter(Control.BorderThicknessProperty, new Thickness(0)));
            cell.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(6, 0, 6, 0)));
            cell.Setters.Add(new Setter(FrameworkElement.FocusVisualStyleProperty, null));
            cell.Setters.Add(new Setter(Control.VerticalContentAlignmentProperty, VerticalAlignment.Center));
            var selected = new Trigger { Property = DataGridCell.IsSelectedProperty, Value = true };
            selected.Setters.Add(new Setter(Control.BackgroundProperty, new DynamicResourceExtension(ThemedDialogColors.SelectedItemActiveBrushKey)));
            selected.Setters.Add(new Setter(Control.ForegroundProperty, new DynamicResourceExtension(ThemedDialogColors.SelectedItemActiveTextBrushKey)));
            cell.Triggers.Add(selected);
            grid.CellStyle = cell;
        }

        /// <summary>Wraps a text box with a search glyph and a hint shown while it is empty.</summary>
        public static Grid SearchBox(TextBox box, string hint)
        {
            box.Padding = new Thickness(24, 3, 4, 3);
            box.VerticalContentAlignment = VerticalAlignment.Center;
            var glyph = Icon(SearchGlyph, 12);
            glyph.Margin = new Thickness(8, 0, 0, 0);
            glyph.Opacity = 0.6;
            glyph.HorizontalAlignment = HorizontalAlignment.Left;
            glyph.IsHitTestVisible = false;
            var hintText = new TextBlock { Text = hint, Margin = new Thickness(27, 0, 0, 0), Opacity = 0.55, VerticalAlignment = VerticalAlignment.Center, IsHitTestVisible = false };
            box.TextChanged += (s, e) => hintText.Visibility = string.IsNullOrEmpty(box.Text) ? Visibility.Visible : Visibility.Collapsed;
            var grid = new Grid();
            grid.Children.Add(box);
            grid.Children.Add(glyph);
            grid.Children.Add(hintText);
            return grid;
        }

        /// <summary>A thin horizontal or vertical divider in the theme's separator color.</summary>
        public static Border Divider(bool vertical = false)
        {
            var line = new Border { Width = vertical ? 1 : double.NaN, Height = vertical ? double.NaN : 1 };
            line.SetResourceReference(Border.BackgroundProperty, ThemedDialogColors.SectionDividerBrushKey);
            return line;
        }

        /// <summary>A "#RRGGBB" text as a brush; transparent when it is not a color.</summary>
        public static Brush BrushFromText(string text)
        {
            try
            {
                if (!string.IsNullOrWhiteSpace(text) && ColorConverter.ConvertFromString(text.Trim()) is Color color)
                {
                    return Frozen(color);
                }
            }
            catch (FormatException)
            {
                // Not a color yet.
            }
            return Brushes.Transparent;
        }
    }

    /// <summary>A one-way binding converter from a function.</summary>
    internal sealed class FuncConverter : IValueConverter
    {
        private readonly Func<object, object> _convert;

        public FuncConverter(Func<object, object> convert) => _convert = convert;

        public object Convert(object value, Type targetType, object parameter, CultureInfo culture) => _convert(value);

        public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) => throw new NotSupportedException();
    }
}
