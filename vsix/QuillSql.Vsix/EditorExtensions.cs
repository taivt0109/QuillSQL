using System;
using System.Collections.Generic;
using System.ComponentModel.Composition;
using System.IO;
using System.Linq;
using System.Runtime.Serialization;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.VisualStudio.Editor;
using Microsoft.VisualStudio.Language.Intellisense;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Text;
using Microsoft.VisualStudio.Text.Adornments;
using Microsoft.VisualStudio.Text.Editor;
using Microsoft.VisualStudio.Text.Tagging;
using Microsoft.VisualStudio.TextManager.Interop;
using Microsoft.VisualStudio.Utilities;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The SQL editor's content type differs between SSMS versions, so the editor parts below
    /// register for all text and check the buffer's language service instead.
    /// </summary>
    internal static class SqlBuffers
    {
        public static bool IsSql(ITextBuffer buffer, IVsEditorAdaptersFactoryService adapters)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return EditorContext.IsSql(adapters.GetBufferAdapter(buffer) as IVsTextLines);
        }

        /// <summary>Runs `work` after `delay` without further calls; each call restarts the wait.</summary>
        public static DispatcherTimer Debounce(TimeSpan delay, Action work)
        {
            var timer = new DispatcherTimer { Interval = delay };
            timer.Tick += (s, e) =>
            {
                timer.Stop();
                try
                {
                    work();
                }
                catch (Exception ex)
                {
                    Log.Error("editor extension", ex);
                }
            };
            return timer;
        }

        public static void Restart(this DispatcherTimer timer)
        {
            timer.Stop();
            timer.Start();
        }
    }

    [Export(typeof(IViewTaggerProvider))]
    [ContentType("text")]
    [TagType(typeof(IErrorTag))]
    [TextViewRole(PredefinedTextViewRoles.Document)]
    internal sealed class IssueTaggerProvider : IViewTaggerProvider
    {
        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;

        public ITagger<T> CreateTagger<T>(ITextView view, ITextBuffer buffer) where T : ITag
        {
            if (buffer != view.TextBuffer)
            {
                return null;
            }
            return view.Properties.GetOrCreateSingletonProperty(() => new IssueTagger(view, Adapters)) as ITagger<T>;
        }
    }

    /// <summary>Underlines code analysis issues a moment after typing stops.</summary>
    internal sealed class IssueTagger : ITagger<IErrorTag>
    {
        private readonly ITextView _view;
        private readonly IVsEditorAdaptersFactoryService _adapters;
        private readonly DispatcherTimer _timer;
        private List<Tuple<ITrackingSpan, IErrorTag>> _issues = new List<Tuple<ITrackingSpan, IErrorTag>>();
        private string _key = "";
        private int _generation;

        public event EventHandler<SnapshotSpanEventArgs> TagsChanged;

        public IssueTagger(ITextView view, IVsEditorAdaptersFactoryService adapters)
        {
            _view = view;
            _adapters = adapters;
            _timer = SqlBuffers.Debounce(TimeSpan.FromMilliseconds(900), Analyze);
            view.TextBuffer.Changed += OnChanged;
            view.GotAggregateFocus += OnChanged;
            CodeAnalysis.Toggled += Refresh;
            Settings.Changed += Refresh;
            view.Closed += (s, e) =>
            {
                _timer.Stop();
                view.TextBuffer.Changed -= OnChanged;
                view.GotAggregateFocus -= OnChanged;
                CodeAnalysis.Toggled -= Refresh;
                Settings.Changed -= Refresh;
            };
            _timer.Start();
        }

        private void OnChanged(object sender, EventArgs e) => _timer.Restart();

        private void Refresh() => _timer.Restart();

        private void Analyze()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var generation = ++_generation;
            if (!CodeAnalysis.UnderlinesOn || !SqlBuffers.IsSql(_view.TextBuffer, _adapters))
            {
                Publish(new List<Tuple<ITrackingSpan, IErrorTag>>());
                return;
            }
            if (_view.HasAggregateFocus)
            {
                // Metadata belongs to the window's connection, which is known only while it is active.
                _key = Commands.MetadataKey(out _);
            }
            var snapshot = _view.TextBuffer.CurrentSnapshot;
            var text = snapshot.GetText();
            var key = _key;
            _ = ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var issues = CodeAnalysis.Analyze(text, key);
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (issues == null || generation != _generation)
                {
                    return;
                }
                var spans = new List<Tuple<ITrackingSpan, IErrorTag>>();
                foreach (var issue in issues)
                {
                    var start = Math.Min(issue.Start, snapshot.Length);
                    var length = Math.Max(1, Math.Min(issue.End, snapshot.Length) - start);
                    if (start + length > snapshot.Length)
                    {
                        continue;
                    }
                    var type = issue.Severity == "warning" ? PredefinedErrorTypeNames.Warning : PredefinedErrorTypeNames.HintedSuggestion;
                    var tip = issue.Code + ": " + Loc.Issue(issue.Message) + (issue.Fix == null ? "" : "\n" + Loc.T("Fix available: " + Loc.T(issue.Fix.Title) + " (Tools > QuillSQL > Fix Code Issues)"));
                    spans.Add(Tuple.Create(snapshot.CreateTrackingSpan(start, length, SpanTrackingMode.EdgeExclusive), (IErrorTag)new ErrorTag(type, tip)));
                }
                Log.Verbose("underlined " + spans.Count + " issue(s): " + string.Join(",", issues.Select(i => i.Code)));
                Publish(spans);
            });
        }

        private void Publish(List<Tuple<ITrackingSpan, IErrorTag>> spans)
        {
            _issues = spans;
            var snapshot = _view.TextBuffer.CurrentSnapshot;
            TagsChanged?.Invoke(this, new SnapshotSpanEventArgs(new SnapshotSpan(snapshot, 0, snapshot.Length)));
        }

        public IEnumerable<ITagSpan<IErrorTag>> GetTags(NormalizedSnapshotSpanCollection spans)
        {
            if (spans.Count == 0)
            {
                yield break;
            }
            var snapshot = spans[0].Snapshot;
            foreach (var issue in _issues)
            {
                var span = issue.Item1.GetSpan(snapshot);
                if (spans.IntersectsWith(new NormalizedSnapshotSpanCollection(span)))
                {
                    yield return new TagSpan<IErrorTag>(span, issue.Item2);
                }
            }
        }
    }

    [Export(typeof(IViewTaggerProvider))]
    [ContentType("text")]
    [TagType(typeof(TextMarkerTag))]
    [TextViewRole(PredefinedTextViewRoles.Document)]
    internal sealed class HighlightTaggerProvider : IViewTaggerProvider
    {
        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;

        public ITagger<T> CreateTagger<T>(ITextView view, ITextBuffer buffer) where T : ITag
        {
            if (buffer != view.TextBuffer)
            {
                return null;
            }
            return view.Properties.GetOrCreateSingletonProperty(() => new HighlightTagger(view, Adapters)) as ITagger<T>;
        }
    }

    /// <summary>Highlights the other uses of the variable, alias or name under the caret.</summary>
    internal sealed class HighlightTagger : ITagger<TextMarkerTag>
    {
        private static readonly TextMarkerTag Marker = new TextMarkerTag("MarkerFormatDefinition/HighlightedReference");
        private readonly ITextView _view;
        private readonly IVsEditorAdaptersFactoryService _adapters;
        private readonly DispatcherTimer _timer;
        private List<SnapshotSpan> _spans = new List<SnapshotSpan>();
        private int _generation;

        public event EventHandler<SnapshotSpanEventArgs> TagsChanged;

        public HighlightTagger(ITextView view, IVsEditorAdaptersFactoryService adapters)
        {
            _view = view;
            _adapters = adapters;
            _timer = SqlBuffers.Debounce(TimeSpan.FromMilliseconds(400), Find);
            view.Caret.PositionChanged += OnCaret;
            view.TextBuffer.Changed += OnEdit;
            view.Closed += (s, e) =>
            {
                _timer.Stop();
                view.Caret.PositionChanged -= OnCaret;
                view.TextBuffer.Changed -= OnEdit;
            };
        }

        private void OnCaret(object sender, CaretPositionChangedEventArgs e) => _timer.Restart();

        private void OnEdit(object sender, TextContentChangedEventArgs e)
        {
            Publish(new List<SnapshotSpan>());
            _timer.Restart();
        }

        private void Find()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var generation = ++_generation;
            if (!SqlBuffers.IsSql(_view.TextBuffer, _adapters))
            {
                return;
            }
            if (!Settings.Current.Enabled || !Settings.Current.HighlightMatches)
            {
                Publish(new List<SnapshotSpan>());
                return;
            }
            var snapshot = _view.TextBuffer.CurrentSnapshot;
            var caret = _view.Caret.Position.BufferPosition.Position;
            var text = snapshot.GetText();
            _ = ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await TaskScheduler.Default;
                var reply = EngineProcess.Shared.Request("occurrences", "\"text\":" + Json.Str(text) + ",\"caret\":" + caret);
                var ranges = reply == null ? null : Json.Parse<OccurrencesReply>(reply)?.Result?.Ranges;
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                if (generation != _generation || _view.TextBuffer.CurrentSnapshot != snapshot)
                {
                    return;
                }
                var spans = new List<SnapshotSpan>();
                if (ranges != null && ranges.Length > 1)
                {
                    spans.AddRange(ranges.Where(r => r.Start + r.Length <= snapshot.Length).Select(r => new SnapshotSpan(snapshot, r.Start, r.Length)));
                    Log.Verbose("highlighted " + spans.Count + " occurrence(s)");
                }
                Publish(spans);
            });
        }

        private void Publish(List<SnapshotSpan> spans)
        {
            if (spans.Count == 0 && _spans.Count == 0)
            {
                return;
            }
            _spans = spans;
            var snapshot = _view.TextBuffer.CurrentSnapshot;
            TagsChanged?.Invoke(this, new SnapshotSpanEventArgs(new SnapshotSpan(snapshot, 0, snapshot.Length)));
        }

        public IEnumerable<ITagSpan<TextMarkerTag>> GetTags(NormalizedSnapshotSpanCollection spans)
        {
            if (spans.Count == 0)
            {
                yield break;
            }
            foreach (var span in _spans)
            {
                if (span.Snapshot == spans[0].Snapshot && spans.IntersectsWith(new NormalizedSnapshotSpanCollection(span)))
                {
                    yield return new TagSpan<TextMarkerTag>(span, Marker);
                }
            }
        }
    }

    [Export(typeof(IAsyncQuickInfoSourceProvider))]
    [Name("QuillSQL QuickInfo")]
    [ContentType("text")]
    [Order]
    internal sealed class QuickInfoSourceProvider : IAsyncQuickInfoSourceProvider
    {
        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;

        public IAsyncQuickInfoSource TryCreateQuickInfoSource(ITextBuffer buffer) =>
            buffer.Properties.GetOrCreateSingletonProperty(() => new QuickInfoSource(buffer, Adapters));
    }

    /// <summary>Describes the table, column, alias or variable under the mouse.</summary>
    internal sealed class QuickInfoSource : IAsyncQuickInfoSource
    {
        private readonly ITextBuffer _buffer;
        private readonly IVsEditorAdaptersFactoryService _adapters;

        public QuickInfoSource(ITextBuffer buffer, IVsEditorAdaptersFactoryService adapters)
        {
            _buffer = buffer;
            _adapters = adapters;
        }

        private static bool IsWordChar(char c) => char.IsLetterOrDigit(c) || c == '_' || c == '@' || c == '#' || c == '$';

        public async Task<QuickInfoItem> GetQuickInfoItemAsync(IAsyncQuickInfoSession session, CancellationToken cancellationToken)
        {
            var point = session.GetTriggerPoint(_buffer.CurrentSnapshot);
            Log.Verbose("quick info requested at " + (point?.Position ?? -1));
            if (!point.HasValue)
            {
                return null;
            }
            await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync(cancellationToken);
            if (!Settings.Current.Enabled || !SqlBuffers.IsSql(_buffer, _adapters))
            {
                return null;
            }
            // Tables, views, procedures and functions get their card from CardPopup instead.
            if (CardPopup.IsOpen)
            {
                return null;
            }
            var key = Commands.MetadataKey(out _);
            var snapshot = point.Value.Snapshot;
            var position = point.Value.Position;
            var text = snapshot.GetText();
            await TaskScheduler.Default;
            var reply = EngineProcess.Shared.Request("hover", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + position);
            var description = reply == null ? null : Json.Parse<TextReply>(reply)?.Result?.Text;
            if (string.IsNullOrEmpty(description))
            {
                return null;
            }
            var start = position;
            while (start > 0 && IsWordChar(text[start - 1]))
            {
                start--;
            }
            var end = position;
            while (end < text.Length && IsWordChar(text[end]))
            {
                end++;
            }
            var span = snapshot.CreateTrackingSpan(start, Math.Max(0, end - start), SpanTrackingMode.EdgeInclusive);
            Log.Verbose("quick info: " + description.Replace("\n", " / "));
            var lines = description.Split('\n').Select(l => (object)new ClassifiedTextElement(new ClassifiedTextRun("text", l)));
            return new QuickInfoItem(span, new ContainerElement(ContainerElementStyle.Stacked, lines));
        }

        public void Dispose() { }
    }

    /// <summary>
    /// SSMS's language service answers mouse hovers itself, so the editor's Quick Info broker is
    /// never asked; start a session for SQL views when the mouse rests on a name.
    /// </summary>
    [Export(typeof(IWpfTextViewCreationListener))]
    [ContentType("text")]
    [TextViewRole(PredefinedTextViewRoles.Document)]
    internal sealed class HoverListener : IWpfTextViewCreationListener
    {
        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;
        [Import] internal IAsyncQuickInfoBroker Broker = null;

        public void TextViewCreated(IWpfTextView view)
        {
            view.MouseHover += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                try
                {
                    if (!Settings.Current.Enabled || !SqlBuffers.IsSql(view.TextBuffer, Adapters))
                    {
                        return;
                    }
                    var point = e.TextPosition.GetPoint(view.TextBuffer, PositionAffinity.Successor);
                    Log.Verbose("mouse hover at " + (point?.Position ?? -1));
                    if (!point.HasValue)
                    {
                        return;
                    }
                    // Objects get their card in a popup of our own, since SSMS may answer the hover
                    // with its tooltip first; columns, aliases and variables use Quick Info.
                    var tracking = point.Value.Snapshot.CreateTrackingPoint(point.Value.Position, PointTrackingMode.Positive);
                    CardPopup.ShowAtMouse(view, point.Value.Position, () =>
                    {
                        if (!Broker.IsQuickInfoActive(view))
                        {
                            _ = Broker.TriggerQuickInfoAsync(view, tracking, QuickInfoSessionOptions.TrackMouse);
                        }
                    });
                }
                catch (Exception ex)
                {
                    Log.Error("mouse hover", ex);
                }
            };
        }
    }

    [DataContract]
    internal sealed class ColorRule
    {
        /// <summary>Text to find in "server / database", case-insensitively.</summary>
        [DataMember(Name = "match")] public string Match { get; set; }
        [DataMember(Name = "color")] public string Color { get; set; }
        [DataMember(Name = "label")] public string Label { get; set; }
    }

    /// <summary>Connection colors from %USERPROFILE%\.quillsql-colors.json, reloaded when the file changes.</summary>
    internal static class ConnectionColors
    {
        public static readonly string Path = System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".quillsql-colors.json");

        public static ColorRule[] All()
        {
            Find(null);
            return _rules;
        }

        public static void Save(System.Collections.Generic.IEnumerable<ColorRule> rules)
        {
            var list = rules.Where(r => !string.IsNullOrWhiteSpace(r.Match)).ToArray();
            var serializer = new System.Runtime.Serialization.Json.DataContractJsonSerializer(typeof(ColorRule[]));
            using (var stream = new MemoryStream())
            {
                serializer.WriteObject(stream, list);
                File.WriteAllBytes(Path, stream.ToArray());
            }
        }
        private static DateTime _loadedAt;
        private static ColorRule[] _rules = new ColorRule[0];

        public static ColorRule Find(string description)
        {
            try
            {
                var stamp = File.Exists(Path) ? File.GetLastWriteTimeUtc(Path) : DateTime.MinValue;
                if (stamp != _loadedAt)
                {
                    _loadedAt = stamp;
                    _rules = stamp == DateTime.MinValue ? new ColorRule[0] : Json.Parse<ColorRule[]>(File.ReadAllText(Path)) ?? new ColorRule[0];
                }
            }
            catch (Exception ex)
            {
                Log.Error("reading " + Path, ex);
                _rules = new ColorRule[0];
            }
            // The most specific rule wins: the longest text found in "server / database", so a rule
            // for one database beats one for its server, which beats one matching a word like "prod".
            return description == null ? null : _rules
                .Where(r => !string.IsNullOrEmpty(r.Match) && description.IndexOf(r.Match, StringComparison.OrdinalIgnoreCase) >= 0)
                .OrderByDescending(r => r.Match.Length)
                .FirstOrDefault();
        }
    }

    [Export(typeof(IWpfTextViewMarginProvider))]
    [Name(EnvironmentMargin.MarginName)]
    [MarginContainer(PredefinedMarginNames.Top)]
    [ContentType("text")]
    [TextViewRole(PredefinedTextViewRoles.Document)]
    internal sealed class EnvironmentMarginProvider : IWpfTextViewMarginProvider
    {
        [Import] internal IVsEditorAdaptersFactoryService Adapters = null;

        public IWpfTextViewMargin CreateMargin(IWpfTextViewHost host, IWpfTextViewMargin container) => new EnvironmentMargin(host.TextView, Adapters);
    }

    /// <summary>A colored bar naming the server, so production windows stand out.</summary>
    internal sealed class EnvironmentMargin : Border, IWpfTextViewMargin
    {
        public const string MarginName = "QuillSqlEnvironment";
        private readonly IWpfTextView _view;
        private readonly IVsEditorAdaptersFactoryService _adapters;
        private readonly TextBlock _label = new TextBlock { Margin = new Thickness(6, 1, 6, 1), FontWeight = FontWeights.SemiBold, Foreground = Brushes.White };
        private readonly DispatcherTimer _timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };

        public EnvironmentMargin(IWpfTextView view, IVsEditorAdaptersFactoryService adapters)
        {
            _view = view;
            _adapters = adapters;
            Child = _label;
            Visibility = Visibility.Collapsed;
            _timer.Tick += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                Update();
            };
            view.GotAggregateFocus += OnFocus;
            view.Closed += (s, e) =>
            {
                _timer.Stop();
                view.GotAggregateFocus -= OnFocus;
            };
            _timer.Start();
        }

        private void OnFocus(object sender, EventArgs e)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Update();
        }

        private void Update()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                if (!_view.HasAggregateFocus)
                {
                    return;
                }
                var description = SqlBuffers.IsSql(_view.TextBuffer, _adapters) ? ConnectionResolver.TryGetActive()?.Description : null;
                var rule = ConnectionColors.Find(description);
                if (rule == null)
                {
                    Visibility = Visibility.Collapsed;
                    return;
                }
                Background = new SolidColorBrush((Color)ColorConverter.ConvertFromString(rule.Color ?? "#C62828"));
                var label = string.IsNullOrEmpty(rule.Label) ? description : rule.Label + "  ·  " + description;
                if (_label.Text != label || Visibility != Visibility.Visible)
                {
                    Log.Verbose("environment bar: " + label);
                }
                _label.Text = label;
                Visibility = Visibility.Visible;
            }
            catch (Exception ex)
            {
                Log.Error("environment margin", ex);
                Visibility = Visibility.Collapsed;
            }
        }

        public FrameworkElement VisualElement => this;

        public double MarginSize => ActualHeight;

        public bool Enabled => true;

        public ITextViewMargin GetTextViewMargin(string marginName) => marginName == MarginName ? this : null;

        public void Dispose() => _timer.Stop();
    }
}
