using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;

namespace QuillSql.Compare
{
    /// <summary>What the window needs from the program it lives in.</summary>
    public interface ICompareHost
    {
        /// <summary>Opens SQL in an editor (SSMS: a new query window).</summary>
        void OpenScript(string sql, string title);

        /// <summary>The window that owns dialogs, or null.</summary>
        Window Owner { get; }
    }

    /// <summary>A host that can open a script in a query window connected to the target.</summary>
    public interface IConnectedScriptHost
    {
        /// <param name="target">The target side, with its password when it has one; null when it is not a live database.</param>
        void OpenScript(string sql, string title, JsonObject target);
    }

    /// <summary>Segoe Fluent Icons code points.</summary>
    internal static class Glyph
    {
        public const string Add = "", Open = "", Save = "", SaveAs = "", Edit = "", Play = "",
            Stop = "", Upload = "", Refresh = "", Switch = "", Forward = "", Back = "",
            Copy = "", Search = "", Settings = "", Connect = "", List = "", People = "",
            Delete = "", Warning = "", Info = "", Help = "", Chevron = "", Document = "",
            Check = "", Recent = "", Next = "", Previous = "", Code = "", Server = "",
            Up = "\uE70E", Down = "\uE70D", Filter = "\uE71C", Report = "\uE9F9", Export = "\uEDE1", Folder = "\uE8B7", View = "\uE890";
    }

    internal static class Ui
    {
        public static TextBlock Text(string text, double size = 13, bool bold = false, Brush brush = null, TextWrapping wrap = TextWrapping.NoWrap)
        {
            var t = new TextBlock { Text = text, FontSize = size, TextWrapping = wrap, VerticalAlignment = VerticalAlignment.Center };
            if (bold) t.FontWeight = FontWeights.SemiBold;
            if (brush != null) t.Foreground = brush;
            return t;
        }

        /// <summary>A page or card title.</summary>
        public static TextBlock Title(string text, double size = 20)
        {
            var t = Text(text, size, bold: true);
            t.TextTrimming = TextTrimming.CharacterEllipsis;
            return t;
        }

        public static TextBlock Icon(string glyph, double size = 16, Brush brush = null)
        {
            var t = new TextBlock { Text = glyph, FontFamily = CompareTheme.Icons, FontSize = size, VerticalAlignment = VerticalAlignment.Center };
            if (brush != null) t.Foreground = brush;
            return t;
        }

        /// <summary>An icon and a label, for buttons and navigation.</summary>
        public static StackPanel IconText(string glyph, string text, double gap = 8)
        {
            var p = new StackPanel { Orientation = Orientation.Horizontal };
            var icon = Icon(glyph, 15);
            icon.Margin = new Thickness(0, 0, text == null ? 0 : gap, 0);
            p.Children.Add(icon);
            if (text != null) p.Children.Add(Text(text));
            return p;
        }

        public static Button Button(string text, Action click, bool primary = false, double minWidth = 80, string glyph = null)
        {
            var b = new Button
            {
                Content = glyph != null ? (object)IconText(glyph, text) : text,
                Margin = new Thickness(8, 0, 0, 0),
                MinWidth = minWidth,
            };
            if (primary) b.Style = CompareTheme.Style("Q.AccentButton");
            b.Click += (s, e) => click();
            return b;
        }

        /// <summary>A command bar button: icon and label, no fill until pointed at.</summary>
        public static Button Command(string glyph, string text, Action click, string tip = null)
        {
            var b = new Button { Content = IconText(glyph, text), Style = CompareTheme.Style("Q.SubtleButton"), ToolTip = tip, Margin = new Thickness(2, 0, 2, 0) };
            b.Click += (s, e) => click();
            return b;
        }

        /// <summary>A square icon-only button.</summary>
        public static Button IconButton(string glyph, string tip, Action click, bool subtle = true)
        {
            var b = new Button { Content = Icon(glyph, 14), ToolTip = tip, Width = 36, Height = 32, Padding = new Thickness(0), MinHeight = 0 };
            System.Windows.Automation.AutomationProperties.SetName(b, tip);
            if (subtle) b.Style = CompareTheme.Style("Q.SubtleButton");
            b.Click += (s, e) => click();
            return b;
        }

