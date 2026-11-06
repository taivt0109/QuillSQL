using System;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using Microsoft.Win32;

namespace QuillSql.Compare
{
    /// <summary>
    /// Colors of the Compare windows. QuillCompare.exe follows the Windows light or dark mode and
    /// accent color; SSMS passes its theme's background and text instead. Fluent.xaml's styles
    /// read the `Q.*` brushes from <see cref="Resources"/>.
    /// </summary>
    public static class CompareTheme
    {
        public static Brush Background, Panel, Foreground, Muted, Border, Accent, AccentText, Added, Removed, Changed, ChangedStrong, High, Medium, Success;
        public static FontFamily Code = new FontFamily("Cascadia Mono, Consolas");
        public static FontFamily Icons = new FontFamily("Segoe Fluent Icons, Segoe MDL2 Assets");
        public static FontFamily Font = new FontFamily("Segoe UI");

        public static bool Dark { get; private set; }

        /// <summary>The standalone window shows Mica behind transparent backgrounds (Windows 11).</summary>
        public static bool Translucent { get; private set; }

        /// <summary>The brushes and, merged in, the control styles.</summary>
        public static ResourceDictionary Resources { get; } = new ResourceDictionary();

        static CompareTheme()
        {
            Resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri("pack://application:,,,/QuillSql.Compare;component/Fluent.xaml", UriKind.Absolute) });
            Use(false);
        }

        /// <summary>Light or dark as set in Windows (Settings > Personalization > Colors), with its accent.</summary>
        public static void UseWindows(bool translucent)
        {
            var dark = false;
            try
            {
                var forced = Environment.GetEnvironmentVariable("QUILLCOMPARE_THEME");
                dark = forced != null ? forced.Equals("dark", StringComparison.OrdinalIgnoreCase) : Registry.GetValue(@"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "AppsUseLightTheme", 1) is int light && light == 0;
            }
            catch (System.Security.SecurityException)
            {
            }
            Use(dark, SystemAccent(dark), translucent: translucent && Backdrop.MicaSupported);
        }

        /// <summary>The accent shade Windows uses on light or dark backgrounds, or null.</summary>
        private static Color? SystemAccent(bool dark)
        {
            try
            {
                // Seven shades from lightest to darkest, four bytes each (R, G, B, unused).
                if (Registry.GetValue(@"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent", "AccentPalette", null) is byte[] p && p.Length >= 28)
                {
                    var i = (dark ? 1 : 4) * 4;
                    return Color.FromRgb(p[i], p[i + 1], p[i + 2]);
                }
            }
            catch (System.Security.SecurityException)
            {
            }
            return null;
        }

        private static Color Hex(uint argb) => Color.FromArgb((byte)(argb >> 24), (byte)(argb >> 16), (byte)(argb >> 8), (byte)argb);

        private static Color Mix(Color a, Color b, double t) => Color.FromRgb(
            (byte)(a.R + (b.R - a.R) * t), (byte)(a.G + (b.G - a.G) * t), (byte)(a.B + (b.B - a.B) * t));

        private static double Luminance(Color c) => (0.2126 * c.R + 0.7152 * c.G + 0.0722 * c.B) / 255;

        /// <summary>
        /// Sets every color. <paramref name="background"/> and <paramref name="text"/> come from a host
        /// theme (SSMS); without them the Windows 11 palette is used.
        /// </summary>
        public static void Use(bool dark, Color? accent = null, Color? background = null, Color? text = null, bool translucent = false)
        {
            Dark = dark;
            Translucent = translucent;
            var r = Resources;
            SolidColorBrush set(string key, Color c)
            {
                var b = new SolidColorBrush(c);
                b.Freeze();
                r[key] = b;
                return b;
            }
            Color pick(uint light, uint darkColor) => Hex(dark ? darkColor : light);

            var bg = background ?? pick(0xFFF3F3F3, 0xFF202020);
            var fg = text ?? pick(0xFF1A1A1A, 0xFFFFFFFF);
            var a = accent ?? pick(0xFF005FB8, 0xFF60CDFF);
            // Surfaces are the background lifted a little, so a host theme's background still works.
            var layer = background.HasValue ? Mix(bg, Colors.White, dark ? 0.04 : 0.6) : pick(0xFFFBFBFB, 0xFF2B2B2B);

            Background = set("Q.Bg", translucent ? Colors.Transparent : bg);
            Panel = set("Q.Layer", translucent ? pick(0xB3FFFFFF, 0x4C3A3A3A) : layer);
            set("Q.CardStroke", pick(0xFFE5E5E5, 0xFF1C1C1C));
            set("Q.Control", translucent ? pick(0xB3FFFFFF, 0x0FFFFFFF) : pick(0xFFFEFEFE, 0xFF2D2D2D));
            set("Q.ControlHover", translucent ? pick(0x80F9F9F9, 0x15FFFFFF) : pick(0xFFF6F6F6, 0xFF323232));
            set("Q.ControlPressed", translucent ? pick(0x4DF9F9F9, 0x08FFFFFF) : pick(0xFFF2F2F2, 0xFF272727));
            set("Q.ControlDisabled", pick(0x4DF9F9F9, 0x0BFFFFFF));
            Border = set("Q.ControlStroke", pick(0xFFE0E0E0, 0xFF3A3A3A));
            set("Q.StrongStroke", pick(0xFF8A8A8A, 0xFF9A9A9A));
            set("Q.CheckFill", pick(0x0A000000, 0x19000000));
            set("Q.Input", translucent ? pick(0xB3FFFFFF, 0x0FFFFFFF) : pick(0xFFFFFFFF, 0xFF2D2D2D));
            set("Q.InputHover", translucent ? pick(0xFFF9F9F9, 0x15FFFFFF) : pick(0xFFFAFAFA, 0xFF323232));
            set("Q.InputFocus", pick(0xFFFFFFFF, 0xFF1F1F1F));
            Foreground = set("Q.Text", fg);
            Muted = set("Q.TextSecondary", Mix(fg, bg, dark ? 0.25 : 0.38));
            set("Q.TextDisabled", Mix(fg, bg, 0.6));
            set("Q.Subtle", pick(0x0A000000, 0x0FFFFFFF));
            set("Q.SubtlePressed", pick(0x06000000, 0x0AFFFFFF));
            set("Q.Selected", Color.FromArgb((byte)(dark ? 0x40 : 0x26), a.R, a.G, a.B));
            set("Q.Divider", pick(0xFFE5E5E5, 0xFF383838));
            Accent = set("Q.Accent", a);
            set("Q.AccentHover", Color.FromArgb(0xE6, a.R, a.G, a.B));
            set("Q.AccentPressed", Color.FromArgb(0xCC, a.R, a.G, a.B));
            AccentText = set("Q.AccentText", Luminance(a) > 0.55 ? Colors.Black : Colors.White);
            set("Q.Flyout", pick(0xFFF9F9F9, 0xFF2C2C2C));
            set("Q.FlyoutStroke", pick(0xFFDADADA, 0xFF454545));
            set("Q.ScrollThumb", pick(0xFFA0A0A0, 0xFF8B8B8B));
            Added = set("Q.Added", pick(0xFFDFF6DD, 0xFF2F3A1F));
            Removed = set("Q.Removed", pick(0xFFFDE7E9, 0xFF442726));
            Changed = set("Q.Changed", pick(0xFFFFF4CE, 0xFF433519));
            ChangedStrong = set("Q.ChangedStrong", pick(0xFFFFD86B, 0xFF7A5C14));
            High = set("Q.Critical", pick(0xFFC42B1C, 0xFFFF99A4));
            Medium = set("Q.Caution", pick(0xFF9D5D00, 0xFFFCE100));
            Success = set("Q.Success", pick(0xFF0F7B0F, 0xFF6CCB5F));

            // Controls that keep their system templates (tree views) use the system selection colors.
            r[SystemColors.HighlightBrushKey] = r["Q.Selected"];
            r[SystemColors.HighlightTextBrushKey] = r["Q.Text"];
            r[SystemColors.InactiveSelectionHighlightBrushKey] = r["Q.Subtle"];
            r[SystemColors.InactiveSelectionHighlightTextBrushKey] = r["Q.Text"];
            r[SystemColors.ControlTextBrushKey] = r["Q.Text"];
            r[SystemColors.WindowTextBrushKey] = r["Q.Text"];
            r[SystemColors.GrayTextBrushKey] = r["Q.TextDisabled"];
        }

        /// <summary>Gives an element (and so everything in it) the styles and colors.</summary>
        public static void Attach(FrameworkElement e)
        {
            if (!e.Resources.MergedDictionaries.Contains(Resources)) e.Resources.MergedDictionaries.Add(Resources);
            System.Windows.Documents.TextElement.SetFontFamily(e, Font);
            System.Windows.Documents.TextElement.SetFontSize(e, 13);
        }

        public static Style Style(object key) => (Style)Resources[key];
    }

    /// <summary>Windows 11 window frame: Mica behind the content and a title bar in the theme's mode.</summary>
    public static class Backdrop
    {
        [DllImport("dwmapi.dll")]
        private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

        [DllImport("dwmapi.dll")]
        private static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref Margins margins);

        [StructLayout(LayoutKind.Sequential)]
        private struct Margins
        {
            public int Left, Right, Top, Bottom;
        }

        private const int UseImmersiveDarkMode = 20, UseImmersiveDarkModeOld = 19, SystemBackdropType = 38, MainWindowBackdrop = 2;

        private static int Build
        {
            get
            {
                // Environment.OSVersion stops at Windows 8 for programs without a compatibility manifest.
                try
                {
                    return int.TryParse(Registry.GetValue(@"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber", "0") as string, out var b) ? b : 0;
                }
                catch (System.Security.SecurityException)
                {
                    return 0;
                }
            }
        }

        /// <summary>Windows 11 22H2 and later can draw Mica for any window.</summary>
        public static bool MicaSupported => Build >= 22621 && SystemParameters.IsGlassEnabled && RenderCapability.Tier > 0;

        /// <summary>Applies the frame when the window gets its handle.</summary>
        public static void Apply(Window w)
        {
            w.Background = CompareTheme.Translucent ? Brushes.Transparent : CompareTheme.Background;
            w.SourceInitialized += (s, e) =>
            {
                var hwnd = new WindowInteropHelper(w).Handle;
                var dark = CompareTheme.Dark ? 1 : 0;
                if (DwmSetWindowAttribute(hwnd, UseImmersiveDarkMode, ref dark, sizeof(int)) != 0)
                {
                    DwmSetWindowAttribute(hwnd, UseImmersiveDarkModeOld, ref dark, sizeof(int));
                }
                if (!CompareTheme.Translucent) return;
                var source = HwndSource.FromHwnd(hwnd);
                if (source?.CompositionTarget != null) source.CompositionTarget.BackgroundColor = Colors.Transparent;
                var margins = new Margins { Left = -1, Right = -1, Top = -1, Bottom = -1 };
                DwmExtendFrameIntoClientArea(hwnd, ref margins);
                var mica = MainWindowBackdrop;
                DwmSetWindowAttribute(hwnd, SystemBackdropType, ref mica, sizeof(int));
            };
        }
    }
}
