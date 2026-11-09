using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.ComponentModelHost;
using Microsoft.VisualStudio.Editor;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Colors each query window's document tab by its connection, using the rules in
    /// %USERPROFILE%\.quillsql-colors.json. The shell has no API for tab colors, so this finds the
    /// tab elements in the window's WPF tree and sets the background of the border their template
    /// draws, which the theme otherwise repaints for the selected tab. A tab whose rule goes away
    /// gets its own background back.
    /// </summary>
    internal static class TabColorizer
    {
        private static readonly DispatcherTimer Timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        /// <summary>Each colored tab's template border and the background it had.</summary>
        private static readonly Dictionary<Control, Tuple<Border, object>> Original = new Dictionary<Control, Tuple<Border, object>>();
        private static readonly Dictionary<string, SolidColorBrush> Brushes = new Dictionary<string, SolidColorBrush>();
        private static bool _loggedShape;

        /// <summary>How many tabs are colored now, for the test hook.</summary>
        public static int ColoredTabs => Original.Count;
        /// <summary>The document view each tab was matched to through its editor, for when the tab is not shown.</summary>
        private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<object, object> Matched = new System.Runtime.CompilerServices.ConditionalWeakTable<object, object>();

        private sealed class Document
        {
            public object FrameView;
            public object DocView;
            public string Caption;
            public string Description;
            /// <summary>The query window's editor, to find the tab that holds it.</summary>
            public DependencyObject Editor;
        }
        private static int _missed;

        public static void Start()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Timer.Tick += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                try
                {
                    Update();
                }
                catch (Exception ex)
                {
                    Timer.Stop();
                    Log.Error("coloring document tabs (stopped)", ex);
                }
            };
            Timer.Start();
        }

        private static SolidColorBrush BrushFor(string color)
        {
            if (!Brushes.TryGetValue(color, out var brush))
            {
                brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(color));
                brush.Freeze();
                Brushes[color] = brush;
            }
            return brush;
        }

        /// <summary>Each open document: its tab keys (its view object, caption, editor) and its connection.</summary>
        private static List<Document> Documents()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var documents = new List<Document>();
            if (!(Package.GetGlobalService(typeof(SVsUIShell)) is IVsUIShell shell) || shell.GetDocumentWindowEnum(out var frames) != VSConstants.S_OK)
            {
                return documents;
            }
            var adapters = (Package.GetGlobalService(typeof(SComponentModel)) as IComponentModel)?.GetService<IVsEditorAdaptersFactoryService>();
            var one = new IVsWindowFrame[1];
            while (frames.Next(1, one, out var fetched) == VSConstants.S_OK && fetched == 1)
            {
                var frame = one[0];
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView);
                frame.GetProperty((int)__VSFPROPID.VSFPROPID_Caption, out var caption);
                DependencyObject editor = null;
                if (docView is IVsCodeWindow code && code.GetPrimaryView(out var textView) == VSConstants.S_OK && textView != null)
                {
                    editor = adapters?.GetWpfTextView(textView)?.VisualElement;
                }
                documents.Add(new Document
                {
                    FrameView = ConnectionResolver.Get(frame, "FrameView"),
                    DocView = docView,
                    Caption = caption as string,
                    Description = docView == null ? null : ConnectionResolver.ForDocView(docView)?.Description,
                    Editor = editor,
                });
            }
            return documents;
        }

        /// <summary>True when `element` sits inside the tab whose view is `view` (SSMS 22 names its parts differently).</summary>
        private static bool Holds(object view, DependencyObject element)
        {
            var content = ConnectionResolver.Get(view, "Content");
            for (var node = element; node != null; node = VisualTreeHelper.GetParent(node) ?? LogicalTreeHelper.GetParent(node))
            {
                if (ReferenceEquals(node, content) || (node is FrameworkElement fe && ReferenceEquals(fe.DataContext, view)))
                {
                    return true;
                }
            }
            return false;
        }

        /// <summary>The document a tab shows: by its view, its title, or the editor inside it.</summary>
        private static Document DocumentOf(Control tab, List<Document> documents)
        {
            var view = tab.DataContext;
            var title = ConnectionResolver.Get(view, "Title")?.ToString();
            var found = documents.FirstOrDefault(d => d.FrameView != null && ReferenceEquals(d.FrameView, view))
                ?? documents.FirstOrDefault(d => d.Caption != null && d.Caption == title)
                ?? documents.FirstOrDefault(d => d.Editor != null && Holds(view, d.Editor));
            if (found != null)
            {
                Matched.Remove(view);
                Matched.Add(view, found.DocView ?? found);
                return found;
            }
            // A tab in the background has its editor out of the tree: use what it was matched to.
            return Matched.TryGetValue(view, out var known) ? documents.FirstOrDefault(d => ReferenceEquals(d.DocView, known)) : null;
        }

        private static IEnumerable<Control> Tabs(DependencyObject root)
        {
            var stack = new Stack<DependencyObject>();
            stack.Push(root);
            while (stack.Count > 0)
            {
                var node = stack.Pop();
                var name = node.GetType().Name;
                if (node is Control control && name.EndsWith("TabItem", StringComparison.Ordinal) && control.DataContext != null)
                {
                    yield return control;
                    continue;
                }
                // Editors hold most of the tree and never contain document tabs.
                if (name == "WpfTextView" || name == "WpfMultiViewHost")
                {
                    continue;
                }
                var count = VisualTreeHelper.GetChildrenCount(node);
                for (var i = 0; i < count; i++)
                {
                    stack.Push(VisualTreeHelper.GetChild(node, i));
                }
            }
        }

        private static string Describe(DependencyObject node, int depth)
        {
            var name = (node as FrameworkElement)?.Name;
            var brush = node is Border border ? border.Background : node is Panel panel ? panel.Background : node is Control control ? control.Background : null;
            var text = node.GetType().Name + (string.IsNullOrEmpty(name) ? "" : "#" + name) + (brush == null ? "" : "=" + brush);
            if (depth >= 7)
            {
                return text;
            }
            var children = Enumerable.Range(0, VisualTreeHelper.GetChildrenCount(node)).Select(i => Describe(VisualTreeHelper.GetChild(node, i), depth + 1)).ToList();
            return children.Count == 0 ? text : text + "[" + string.Join(" ", children) + "]";
        }

        /// <summary>
        /// The border that paints the tab: PART_Border in SSMS 21's template, the TopCurvedBorder of
        /// SSMS 22's, else the first border
        /// with an opaque background, else the outermost one.
        /// </summary>
        private static Border Chrome(Control tab)
        {
            var borders = new List<Border>();
            var queue = new Queue<DependencyObject>();
            queue.Enqueue(tab);
            while (queue.Count > 0)
            {
                var node = queue.Dequeue();
                if (node is Border border)
                {
                    borders.Add(border);
                }
                var count = VisualTreeHelper.GetChildrenCount(node);
                for (var i = 0; i < count; i++)
                {
                    queue.Enqueue(VisualTreeHelper.GetChild(node, i));
                }
            }
            return borders.FirstOrDefault(b => b.Name == "PART_Border")
                ?? borders.FirstOrDefault(b => b.GetType().Name.EndsWith("CurvedBorder", StringComparison.Ordinal))
                ?? borders.FirstOrDefault(b => b.Background is SolidColorBrush brush && brush.Color.A == 255)
                ?? borders.FirstOrDefault();
        }

        private static void Update()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var documents = Documents();
            var windows = Application.Current?.Windows.Cast<Window>().ToList() ?? new List<Window>();
            var tabs = windows.SelectMany(w => Tabs(w)).ToList();
            if (!_loggedShape && tabs.Count > 0 && documents.Count > 0)
            {
                _loggedShape = true;
                Log.Verbose("document tabs: " + string.Join(", ", tabs.Select(t => t.GetType().Name + "/" + t.DataContext.GetType().Name))
                    + "; frame views: " + string.Join(", ", documents.Select(d => (d.FrameView?.GetType().Name ?? "null") + " '" + d.Caption + "'"))
                    + "; template: " + Describe(tabs[0], 0));
            }
            // Documents open but no tab found for a minute: say once what the window holds, so another
            // SSMS version's tab elements can be recognized.
            if (!_loggedShape && documents.Count > 0 && tabs.Count == 0 && ++_missed == 30)
            {
                _loggedShape = true;
                var names = new SortedSet<string>();
                foreach (var w in windows)
                {
                    var stack = new Stack<DependencyObject>();
                    stack.Push(w);
                    while (stack.Count > 0 && names.Count < 60)
                    {
                        var node = stack.Pop();
                        var type = node.GetType().Name;
                        if (type.IndexOf("Tab", StringComparison.OrdinalIgnoreCase) >= 0 || type.IndexOf("Document", StringComparison.OrdinalIgnoreCase) >= 0)
                        {
                            names.Add(type + "/" + ((node as FrameworkElement)?.DataContext?.GetType().Name ?? "-"));
                        }
                        if (type == "WpfTextView" || type == "WpfMultiViewHost")
                        {
                            continue;
                        }
                        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(node); i++)
                        {
                            stack.Push(VisualTreeHelper.GetChild(node, i));
                        }
                    }
                }
                Log.Write("no document tabs found in " + windows.Count + " window(s) for " + documents.Count + " document(s); elements: " + string.Join(", ", names)
                    + "; frame views: " + string.Join(", ", documents.Select(d => (d.FrameView?.GetType().Name ?? "null") + " '" + d.Caption + "'")));
            }
            var colored = new HashSet<Control>();
            foreach (var tab in tabs)
            {
                var title = ConnectionResolver.Get(tab.DataContext, "Title")?.ToString();
                var document = DocumentOf(tab, documents);
                var rule = document == null ? null : ConnectionColors.Find(document.Description);
                if (rule == null)
                {
                    continue;
                }
                if (!Original.ContainsKey(tab))
                {
                    var chrome = Chrome(tab);
                    if (chrome == null)
                    {
                        continue;
                    }
                    Original[tab] = Tuple.Create(chrome, chrome.ReadLocalValue(Border.BackgroundProperty));
                    Log.Verbose("tab colored " + (rule.Color ?? "") + ": " + title);
                }
                // A local value outranks the theme's triggers for the selected and hovered tab.
                Original[tab].Item1.Background = BrushFor(rule.Color ?? "#C62828");
                colored.Add(tab);
            }
            foreach (var tab in Original.Keys.Where(t => !colored.Contains(t)).ToList())
            {
                var chrome = Original[tab].Item1;
                var value = Original[tab].Item2;
                if (value == DependencyProperty.UnsetValue)
                {
                    chrome.ClearValue(Border.BackgroundProperty);
                }
                else
                {
                    chrome.SetValue(Border.BackgroundProperty, value);
                }
                Original.Remove(tab);
            }
        }
    }
}