        /// <summary>A command bar button that opens a menu below it.</summary>
        public static Button MenuButton(string glyph, string text, ContextMenu menu)
        {
            var content = IconText(glyph, text);
            var chevron = Icon(Glyph.Chevron, 9);
            chevron.Margin = new Thickness(8, 1, 0, 0);
            content.Children.Add(chevron);
            var b = new Button { Content = content, Style = CompareTheme.Style("Q.SubtleButton"), Margin = new Thickness(2, 0, 2, 0) };
            CompareTheme.Attach(menu);
            b.Click += (s, e) =>
            {
                menu.PlacementTarget = b;
                menu.Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom;
                menu.IsOpen = true;
            };
            return b;
        }

        public static MenuItem MenuItem(string text, Action click, string glyph = null, string hint = null)
        {
            var m = new MenuItem { Header = text, InputGestureText = hint ?? "" };
            if (glyph != null) m.Icon = Icon(glyph, 14);
            m.Click += (s, e) => click();
            return m;
        }

        /// <summary>A rounded surface that groups related content.</summary>
        public static Border Card(UIElement child, double padding = 16)
        {
            var b = new Border { Child = child, CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1), Padding = new Thickness(padding) };
            b.SetResourceReference(Border.BackgroundProperty, "Q.Layer");
            b.SetResourceReference(Border.BorderBrushProperty, "Q.CardStroke");
            return b;
        }

