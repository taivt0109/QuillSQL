using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;

namespace QuillSql.Compare
{
    /// <summary>Colors of the Compare windows; SSMS replaces them with its theme's.</summary>
    public static class CompareTheme
    {
        public static Brush Background = SystemColors.WindowBrush;
        public static Brush Panel = new SolidColorBrush(Color.FromRgb(0xF3, 0xF3, 0xF3));
        public static Brush Foreground = SystemColors.WindowTextBrush;
        public static Brush Muted = Brushes.Gray;
        public static Brush Border = new SolidColorBrush(Color.FromRgb(0xD0, 0xD0, 0xD0));
        public static Brush Accent = new SolidColorBrush(Color.FromRgb(0x2B, 0x5F, 0xA6));
        public static Brush AccentText = Brushes.White;
        public static Brush Added = new SolidColorBrush(Color.FromArgb(0x40, 0x2E, 0xA0, 0x43));
        public static Brush Removed = new SolidColorBrush(Color.FromArgb(0x40, 0xD0, 0x3A, 0x2F));
        public static Brush Changed = new SolidColorBrush(Color.FromArgb(0x50, 0xE3, 0xA0, 0x08));
        public static Brush High = new SolidColorBrush(Color.FromRgb(0xC4, 0x2B, 0x1C));
        public static Brush Medium = new SolidColorBrush(Color.FromRgb(0xB8, 0x6E, 0x00));
        public static FontFamily Code = new FontFamily("Consolas");

        /// <summary>Applies the colors to a root element (and lets children inherit the foreground).</summary>
        public static void Apply(Control c)
        {
            c.Background = Background;
            c.Foreground = Foreground;
        }
    }

    /// <summary>What the window needs from the program it lives in.</summary>
    public interface ICompareHost
    {
        /// <summary>Opens SQL in an editor (SSMS: a new query window).</summary>
        void OpenScript(string sql, string title);

        /// <summary>The window that owns dialogs, or null.</summary>
        Window Owner { get; }
    }

    internal static class Ui
    {
        public static TextBlock Text(string text, double size = 12, bool bold = false, Brush brush = null, TextWrapping wrap = TextWrapping.NoWrap)
        {
            var t = new TextBlock { Text = text, FontSize = size, TextWrapping = wrap, VerticalAlignment = VerticalAlignment.Center };
            if (bold) t.FontWeight = FontWeights.SemiBold;
            if (brush != null) t.Foreground = brush;
            return t;
        }

        public static Button Button(string text, Action click, bool primary = false, double minWidth = 80)
        {
            var b = new Button { Content = text, Padding = new Thickness(12, 4, 12, 4), Margin = new Thickness(4, 0, 0, 0), MinWidth = minWidth };
            if (primary)
            {
                b.Background = CompareTheme.Accent;
                b.Foreground = CompareTheme.AccentText;
                b.FontWeight = FontWeights.SemiBold;
                b.IsDefault = false;
            }
            b.Click += (s, e) => click();
            return b;
        }

