using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Documents;
using System.Windows.Threading;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

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
        private readonly ScrollViewer _definitionScroll;
        private readonly DispatcherTimer _cardDelay = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(200) };
        private bool _showDefinition;
        private ConnectionTarget _target;
        private int _cardVersion;

        public event Action Committed;

        /// <summary>A click in the list or the card is done: the editor should take the keyboard back.</summary>
        public event Action Clicked;

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
                    Child = _definitionScroll = new ScrollViewer { Content = _definitionText, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled },
                },
            };
            _definition.SetResourceReference(TextElement.ForegroundProperty, EnvironmentColors.CommandBarTextActiveBrushKey);
            _cardDelay.Tick += (s, e) =>
            {
                _cardDelay.Stop();
                LoadCard();
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
            // Clicking a tab or a button of the card moves the focus into the popup; once the click is
            // done the editor gets it back, so typing and the arrow keys go on working.
            border.PreviewMouseUp += (s, e) => Clicked?.Invoke();
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

        /// <summary>The mouse is on the list or the card: a click there must not close it.</summary>
        public bool IsMouseOver => _popup.IsOpen && _popup.Child.IsMouseOver;

        /// <summary>The keyboard is in the list or the card (a tab or a button was clicked).</summary>
        public bool HasKeyboard => _popup.IsOpen && _popup.Child.IsKeyboardFocusWithin;

        public CompletionItem Selected => (_list.SelectedItem as Row)?.Item;

        public int Count => _list.Items.Count;

        public IEnumerable<string> Labels => _list.Items.Cast<Row>().Select(r => r.Item.Label);

        public IEnumerable<CompletionItem> Items => _list.Items.Cast<Row>().Select(r => r.Item);

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
            _cardDelay.Stop();
            _cardVersion++;
            var info = _showDefinition ? Selected?.Info : null;
            _definitionText.Text = info ?? "";
            ShowText();
            _definition.Visibility = string.IsNullOrEmpty(info) ? Visibility.Collapsed : Visibility.Visible;
            // A table, view, procedure or function of the database: the same card as when the mouse
            // rests on its name, at once when it was read lately, else once the selection settles.
            var name = CardName(Selected);
            if (name == null)
            {
                return;
            }
            var cached = ObjectScripts.Cached(_target, name);
            if (cached != null)
            {
                ShowCard(cached);
            }
            else
            {
                _cardDelay.Start();
            }
        }

        /// <summary>The schema.name whose card the item can show; null when it has none.</summary>
        private string CardName(CompletionItem item)
        {
            if (!_showDefinition || item == null || _target == null || !_target.CanQuery || !Settings.Current.ObjectCards)
            {
                return null;
            }
            switch (item.Kind)
            {
                case "table": case "view": case "procedure": case "scalar_function": case "table_function": case "synonym":
                    break;
                default:
                    return null;
            }
            // The detail reads "schema.name" (with the parameters after it for routines); objects of
            // another database have three parts and no card.
            var name = (item.Detail ?? "").Split(new[] { " (" }, StringSplitOptions.None)[0].Trim();
            return name.Count(c => c == '.') == 1 ? name : null;
        }

        private void LoadCard()
        {
            var name = CardName(Selected);
            if (name == null || !_popup.IsOpen)
            {
                return;
            }
            var version = _cardVersion;
            var target = _target;
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                ObjectDetails details = null;
                try
                {
                    await TaskScheduler.Default;
                    details = ObjectScripts.Load(target, name);
                }
                catch (Exception ex)
                {
                    Log.Verbose("card for " + name + ": " + ex.Message);
                }
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (details != null && version == _cardVersion && _popup.IsOpen)
                {
                    ShowCard(details);
                }
            }).FileAndForget("QuillSQL/CompletionCard");
        }

        private void ShowCard(ObjectDetails details)
        {
            _definitionScroll.Content = new Border { Child = ObjectScripts.Card(details), Padding = new Thickness(10, 8, 10, 8) };
            _definition.MaxWidth = 720;
            _definition.MaxHeight = 440;
            _definition.Visibility = Visibility.Visible;
        }

        private void ShowText()
        {
            _definitionScroll.Content = _definitionText;
            _definition.MaxWidth = 460;
            _definition.MaxHeight = 300;
        }

        /// <summary>Opens the list with `select` selected, or nothing selected when it is -1.</summary>
        public void Show(IList<CompletionItem> items, IntPtr window, int screenX, int screenY, bool showDefinition, int select, ConnectionTarget target = null)
        {
            _showDefinition = showDefinition;
            _target = target;
            var rows = items.Select(i => new Row(i)).ToList();
            _list.ItemsSource = rows;
            _list.SelectedIndex = Math.Min(select, rows.Count - 1);
            _list.ScrollIntoView(_list.SelectedIndex >= 0 ? _list.SelectedItem : rows.FirstOrDefault());
            UpdateDefinition();
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
            // From no selection, the arrows start at the first item.
            var index = _list.SelectedIndex < 0 ? 0 : Math.Max(0, Math.Min(_list.Items.Count - 1, _list.SelectedIndex + delta));
            _list.SelectedIndex = index;
            _list.ScrollIntoView(_list.SelectedItem);
        }

        public void Close()
        {
            _cardDelay.Stop();
            _cardVersion++;
            _popup.IsOpen = false;
        }

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
                        case "picker": return "☑";
                        case "type": return "Ty";
                        case "database": return "DB";
                        case "server": return "LS";
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
                        case "column": case "picker": return Column;
                        case "procedure": case "function": return Routine;
                        case "variable": case "global_variable": case "parameter": return Variable;
                        case "snippet": case "body": return Snippet;
                        case "join": case "join_guess": return Join;
                        case "schema": case "alias": case "database": case "server": case "type": return Name;
                        default: return Keyword;
                    }
                }
            }
        }
    }
}
