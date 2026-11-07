using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>Picks several columns at once with check boxes, from the suggestions of a select list.</summary>
    internal static class ColumnPicker
    {
        /// <summary>The inserts of the columns checked, in the list's order; null when cancelled.</summary>
        public static IList<string> Pick(IList<CompletionItem> columns)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var window = Theme.Dialog(Loc.T("Choose Columns"), true);
            window.Width = 460;
            window.Height = 520;
            var layout = new DockPanel { Margin = new Thickness(14) };

            var filter = new TextBox { MinHeight = 24, VerticalContentAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 0, 8) };
            DockPanel.SetDock(filter, Dock.Top);
            layout.Children.Add(filter);

            var tools = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 6) };
            DockPanel.SetDock(tools, Dock.Top);
            layout.Children.Add(tools);

            var boxes = new List<CheckBox>();
            var list = new StackPanel();
            foreach (var c in columns)
            {
                var row = new DockPanel();
                var type = new TextBlock { Text = c.Detail ?? "", Opacity = 0.6, Margin = new Thickness(16, 0, 0, 0) };
                DockPanel.SetDock(type, Dock.Right);
                row.Children.Add(type);
                row.Children.Add(new TextBlock { Text = c.Insert, FontFamily = Theme.Mono });
                var box = new CheckBox { Content = row, Tag = c.Insert, Margin = new Thickness(0, 2, 0, 2), HorizontalContentAlignment = HorizontalAlignment.Stretch };
                boxes.Add(box);
                list.Children.Add(box);
            }
            layout.Children.Add(new ScrollViewer { Content = list, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });

            void setAll(bool on)
            {
                foreach (var b in boxes.Where(b => b.Visibility == Visibility.Visible))
                {
                    b.IsChecked = on;
                }
            }
            var all = Theme.FlatButton(Loc.T("Select all"));
            all.Click += (o, e) => setAll(true);
            var none = Theme.FlatButton(Loc.T("Clear"));
            none.Click += (o, e) => setAll(false);
            tools.Children.Add(all);
            tools.Children.Add(none);
            filter.TextChanged += (o, e) =>
            {
                foreach (var b in boxes)
                {
                    b.Visibility = ((string)b.Tag).IndexOf(filter.Text.Trim(), System.StringComparison.OrdinalIgnoreCase) >= 0 ? Visibility.Visible : Visibility.Collapsed;
                }
            };

            var ok = Theme.DialogButton(Loc.T("Insert"), isDefault: true);
            ok.Click += (o, e) => window.DialogResult = true;
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isCancel: true);
            var root = new DockPanel();
            var footer = Theme.Footer(null, ok, cancel);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(layout);
            window.Content = root;
            window.Loaded += (o, e) => Keyboard.Focus(filter);
            EditorContext.SetOwner(window);
            if (window.ShowModal() != true)
            {
                return null;
            }
            var chosen = boxes.Where(b => b.IsChecked == true).Select(b => (string)b.Tag).ToList();
            return chosen.Count == 0 ? null : chosen;
        }
    }
}