        public static StackPanel Row(params UIElement[] children)
        {
            var p = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 3, 0, 3) };
            foreach (var c in children) p.Children.Add(c);
            return p;
        }

        public static StackPanel Column(params UIElement[] children)
        {
            var p = new StackPanel { Orientation = Orientation.Vertical };
            foreach (var c in children) p.Children.Add(c);
            return p;
        }

        /// <summary>A label column and a control column.</summary>
        public static Grid Form(params Tuple<string, UIElement>[] rows)
        {
            var g = new Grid();
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(130) });
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            for (var i = 0; i < rows.Length; i++)
            {
                g.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                if (rows[i].Item1 != null)
                {
                    var label = Text(rows[i].Item1);
                    label.Margin = new Thickness(0, 4, 8, 4);
                    Grid.SetRow(label, i);
                    g.Children.Add(label);
                }
                var control = rows[i].Item2;
                if (control is FrameworkElement fe) fe.Margin = new Thickness(0, 3, 0, 3);
                Grid.SetRow(control, i);
                Grid.SetColumn(control, 1);
                g.Children.Add(control);
            }
            return g;
        }

        public static Tuple<string, UIElement> Field(string label, UIElement control) => Tuple.Create(label, control);

        public static CheckBox Check(string text, bool value, Action<bool> changed = null)
        {
            var c = new CheckBox { Content = text, IsChecked = value, Margin = new Thickness(0, 3, 12, 3), VerticalAlignment = VerticalAlignment.Center };
            if (changed != null)
            {
                c.Checked += (s, e) => changed(true);
                c.Unchecked += (s, e) => changed(false);
            }
            return c;
        }

        public static TextBox Code(bool readOnly = true)
        {
            return new TextBox
            {
                FontFamily = CompareTheme.Code,
                FontSize = 12,
                IsReadOnly = readOnly,
                AcceptsReturn = true,
                AcceptsTab = true,
                TextWrapping = TextWrapping.NoWrap,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
        }

        public static void Error(Window owner, string message)
        {
            if (owner != null) MessageBox.Show(owner, message, "QuillSQL Compare", MessageBoxButton.OK, MessageBoxImage.Warning);
            else MessageBox.Show(message, "QuillSQL Compare", MessageBoxButton.OK, MessageBoxImage.Warning);
        }

        public static bool Confirm(Window owner, string message)
        {
            var r = owner != null
                ? MessageBox.Show(owner, message, "QuillSQL Compare", MessageBoxButton.YesNo, MessageBoxImage.Question)
                : MessageBox.Show(message, "QuillSQL Compare", MessageBoxButton.YesNo, MessageBoxImage.Question);
            return r == MessageBoxResult.Yes;
        }

        /// <summary>Runs work and reports failures in a message box; returns null on failure.</summary>
        public static async Task<T> Try<T>(Window owner, Func<Task<T>> work) where T : class
        {
            try
            {
                return await work();
            }
            catch (EngineException ex)
            {
                Error(owner, ex.Message);
            }
            catch (Exception ex) when (ex is System.IO.IOException || ex is InvalidOperationException || ex is System.ComponentModel.Win32Exception)
            {
                Error(owner, ex.Message);
            }
            return null;
        }

        public static Window Dialog(Window owner, string title, UIElement content, double width, double height)
        {
            var w = new Window
            {
                Title = title,
                Content = content,
                Width = width,
                Height = height,
                WindowStartupLocation = owner != null ? WindowStartupLocation.CenterOwner : WindowStartupLocation.CenterScreen,
                ShowInTaskbar = false,
                Background = CompareTheme.Background,
                Foreground = CompareTheme.Foreground,
            };
            if (owner != null) w.Owner = owner;
            w.KeyDown += (s, e) => { if (e.Key == Key.Escape) w.Close(); };
            return w;
        }

        public static string Plural(long n, string one, string many) => n + " " + (n == 1 ? one : many);
    }

    /// <summary>Line-by-line alignment of two texts for the side-by-side view.</summary>
    internal static class LineDiff
    {
        public enum Kind { Same, Changed, OnlyLeft, OnlyRight }

        public struct Line
        {
            public string Left;
            public string Right;
            public Kind Kind;
        }

        private static string Key(string s) => System.Text.RegularExpressions.Regex.Replace(s.Trim(), @"\s+", " ");

        public static List<Line> Align(string left, string right)
        {
            var a = (left ?? "").Replace("\r\n", "\n").Split('\n');
            var b = (right ?? "").Replace("\r\n", "\n").Split('\n');
            if (left == null) a = new string[0];
            if (right == null) b = new string[0];
            var result = new List<Line>();
            // Too large for a full LCS table: compare by position.
            if ((long)a.Length * b.Length > 4_000_000)
            {
                for (var i = 0; i < Math.Max(a.Length, b.Length); i++)
                {
                    var l = i < a.Length ? a[i] : null;
                    var r = i < b.Length ? b[i] : null;
                    result.Add(new Line { Left = l, Right = r, Kind = l == null ? Kind.OnlyRight : r == null ? Kind.OnlyLeft : Key(l) == Key(r) ? Kind.Same : Kind.Changed });
                }
                return result;
            }
            var ka = Array.ConvertAll(a, Key);
            var kb = Array.ConvertAll(b, Key);
            var lcs = new int[a.Length + 1, b.Length + 1];
            for (var i = a.Length - 1; i >= 0; i--)
            {
                for (var j = b.Length - 1; j >= 0; j--)
                {
                    lcs[i, j] = ka[i] == kb[j] ? lcs[i + 1, j + 1] + 1 : Math.Max(lcs[i + 1, j], lcs[i, j + 1]);
                }
            }
            int x = 0, y = 0;
            var lefts = new List<string>();
            var rights = new List<string>();
            void flush()
            {
                // Unmatched runs are paired line by line as changes.
                for (var k = 0; k < Math.Max(lefts.Count, rights.Count); k++)
                {
                    var l = k < lefts.Count ? lefts[k] : null;
                    var r = k < rights.Count ? rights[k] : null;
                    result.Add(new Line { Left = l, Right = r, Kind = l == null ? Kind.OnlyRight : r == null ? Kind.OnlyLeft : Kind.Changed });
                }
                lefts.Clear();
                rights.Clear();
            }
            while (x < a.Length && y < b.Length)
            {
                if (ka[x] == kb[y])
                {
                    flush();
                    result.Add(new Line { Left = a[x], Right = b[y], Kind = Kind.Same });
                    x++;
                    y++;
                }
                else if (lcs[x + 1, y] >= lcs[x, y + 1])
                {
                    lefts.Add(a[x++]);
                }
                else
                {
                    rights.Add(b[y++]);
                }
            }
            while (x < a.Length) lefts.Add(a[x++]);
            while (y < b.Length) rights.Add(b[y++]);
            flush();
            return result;
        }
    }
}
