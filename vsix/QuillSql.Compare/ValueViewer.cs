using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace QuillSql.Compare
{
    /// <summary>
    /// "Value details": one column's value in the source and in the target, side by side: as text
    /// with the differing lines marked, as bytes (hex, ANSI, Unicode, UTF-8) for binary values, and
    /// as pictures when the bytes are an image.
    /// </summary>
    internal static class ValueViewer
    {
        public static void Show(Window owner, string title, string sourceName, string source, bool hasSource, string targetName, string target, bool hasTarget)
        {
            var w = Ui.Dialog(owner, "Value details", null, 1000, 640);
            var tabs = new TabControl();
            var showSpace = Ui.Check("Show white space", false);
            var count = Ui.Text("", brush: CompareTheme.Muted);
            var left = Box();
            var right = Box();
            var diffs = new List<int>();
            var at = -1;
            void fillText()
            {
                var lines = LineDiff.Align(Visible(hasSource ? source : null, showSpace.IsChecked == true), Visible(hasTarget ? target : null, showSpace.IsChecked == true), true, false);
                Fill(left, lines, true);
                Fill(right, lines, false);
                diffs.Clear();
                for (var i = 0; i < lines.Count; i++)
                {
                    if (lines[i].Kind != LineDiff.Kind.Same && (i == 0 || lines[i - 1].Kind == LineDiff.Kind.Same)) diffs.Add(i);
                }
                at = -1;
                count.Text = hasSource && hasTarget ? (source == target ? "The values are identical." : Ui.Plural(diffs.Count, "difference", "differences")) : "";
            }
            void go(int step)
            {
                if (diffs.Count == 0) return;
                at = Math.Max(0, Math.Min(diffs.Count - 1, at + step));
                left.ScrollToVerticalOffset(Math.Max(0, (diffs[at] - 3) * 18.0));
                right.ScrollToVerticalOffset(Math.Max(0, (diffs[at] - 3) * 18.0));
                count.Text = (at + 1) + " of " + diffs.Count;
            }
            showSpace.Checked += (s, e) => fillText();
            showSpace.Unchecked += (s, e) => fillText();
            fillText();
            var tools = Ui.Row(Ui.IconButton(Glyph.Up, "Previous difference", () => go(-1)), Ui.IconButton(Glyph.Down, "Next difference", () => go(1)), new Border { Width = 8 }, count, new Border { Width = 16 }, showSpace);
            tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Document, "Text"), Content = Sides(sourceName, Describe(source, hasSource), left, targetName, Describe(target, hasTarget), right, tools) });

            // Binary values come from the engine as 0x… hex.
            var b1 = Bytes(hasSource ? source : null);
            var b2 = Bytes(hasTarget ? target : null);
            if (b1 != null || b2 != null)
            {
                var view = new ComboBox { ItemsSource = new[] { "Hexadecimal", "ANSI", "Unicode", "UTF-8" }, SelectedIndex = 0, Width = 160 };
                var bl = Box();
                var br = Box();
                void fillBytes()
                {
                    var lines = LineDiff.Align(AsText(b1, view.SelectedIndex), AsText(b2, view.SelectedIndex), true, false);
                    Fill(bl, lines, true);
                    Fill(br, lines, false);
                }
                view.SelectionChanged += (s, e) => fillBytes();
                fillBytes();
                var label = Ui.Text("Show as", brush: CompareTheme.Muted);
                label.Margin = new Thickness(0, 0, 8, 0);
                tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.Code, "Binary"), Content = Sides(sourceName, b1 == null ? "" : Ui.Plural(b1.Length, "byte", "bytes"), bl, targetName, b2 == null ? "" : Ui.Plural(b2.Length, "byte", "bytes"), br, Ui.Row(label, view)) });
                var i1 = Picture(b1);
                var i2 = Picture(b2);
                if (i1 != null || i2 != null)
                {
                    UIElement pic(BitmapSource img) => img == null ? (UIElement)Ui.Text("Not an image", brush: CompareTheme.Muted) : new ScrollViewer { HorizontalScrollBarVisibility = ScrollBarVisibility.Auto, Content = new Image { Source = img, Stretch = Stretch.None } };
                    var grid = new Grid();
                    grid.ColumnDefinitions.Add(new ColumnDefinition());
                    grid.ColumnDefinitions.Add(new ColumnDefinition());
                    var a = Ui.Column(Ui.Text(sourceName + (i1 == null ? "" : " · " + i1.PixelWidth + " × " + i1.PixelHeight), bold: true), pic(i1));
                    var b = Ui.Column(Ui.Text(targetName + (i2 == null ? "" : " · " + i2.PixelWidth + " × " + i2.PixelHeight), bold: true), pic(i2));
                    a.Margin = b.Margin = new Thickness(12);
                    Grid.SetColumn(b, 1);
                    grid.Children.Add(a);
                    grid.Children.Add(b);
                    tabs.Items.Add(new TabItem { Header = Ui.IconText(Glyph.View, "Image"), Content = grid });
                }
            }

            var heading = Ui.Title(title, 16);
            var body = new DockPanel { Margin = new Thickness(24, 20, 24, 16) };
            heading.Margin = new Thickness(0, 0, 0, 12);
            DockPanel.SetDock(heading, Dock.Top);
            body.Children.Add(heading);
            body.Children.Add(Ui.Card(tabs, 0));
            var copy1 = Ui.Button("Copy " + sourceName, () => Clipboard.SetText(source ?? ""), glyph: Glyph.Copy);
            var copy2 = Ui.Button("Copy " + targetName, () => Clipboard.SetText(target ?? ""), glyph: Glyph.Copy);
            copy1.IsEnabled = hasSource && source != null;
            copy2.IsEnabled = hasTarget && target != null;
            var close = Ui.Button("Close", () => w.Close(), primary: true);
            close.IsCancel = true;
            var buttons = Ui.Row(copy1, copy2, close);
            buttons.HorizontalAlignment = HorizontalAlignment.Right;
            w.Content = Ui.WithFooter(body, buttons);
            w.ShowDialog();
        }

        private static string Describe(string value, bool has) => !has ? "no row" : value == null || value == "NULL" ? "NULL" : Ui.Plural(value.Length, "character", "characters");

        /// <summary>Spaces as ·, tabs as →, line ends as ¶, when white space is shown.</summary>
        private static string Visible(string text, bool show) => text == null || !show ? text : text.Replace(" ", "·").Replace("\t", "→").Replace("\r\n", "¶\n").Replace("\n", "¶\n");

        public static byte[] Bytes(string value)
        {
            if (value == null || value.Length < 2 || !value.StartsWith("0x", StringComparison.OrdinalIgnoreCase) || value.Length % 2 != 0) return null;
            var bytes = new byte[(value.Length - 2) / 2];
            for (var i = 0; i < bytes.Length; i++)
            {
                if (!byte.TryParse(value.Substring(2 + i * 2, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out bytes[i])) return null;
            }
            return bytes;
        }

        /// <summary>16 bytes a line in hex, or the bytes decoded as text.</summary>
        public static string AsText(byte[] bytes, int view)
        {
            if (bytes == null) return null;
            switch (view)
            {
                case 1: return Encoding.Default.GetString(bytes);
                case 2: return Encoding.Unicode.GetString(bytes);
                case 3: return Encoding.UTF8.GetString(bytes);
            }
            var sb = new StringBuilder();
            for (var i = 0; i < bytes.Length; i += 16)
            {
                if (i > 0) sb.Append('\n');
                sb.Append(i.ToString("X8")).Append("  ");
                for (var j = i; j < Math.Min(i + 16, bytes.Length); j++) sb.Append(bytes[j].ToString("X2")).Append(' ');
            }
            return sb.ToString();
        }

        /// <summary>The bytes as a picture when they are a PNG, JPEG, GIF, BMP or TIFF image.</summary>
        public static BitmapSource Picture(byte[] bytes)
        {
            if (bytes == null || bytes.Length < 8) return null;
            bool starts(params byte[] sig) => bytes.Take(sig.Length).SequenceEqual(sig);
            if (!(starts(0x89, 0x50, 0x4E, 0x47) || starts(0xFF, 0xD8, 0xFF) || starts(0x47, 0x49, 0x46, 0x38) || starts(0x42, 0x4D) || starts(0x49, 0x49, 0x2A, 0x00) || starts(0x4D, 0x4D, 0x00, 0x2A))) return null;
            try
            {
                var image = new BitmapImage();
                image.BeginInit();
                image.CacheOption = BitmapCacheOption.OnLoad;
                image.StreamSource = new MemoryStream(bytes);
                image.EndInit();
                image.Freeze();
                return image;
            }
            catch (Exception ex) when (ex is NotSupportedException || ex is IOException || ex is ArgumentException || ex is InvalidOperationException)
            {
                return null;
            }
        }

        private static RichTextBox Box() => new RichTextBox
        {
            IsReadOnly = true,
            FontFamily = CompareTheme.Code,
            FontSize = 12.5,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        };

        private static Grid Sides(string leftName, string leftNote, RichTextBox left, string rightName, string rightNote, RichTextBox right, UIElement tools)
        {
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition());
            grid.ColumnDefinitions.Add(new ColumnDefinition());
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition());
            if (tools is FrameworkElement fe) fe.Margin = new Thickness(12, 8, 12, 0);
            Grid.SetColumnSpan(tools, 2);
            grid.Children.Add(tools);
            UIElement head(string name, string note)
            {
                var t = new TextBlock { Margin = new Thickness(16, 8, 16, 4) };
                t.Inlines.Add(new Run(name) { FontWeight = FontWeights.SemiBold });
                t.Inlines.Add(new Run("  " + note) { Foreground = CompareTheme.Muted });
                return t;
            }
            var lh = head(leftName, leftNote);
            var rh = head(rightName, rightNote);
            Grid.SetRow(lh, 1);
            Grid.SetRow(rh, 1);
            Grid.SetColumn(rh, 1);
            Grid.SetRow(left, 2);
            Grid.SetRow(right, 2);
            Grid.SetColumn(right, 1);
            grid.Children.Add(lh);
            grid.Children.Add(rh);
            grid.Children.Add(left);
            grid.Children.Add(right);
            var busy = false;
            void link(RichTextBox from, RichTextBox to) => from.AddHandler(ScrollViewer.ScrollChangedEvent, new ScrollChangedEventHandler((s, e) =>
            {
                if (busy) return;
                busy = true;
                to.ScrollToVerticalOffset(e.VerticalOffset);
                to.ScrollToHorizontalOffset(e.HorizontalOffset);
                busy = false;
            }));
            link(left, right);
            link(right, left);
            return grid;
        }

        private static void Fill(RichTextBox box, List<LineDiff.Line> lines, bool left)
        {
            var doc = new FlowDocument { PageWidth = 6000, FontFamily = CompareTheme.Code, FontSize = 12.5, PagePadding = new Thickness(12, 4, 12, 4) };
            var p = new Paragraph { Margin = new Thickness(0), LineHeight = 18, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
            var missing = (Brush)CompareTheme.Resources["Q.Subtle"];
            foreach (var line in lines)
            {
                var text = (left ? line.Left : line.Right) ?? "";
                Brush back = line.Kind == LineDiff.Kind.Changed ? CompareTheme.Changed
                    : line.Kind == LineDiff.Kind.OnlyLeft ? (left ? CompareTheme.Added : missing)
                    : line.Kind == LineDiff.Kind.OnlyRight ? (left ? missing : CompareTheme.Removed) : null;
                if (line.Kind == LineDiff.Kind.Changed)
                {
                    InlineDiff.Compare(line.Left, line.Right, out var a, out var b);
                    foreach (var piece in left ? a : b) p.Inlines.Add(new Run(piece.Text) { Background = piece.Different ? CompareTheme.ChangedStrong : CompareTheme.Changed });
                    p.Inlines.Add(new Run("\n") { Background = back });
                    continue;
                }
                var run = new Run(text + "\n");
                if (back != null) run.Background = back;
                p.Inlines.Add(run);
            }
            doc.Blocks.Add(p);
            box.Document = doc;
        }
    }
}
