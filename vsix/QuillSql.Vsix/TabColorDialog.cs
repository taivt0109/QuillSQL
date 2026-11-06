using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Picks the tab color of one server, or one database on it. The choice is saved as a color rule
    /// whose text is "server / " or "server / database", so it outranks the rules matching a word.
    /// </summary>
    internal static class TabColorDialog
    {
        private static readonly string[][] Presets =
        {
            new[] { "Production", "#C62828", "PRODUCTION" },
            new[] { "Staging", "#EF6C00", "STAGING" },
            new[] { "UAT", "#F9A825", "UAT" },
            new[] { "Development", "#1565C0", "DEVELOPMENT" },
            new[] { "Test", "#2E7D32", "TEST" },
            new[] { "Other", "#7B1FA2", "" },
            new[] { "Gray", "#616161", "" },
        };

        /// <summary>The rule text for a server, or for one of its databases.</summary>
        public static string MatchFor(string server, string database) =>
            server + " / " + (string.IsNullOrEmpty(database) ? "" : database);

        /// <summary>Opens the dialog for the active query window's connection.</summary>
        public static void ForActiveWindow()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var description = ConnectionResolver.TryGetActive()?.Description;
            if (description == null)
            {
                EditorContext.Status("open a connected query window first.");
                return;
            }
            var at = description.IndexOf(" / ", StringComparison.Ordinal);
            var server = at < 0 ? description : description.Substring(0, at);
            var database = at < 0 ? null : description.Substring(at + 3);
            Show(server, database == "(default)" ? null : database);
        }

        /// <summary>Asks for the color of `server` (and `database`, when given) and saves it.</summary>
        public static void Show(string server, string database)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var rules = (ConnectionColors.All() ?? new ColorRule[0]).ToList();
            var serverMatch = MatchFor(server, null);
            var databaseMatch = database == null ? null : MatchFor(server, database);
            ColorRule existing(string match) => match == null ? null : rules.FirstOrDefault(r => string.Equals(r.Match, match, StringComparison.OrdinalIgnoreCase));

            var window = Theme.Dialog(Loc.T("Tab Color"), false);
            window.SizeToContent = SizeToContent.WidthAndHeight;
            var panel = new StackPanel { Margin = new Thickness(18, 16, 18, 14), Width = 470 };
            panel.Children.Add(new TextBlock { Text = Loc.T("Color the query windows of"), Opacity = 0.75 });
            panel.Children.Add(new TextBlock { Text = server + (database == null ? "" : "  /  " + database), FontWeight = FontWeights.SemiBold, FontSize = 14, Margin = new Thickness(0, 2, 0, 12), TextTrimming = TextTrimming.CharacterEllipsis });

            // Scope: one database, or every database on the server.
            RadioButton onlyDatabase = null, wholeServer = null;
            if (database != null)
            {
                onlyDatabase = new RadioButton { Content = new TextBlock { Text = Loc.T("Only database " + database) }, IsChecked = existing(databaseMatch) != null || existing(serverMatch) == null, Margin = new Thickness(0, 0, 0, 4) };
                wholeServer = new RadioButton { Content = new TextBlock { Text = Loc.T("Every database on this server") }, IsChecked = !(existing(databaseMatch) != null || existing(serverMatch) == null), Margin = new Thickness(0, 0, 0, 12) };
                panel.Children.Add(onlyDatabase);
                panel.Children.Add(wholeServer);
            }
            string matchNow() => wholeServer == null || wholeServer.IsChecked == true ? serverMatch : databaseMatch;

            // Swatches.
            var current = existing(matchNow());
            var color = new TextBox { Text = current?.Color ?? Presets[0][1], Width = 90, FontFamily = Theme.Mono, VerticalContentAlignment = VerticalAlignment.Center, MinHeight = 24 };
            var label = new TextBox { Text = current?.Label ?? Presets[0][2], Width = 220, VerticalContentAlignment = VerticalAlignment.Center, MinHeight = 24 };
            var swatches = new WrapPanel { Margin = new Thickness(0, 0, 0, 12) };
            var swatchBorders = new List<Tuple<Border, string>>();
            void highlight()
            {
                foreach (var s in swatchBorders)
                {
                    var on = string.Equals(s.Item2, color.Text.Trim(), StringComparison.OrdinalIgnoreCase);
                    s.Item1.BorderBrush = on ? (Brush)Theme.Frozen(Theme.Blue) : Brushes.Transparent;
                }
            }
            foreach (var p in Presets)
            {
                var square = new Border { Width = 46, Height = 22, CornerRadius = new CornerRadius(3), Background = Theme.BrushFromText(p[1]), HorizontalAlignment = HorizontalAlignment.Center };
                var stack = new StackPanel();
                stack.Children.Add(square);
                stack.Children.Add(new TextBlock { Text = Loc.T(p[0]), FontSize = 11, HorizontalAlignment = HorizontalAlignment.Center, Margin = new Thickness(0, 3, 0, 0) });
                var frame = new Border { Child = stack, Padding = new Thickness(4), BorderThickness = new Thickness(2), CornerRadius = new CornerRadius(4), Margin = new Thickness(0, 0, 4, 4), Cursor = System.Windows.Input.Cursors.Hand, Background = Brushes.Transparent, Width = 64 };
                var preset = p;
                frame.MouseLeftButtonUp += (s, e) =>
                {
                    color.Text = preset[1];
                    if (preset[2].Length > 0 || Presets.Any(q => q[2] == label.Text))
                    {
                        label.Text = preset[2];
                    }
                };
                swatchBorders.Add(Tuple.Create(frame, p[1]));
                swatches.Children.Add(frame);
            }
            panel.Children.Add(swatches);

            var preview = new Border { Width = 24, Height = 22, CornerRadius = new CornerRadius(3), Margin = new Thickness(8, 0, 0, 0), BorderBrush = Theme.Line, BorderThickness = new Thickness(1) };
            color.TextChanged += (s, e) =>
            {
                preview.Background = Theme.BrushFromText(color.Text);
                highlight();
            };
            preview.Background = Theme.BrushFromText(color.Text);
            highlight();
            var colorRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 8) };
            colorRow.Children.Add(new TextBlock { Text = Loc.T("Color (#RRGGBB)"), Width = 130, VerticalAlignment = VerticalAlignment.Center });
            colorRow.Children.Add(color);
            colorRow.Children.Add(preview);
            var labelRow = new StackPanel { Orientation = Orientation.Horizontal };
            labelRow.Children.Add(new TextBlock { Text = Loc.T("Label"), Width = 130, VerticalAlignment = VerticalAlignment.Center });
            labelRow.Children.Add(label);
            panel.Children.Add(colorRow);
            panel.Children.Add(labelRow);
            panel.Children.Add(new TextBlock { Text = Loc.T("The label shows in the bar above the editor. Other rules are in Options > Tab colors."), Opacity = 0.65, FontSize = 11, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 0) });

            void scopeChanged()
            {
                var rule = existing(matchNow());
                if (rule != null)
                {
                    color.Text = rule.Color;
                    label.Text = rule.Label ?? "";
                }
            }
            if (onlyDatabase != null)
            {
                onlyDatabase.Checked += (s, e) => scopeChanged();
                wholeServer.Checked += (s, e) => scopeChanged();
            }

            var save = Theme.DialogButton(Loc.T("Save"), isDefault: true);
            var remove = Theme.DialogButton(Loc.T("No color"));
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isCancel: true);
            save.Click += (s, e) =>
            {
                if (Theme.BrushFromText(color.Text) == Brushes.Transparent)
                {
                    color.Focus();
                    color.SelectAll();
                    return;
                }
                var match = matchNow();
                rules.RemoveAll(r => string.Equals(r.Match, match, StringComparison.OrdinalIgnoreCase));
                rules.Insert(0, new ColorRule { Match = match, Color = color.Text.Trim(), Label = label.Text.Trim() });
                ConnectionColors.Save(rules);
                window.DialogResult = true;
            };
            remove.Click += (s, e) =>
            {
                var match = matchNow();
                rules.RemoveAll(r => string.Equals(r.Match, match, StringComparison.OrdinalIgnoreCase));
                ConnectionColors.Save(rules);
                window.DialogResult = true;
            };
            var root = new DockPanel();
            var footer = Theme.Footer(null, save, remove, cancel);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(panel);
            window.Content = root;
            EditorContext.SetOwner(window);
            if (window.ShowModal() == true)
            {
                EditorContext.Status("tab color saved; open query windows update in a moment.");
            }
        }
    }
}