        /// <summary>A small rounded label: "3 different".</summary>
        public static Border Badge(string text, Brush fill, Brush ink)
        {
            return new Border { Child = Text(text, 12, brush: ink), Background = fill, CornerRadius = new CornerRadius(10), Padding = new Thickness(9, 2, 9, 3), Margin = new Thickness(0, 0, 6, 0) };
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

        /// <summary>Fields with their labels above them, as in Windows 11 forms.</summary>
        public static StackPanel Form(params Tuple<string, UIElement>[] rows)
        {
            var p = new StackPanel();
            foreach (var row in rows)
            {
                if (row.Item1 != null)
                {
                    var label = Text(row.Item1, brush: CompareTheme.Muted);
                    label.Margin = new Thickness(0, 0, 0, 4);
                    p.Children.Add(label);
                }
                if (row.Item2 is FrameworkElement fe) fe.Margin = new Thickness(0, 0, 0, 12);
                p.Children.Add(row.Item2);
            }
            return p;
        }

        public static Tuple<string, UIElement> Field(string label, UIElement control) => Tuple.Create(label, control);

        public static CheckBox Check(string text, bool value, Action<bool> changed = null)
        {
            var c = new CheckBox { Content = text, IsChecked = value, Margin = new Thickness(0, 2, 16, 2), VerticalAlignment = VerticalAlignment.Center };
            if (changed != null)
            {
                c.Checked += (s, e) => changed(true);
                c.Unchecked += (s, e) => changed(false);
            }
            return c;
        }

        /// <summary>A search or filter box with a hint shown while it is empty.</summary>
        public static TextBox SearchBox(string hint, double width) => new TextBox { Tag = hint, Width = width, VerticalAlignment = VerticalAlignment.Center };

        public static TextBox Code(bool readOnly = true)
        {
            return new TextBox
            {
                Style = CompareTheme.Style("Q.CodeBox"),
                FontFamily = CompareTheme.Code,
                FontSize = 12.5,
                IsReadOnly = readOnly,
                AcceptsReturn = true,
                AcceptsTab = true,
                TextWrapping = TextWrapping.NoWrap,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
        }

        /// <summary>A check box column that ticks with one click.</summary>
        public static DataGridTemplateColumn CheckColumn(string header, string path, double width)
        {
            var box = new FrameworkElementFactory(typeof(CheckBox));
            box.SetBinding(System.Windows.Controls.Primitives.ToggleButton.IsCheckedProperty, new System.Windows.Data.Binding(path)
            {
                Mode = System.Windows.Data.BindingMode.TwoWay,
                UpdateSourceTrigger = System.Windows.Data.UpdateSourceTrigger.PropertyChanged,
            });
            box.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            return new DataGridTemplateColumn { Header = header, Width = width, CellTemplate = new DataTemplate { VisualTree = box } };
        }

        /// <summary>Right-aligned text, for counts.</summary>
        public static Style RightAligned()
        {
            var s = new Style(typeof(TextBlock));
            s.Setters.Add(new Setter(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Right));
            return s;
        }

        public enum Tone { Info, Warning, Question }

        /// <summary>A Windows 11 style dialog: icon, message, and buttons in a footer. Returns the index of the button pressed, or -1.</summary>
        public static int Message(Window owner, string title, string message, Tone tone, params string[] buttons)
        {
            var result = -1;
            var w = Dialog(owner, title, null, 0, 0);
            w.SizeToContent = SizeToContent.WidthAndHeight;
            w.ResizeMode = ResizeMode.NoResize;
            w.MinWidth = 400;
            w.MaxWidth = 580;
            var icon = Icon(tone == Tone.Warning ? Glyph.Warning : tone == Tone.Question ? Glyph.Help : Glyph.Info, 22, tone == Tone.Warning ? CompareTheme.Medium : CompareTheme.Accent);
            icon.VerticalAlignment = VerticalAlignment.Top;
            icon.Margin = new Thickness(0, 0, 14, 0);
            var text = Text(message, wrap: TextWrapping.Wrap);
            text.MaxWidth = 460;
            text.VerticalAlignment = VerticalAlignment.Top;
            var body = new DockPanel { Margin = new Thickness(24, 24, 24, 24) };
            DockPanel.SetDock(icon, Dock.Left);
            body.Children.Add(icon);
            body.Children.Add(text);
            var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
            for (var i = 0; i < buttons.Length; i++)
            {
                var index = i;
                var b = Button(buttons[i], () => { result = index; w.Close(); }, primary: i == 0, minWidth: 100);
                if (i == 0) b.IsDefault = true;
                if (i == buttons.Length - 1) b.IsCancel = true;
                row.Children.Add(b);
            }
            w.Content = WithFooter(body, row);
            w.ShowDialog();
            return result;
        }

        /// <summary>Content above a footer band that holds the buttons.</summary>
        public static DockPanel WithFooter(UIElement content, UIElement buttons)
        {
            var footer = new Border { Child = buttons, Padding = new Thickness(24, 16, 24, 16), BorderThickness = new Thickness(0, 1, 0, 0) };
            footer.SetResourceReference(Border.BackgroundProperty, "Q.Layer");
            footer.SetResourceReference(Border.BorderBrushProperty, "Q.Divider");
            var root = new DockPanel();
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            root.Children.Add(content);
            return root;
        }

        public static void Error(Window owner, string message) => Message(owner, "QuillSQL Compare", message, Tone.Warning, "OK");

        public static bool Confirm(Window owner, string message) => Message(owner, "QuillSQL Compare", message, Tone.Question, "Yes", "No") == 0;

        /// <summary>Asks for one line of text; null when cancelled.</summary>
        public static string Prompt(Window owner, string title, string label, string value)
        {
            string result = null;
            var w = Dialog(owner, title, null, 460, 0);
            w.SizeToContent = SizeToContent.Height;
            w.ResizeMode = ResizeMode.NoResize;
            var box = new TextBox { Text = value ?? "" };
            var heading = Title(title, 18);
            heading.Margin = new Thickness(0, 0, 0, 16);
            var body = Column(heading, Form(Field(label, box)));
            body.Margin = new Thickness(24, 22, 24, 10);
            var ok = Button("OK", () => { result = box.Text; w.Close(); }, primary: true, minWidth: 100);
            ok.IsDefault = true;
            var cancel = Button("Cancel", () => w.Close(), minWidth: 100);
            cancel.IsCancel = true;
            var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
            row.Children.Add(ok);
            row.Children.Add(cancel);
            w.Content = WithFooter(body, row);
            w.Loaded += (s, e) => { box.Focus(); box.SelectAll(); };
            w.ShowDialog();
            return result;
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
                WindowStartupLocation = owner != null && owner.IsLoaded ? WindowStartupLocation.CenterOwner : WindowStartupLocation.CenterScreen,
                ShowInTaskbar = false,
                Foreground = CompareTheme.Foreground,
            };
            if (width > 0) w.Width = width;
            if (height > 0) w.Height = height;
            CompareTheme.Attach(w);
            Backdrop.Apply(w);
            if (owner != null && owner.IsLoaded) w.Owner = owner;
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

        /// <summary>
        /// Aligns the lines of two texts. Lines are equal as the comparison sees them: ignoring
        /// case unless it is case-sensitive, and ignoring white space only when it is ignored.
        /// </summary>
        public static List<Line> Align(string left, string right, bool caseSensitive = true, bool ignoreWhiteSpace = true)
        {
            string Key(string s)
            {
                var k = ignoreWhiteSpace ? System.Text.RegularExpressions.Regex.Replace(s.Trim(), @"\s+", " ") : s.TrimEnd('\r');
                return caseSensitive ? k : k.ToUpperInvariant();
            }
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
