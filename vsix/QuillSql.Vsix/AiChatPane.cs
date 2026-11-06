using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.VisualStudio.PlatformUI;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>What a chat message carries besides the question.</summary>
    [Flags]
    internal enum ChatAttach
    {
        None = 0,
        Query = 1,
        Schema = 2,
        EstimatedPlan = 4,
        ActualPlan = 8,
        QueryStore = 16,
    }

    /// <summary>The AI chat tool window, docked beside the query windows.</summary>
    [Guid("6f1d3c2a-8b47-4e95-a0c3-71d9e5b2f480")]
    public sealed class AiChatPane : ToolWindowPane
    {
        public AiChatPane() : base(null)
        {
            Caption = "QuillSQL AI Chat";
            Content = new ChatView();
        }

        internal ChatView View => (ChatView)Content;

        /// <summary>Shows the chat, then runs `then` with it.</summary>
        internal static void Open(AsyncPackage package, Action<ChatView> then = null)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            _ = package.JoinableTaskFactory.RunAsync(async () =>
            {
                try
                {
                    var pane = await package.ShowToolWindowAsync(typeof(AiChatPane), 0, true, package.DisposalToken) as AiChatPane;
                    await package.JoinableTaskFactory.SwitchToMainThreadAsync();
                    if (pane != null)
                    {
                        then?.Invoke(pane.View);
                        pane.View.FocusInput();
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("opening the AI chat", ex);
                }
            });
        }

        /// <summary>Starts a new topic with a question and the context it needs.</summary>
        internal static void Start(AsyncPackage package, string question, ChatAttach attach)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Open(package, view => view.NewTopic(question, attach));
        }
    }

    /// <summary>The context of one message, read from the query window and the database.</summary>
    internal sealed class ChatContext
    {
        public readonly List<string> Labels = new List<string>();
        public Func<string> Build = () => "";

        private static int Lines(string text) => text.Split('\n').Length;

        /// <summary>
        /// Takes what is needed from the editor (UI thread). Returns null with `problem` set when the
        /// context cannot be read, or with `problem` null when the user cancelled.
        /// </summary>
        public static ChatContext Prepare(ChatAttach attach, out string problem)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            problem = null;
            var context = new ChatContext();
            if (attach == ChatAttach.None)
            {
                return context;
            }
            var editor = EditorContext.Active();
            var connection = ConnectionResolver.TryGetActive();
            var key = Commands.MetadataKey(out _);
            var needsEditor = (attach & (ChatAttach.Query | ChatAttach.Schema | ChatAttach.EstimatedPlan | ChatAttach.ActualPlan)) != 0;
            var needsConnection = (attach & (ChatAttach.EstimatedPlan | ChatAttach.ActualPlan | ChatAttach.QueryStore)) != 0;
            if (needsEditor && editor == null)
            {
                problem = "Open a query window first: the query is read from it.";
                return null;
            }
            if (needsConnection && (connection == null || !connection.CanQuery))
            {
                problem = "Connect the query window with Windows or SQL Server authentication first: the plan and Query Store are read from the database.";
                return null;
            }
            var selected = editor != null && editor.HasSelection;
            var sql = editor == null ? null : selected ? editor.Text.Substring(editor.SelectionStart, editor.SelectionEnd - editor.SelectionStart) : editor.Text;
            var description = connection?.Description;
            string schema = null;
            PlanTarget target = null;
            var parts = new List<string>();
            if (description != null)
            {
                parts.Add("Server and database: " + description);
            }
            if ((attach & ChatAttach.Query) != 0)
            {
                context.Labels.Add(Loc.T((selected ? "Selection" : "Query") + " (" + Lines(sql) + " lines)"));
                parts.Add((selected ? "The selected SQL:" : "The script:") + "\n```sql\n" + sql.Trim() + "\n```");
            }
            if ((attach & ChatAttach.Schema) != 0)
            {
                schema = AiAssistant.Context(editor, key);
                context.Labels.Add(Loc.T("Table definitions"));
                parts.Add("Tables and columns it uses:\n" + schema);
            }
            var actual = (attach & ChatAttach.ActualPlan) != 0;
            if ((attach & (ChatAttach.EstimatedPlan | ChatAttach.ActualPlan)) != 0)
            {
                target = AiAssistant.Target(key, sql);
                if (actual)
                {
                    if (target.WriteReason != null)
                    {
                        problem = "The actual plan needs the query to run, and it was not run because " + target.WriteReason + ". Attach the estimated plan instead.";
                        return null;
                    }
                    var run = EditorContext.Confirm("Analyze Actual Plan with AI",
                        Loc.T("Run the query on " + connection.Description + " to measure its actual plan?") + "\n\n"
                        + Loc.T("The results are discarded. The script only reads, but it uses the server like any query."), "Run");
                    if (!run)
                    {
                        return null;
                    }
                }
                context.Labels.Add(Loc.T(actual ? "Actual plan and indexes" : "Estimated plan and indexes"));
            }
            if ((attach & ChatAttach.QueryStore) != 0)
            {
                context.Labels.Add(Loc.T("Query Store: top 10 of 7 days"));
            }
            context.Build = () =>
            {
                var all = new List<string>(parts);
                if (target != null)
                {
                    var errors = new List<string>();
                    var indexes = PlanCapture.Indexes(connection, target.Objects ?? new string[0]);
                    var plans = PlanCapture.Plans(connection, target.Batches ?? new string[0], actual, errors);
                    all.Add("Tables, row counts and indexes:\n" + indexes);
                    all.Add((actual ? "Actual" : "Estimated") + " execution plan:\n" + AiAssistant.Summarize(plans, 150)
                        + (errors.Count == 0 ? "" : "\nErrors while getting the plan:\n" + string.Join("\n", errors)));
                }
                if ((attach & ChatAttach.QueryStore) != 0)
                {
                    var queries = PlanCapture.QueryStore(connection, 7, 10);
                    if (queries.Count == 0)
                    {
                        all.Add("Query Store has no queries from the last 7 days (it may be turned off for this database).");
                    }
                    else
                    {
                        var store = new StringBuilder("Most expensive queries of the last 7 days (Query Store):\n");
                        for (var i = 0; i < queries.Count; i++)
                        {
                            store.Append('\n').Append(i + 1).Append(". ").Append(PlanCapture.Describe(queries[i])).Append('\n');
                            if (i < 3 && queries[i].WorstPlanXml != null)
                            {
                                store.Append("Slowest plan of this query:\n").Append(AiAssistant.Summarize(new[] { queries[i].WorstPlanXml }, 40)).Append('\n');
                            }
                        }
                        var used = AiAssistant.Target(key, string.Join("\n;\n", queries.Select(q => q.Text)));
                        store.Append("\nTables, row counts and indexes:\n").Append(PlanCapture.Indexes(connection, used.Objects ?? new string[0]));
                        all.Add(store.ToString());
                    }
                }
                return string.Join("\n\n", all);
            };
            return context;
        }
    }

    /// <summary>
    /// The chat: a header with the topic and connection, the conversation, and a composer that
    /// chooses what to send along with the question.
    /// </summary>
    internal sealed class ChatView : Border
    {
        private const string ChatGlyph = "\uE8BD";
        private const string AddGlyph = "\uE710";
        private const string HistoryGlyph = "\uE81C";
        private const string SendGlyph = "\uE724";
        private const string StopGlyph = "\uE71A";
        private const string AttachGlyph = "\uE723";
        private const string CheckGlyph = "\uE73E";
        private const string InsertGlyph = "\uE77F";
        private const string RetryGlyph = "\uE72C";
        private const string DeleteGlyph = "\uE74D";
        private const string PlanGlyph = "\uE9D9";
        private const string ExplainGlyph = "\uE897";
        private const string ReviewGlyph = "\uE73E";
        private const string ClockGlyph = "\uE916";

        private readonly TextBlock _title = new TextBlock { FontWeight = FontWeights.SemiBold, FontSize = 13, TextTrimming = TextTrimming.CharacterEllipsis };
        private readonly StackPanel _connection = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 3, 0, 0) };
        private readonly StackPanel _messages = new StackPanel { Margin = new Thickness(14, 12, 14, 12) };
        private readonly ScrollViewer _scroll;
        private readonly TextBox _input = new TextBox
        {
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            MinHeight = 58,
            MaxHeight = 180,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            Padding = new Thickness(8, 6, 40, 6),
            BorderThickness = new Thickness(1),
        };
        private readonly Button _send;
        private readonly TextBlock _sendIcon;
        private readonly Dictionary<ChatAttach, ToggleButton> _chips = new Dictionary<ChatAttach, ToggleButton>();
        private ChatThread _thread;
        private CancellationTokenSource _busy;
        private WrapPanel _followUps;

        public ChatView()
        {
            SetResourceReference(BackgroundProperty, EnvironmentColors.ToolWindowBackgroundBrushKey);
            SetResourceReference(TextElement.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            ThemedDialogStyleLoader.SetUseDefaultThemedDialogStyles(this, true);
            Theme.Font(this);

            // Header: the topic, the connection, and the conversation commands.
            var newChat = Theme.FlatButton(Loc.T("New chat"), AddGlyph);
            newChat.ToolTip = Loc.T("Start a new topic. The current one stays in History.");
            newChat.Click += (s, e) => NewTopic(null, ChatAttach.None);
            var history = Theme.FlatButton(Loc.T("History"), HistoryGlyph);
            history.ToolTip = Loc.T("Open an earlier conversation and continue it.");
            history.Margin = new Thickness(6, 0, 0, 0);
            history.Click += (s, e) => ShowHistory(history);
            var commands = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Top };
            commands.Children.Add(newChat);
            commands.Children.Add(history);
            var heading = new StackPanel();
            heading.Children.Add(_title);
            heading.Children.Add(_connection);
            var header = new DockPanel { Margin = new Thickness(14, 10, 14, 10) };
            DockPanel.SetDock(commands, Dock.Right);
            header.Children.Add(commands);
            header.Children.Add(heading);
            var headerBar = new Border { Child = header, BorderThickness = new Thickness(0, 0, 0, 1), BorderBrush = Theme.Line };

            _scroll = new ScrollViewer { Content = _messages, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };

            // Composer: what to send along, the question, and Send.
            var attachRow = new WrapPanel { Margin = new Thickness(0, 0, 0, 6) };
            var attachLabel = new TextBlock { Text = Loc.T("Send with it:"), Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 6, 4) };
            attachRow.Children.Add(attachLabel);
            AddChip(attachRow, ChatAttach.Query, "Query", "The query in the editor, or only the selected part.");
            AddChip(attachRow, ChatAttach.Schema, "Table definitions", "Columns and types of the tables the query uses.");
            AddChip(attachRow, ChatAttach.EstimatedPlan, "Estimated plan", "The estimated execution plan, with row counts and indexes. Nothing is run.");
            AddChip(attachRow, ChatAttach.ActualPlan, "Actual plan", "Runs the query once (read-only queries only, after asking) and sends the measured plan.");
            AddChip(attachRow, ChatAttach.QueryStore, "Slow queries", "The 10 most expensive queries of the last 7 days from Query Store.");
            _input.SetResourceReference(Control.BackgroundProperty, EnvironmentColors.ComboBoxBackgroundBrushKey);
            _input.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ComboBoxTextBrushKey);
            _input.SetResourceReference(Control.BorderBrushProperty, EnvironmentColors.ComboBoxBorderBrushKey);
            _input.SetResourceReference(TextBoxBase.CaretBrushProperty, EnvironmentColors.ComboBoxTextBrushKey);
            _input.PreviewKeyDown += OnInputKey;
            var hint = new TextBlock
            {
                Text = Loc.T("Ask about the query, its plan or indexes..."),
                Opacity = 0.5,
                Margin = new Thickness(11, 7, 44, 0),
                IsHitTestVisible = false,
                TextWrapping = TextWrapping.Wrap,
            };
            _input.TextChanged += (s, e) => hint.Visibility = _input.Text.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
            _sendIcon = Theme.Icon(SendGlyph, 13, Brushes.White);
            _send = AccentButton(_sendIcon);
            _send.HorizontalAlignment = HorizontalAlignment.Right;
            _send.VerticalAlignment = VerticalAlignment.Bottom;
            _send.Margin = new Thickness(0, 0, 6, 6);
            _send.Click += (s, e) =>
            {
                if (_busy != null)
                {
                    _busy.Cancel();
                }
                else
                {
                    SendInput();
                }
            };
            var box = new Grid();
            box.Children.Add(_input);
            box.Children.Add(hint);
            box.Children.Add(_send);
            var footer = new DockPanel { Margin = new Thickness(0, 6, 0, 0) };
            var safety = new TextBlock { Text = Loc.T("AI only reads what you send; it never runs anything on the database."), Opacity = 0.55, FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis };
            var keys = new TextBlock { Text = Loc.T("Enter to send · Shift+Enter for a new line"), Opacity = 0.55, FontSize = 11, Margin = new Thickness(12, 0, 0, 0) };
            DockPanel.SetDock(keys, Dock.Right);
            footer.Children.Add(keys);
            footer.Children.Add(safety);
            var composer = new StackPanel { Margin = new Thickness(14, 10, 14, 10) };
            composer.Children.Add(attachRow);
            composer.Children.Add(box);
            composer.Children.Add(footer);
            var composerBar = new Border { Child = composer, BorderThickness = new Thickness(0, 1, 0, 0), BorderBrush = Theme.Line };

            var root = new DockPanel();
            DockPanel.SetDock(headerBar, Dock.Top);
            DockPanel.SetDock(composerBar, Dock.Bottom);
            root.Children.Add(headerBar);
            root.Children.Add(composerBar);
            root.Children.Add(_scroll);
            Child = root;

            IsVisibleChanged += (s, e) => Refresh();
            GotKeyboardFocus += (s, e) => Refresh();
            NewTopic(null, ChatAttach.None);
        }

        public void FocusInput() => Dispatcher.BeginInvoke(new Action(() => _input.Focus()), DispatcherPriority.Input);

        // ---- Topics ----

        /// <summary>Clears the conversation; with a question, asks it with its context.</summary>
        public void NewTopic(string question, ChatAttach attach)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_busy != null)
            {
                EditorContext.Status("wait for the answer, or stop it, before starting a new topic.");
                return;
            }
            _thread = null;
            _messages.Children.Clear();
            _followUps = null;
            _title.Text = Loc.T("New conversation");
            foreach (var chip in _chips)
            {
                chip.Value.IsChecked = chip.Key == ChatAttach.Query || chip.Key == ChatAttach.Schema;
            }
            Refresh();
            if (question == null)
            {
                _messages.Children.Add(Welcome());
                return;
            }
            Ask(Loc.T(question), attach);
        }

        private void Load(ChatThread thread)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_busy != null)
            {
                return;
            }
            _thread = thread;
            _messages.Children.Clear();
            _followUps = null;
            _title.Text = thread.Title;
            foreach (var chip in _chips.Values)
            {
                chip.IsChecked = false;
            }
            for (var i = 0; i < thread.Messages.Count; i++)
            {
                var m = thread.Messages[i];
                if (m.Role == "user")
                {
                    _messages.Children.Add(UserMessage(m));
                }
                else if (m.Role == "assistant")
                {
                    var answer = AiChat.WithoutNext(m.Text, out var next);
                    _messages.Children.Add(AssistantMessage(answer, out _));
                    if (i == thread.Messages.Count - 1)
                    {
                        AddFollowUps(next);
                    }
                }
                else
                {
                    _messages.Children.Add(ErrorMessage(m.Text, null));
                }
            }
            Refresh();
            ScrollToEnd();
        }

        private void ShowHistory(UIElement anchor)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var menu = new ContextMenu { PlacementTarget = anchor, Placement = PlacementMode.Bottom };
            var threads = ChatStore.All().Take(25).ToList();
            if (threads.Count == 0)
            {
                menu.Items.Add(new MenuItem { Header = Loc.T("No conversations yet."), IsEnabled = false });
            }
            foreach (var t in threads)
            {
                var item = new StackPanel { MaxWidth = 420 };
                item.Children.Add(new TextBlock { Text = t.Title, FontWeight = t == _thread || t.Id == _thread?.Id ? FontWeights.SemiBold : FontWeights.Normal, TextTrimming = TextTrimming.CharacterEllipsis });
                item.Children.Add(new TextBlock { Text = (t.Connection ?? "") + "  ·  " + t.Updated.ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture) + "  ·  " + Loc.T(t.Messages.Count(m => m.Role == "user") + " questions"), Opacity = 0.65, FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis });
                var menuItem = new MenuItem { Header = item, Padding = new Thickness(4, 4, 4, 4) };
                var thread = t;
                menuItem.Click += (s, e) => Load(thread);
                menu.Items.Add(menuItem);
            }
            if (_thread != null)
            {
                menu.Items.Add(new Separator());
                var delete = new MenuItem { Header = Loc.T("Delete this conversation"), Icon = Theme.Icon(DeleteGlyph, 12) };
                delete.Click += (s, e) =>
                {
                    ChatStore.Delete(_thread);
                    NewTopic(null, ChatAttach.None);
                };
                menu.Items.Add(delete);
            }
            menu.IsOpen = true;
        }

        // ---- Asking ----

        private void OnInputKey(object sender, KeyEventArgs e)
        {
            if (e.Key == Key.Enter && (Keyboard.Modifiers & ModifierKeys.Shift) == 0)
            {
                e.Handled = true;
                SendInput();
            }
            else if (e.Key == Key.Escape && _busy != null)
            {
                e.Handled = true;
                _busy.Cancel();
            }
        }

        private void SendInput()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var question = _input.Text.Trim();
            if (question.Length == 0 || _busy != null)
            {
                return;
            }
            var attach = _chips.Where(c => c.Value.IsChecked == true && c.Value.IsEnabled).Aggregate(ChatAttach.None, (all, c) => all | c.Key);
            Ask(question, attach);
        }

        private void Ask(string question, ChatAttach attach)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_busy != null)
            {
                return;
            }
            ChatContext context;
            string problem;
            try
            {
                context = ChatContext.Prepare(attach, out problem);
            }
            catch (Exception ex)
            {
                Log.Error("preparing the AI chat context", ex);
                context = null;
                problem = ex.Message;
            }
            if (context == null)
            {
                if (problem != null)
                {
                    ShowNotice(problem);
                }
                return;
            }
            if (_thread == null)
            {
                _messages.Children.Clear();
                _thread = new ChatThread
                {
                    Title = question.Length > 70 ? question.Substring(0, 70) + "..." : question,
                    Connection = ConnectionResolver.TryGetActive()?.Description,
                };
                _title.Text = _thread.Title;
            }
            RemoveFollowUps();
            var user = new ChatMessage { Role = "user", Text = question, Attached = context.Labels };
            _thread.Messages.Add(user);
            var userView = UserMessage(user);
            _messages.Children.Add(userView);
            _input.Clear();
            foreach (var chip in _chips.Values)
            {
                chip.IsChecked = false;
            }

            var answerView = AssistantMessage("", out var document);
            var progress = new TextBlock { Opacity = 0.7, Margin = new Thickness(0, 4, 0, 0) };
            var progressRow = new StackPanel { Orientation = Orientation.Horizontal };
            progressRow.Children.Add(Theme.Icon(ClockGlyph, 12));
            progressRow.Children.Add(progress);
            progress.Margin = new Thickness(6, 0, 0, 0);
            answerView.Children.Insert(1, progressRow);
            _messages.Children.Add(answerView);
            ScrollToEnd();

            var cancel = new CancellationTokenSource();
            SetBusy(cancel);
            var thread = _thread;
            var heavy = (attach & (ChatAttach.EstimatedPlan | ChatAttach.ActualPlan | ChatAttach.QueryStore)) != 0;
            var stage = Loc.T(heavy ? "Reading the plan and indexes from the database..." : "Claude is thinking...");
            var started = DateTime.Now;
            var streamed = new StringBuilder();
            var shownLength = -1;
            var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(150) };
            timer.Tick += (s, e) =>
            {
                string text;
                lock (streamed)
                {
                    text = streamed.ToString();
                }
                progress.Text = stage + "  " + (int)(DateTime.Now - started).TotalSeconds + " s";
                if (text.Length != shownLength)
                {
                    shownLength = text.Length;
                    Render(document, AiChat.WithoutNext(text, out _));
                    if (IsNearEnd())
                    {
                        ScrollToEnd();
                    }
                }
            };
            timer.Start();

            _ = ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                string answer = null, error = null, sent = null;
                var session = thread.Session;
                await TaskScheduler.Default;
                try
                {
                    sent = context.Build();
                    var input = sent.Length == 0 ? question : question + "\n\n--- Context ---\n" + sent;
                    stage = Loc.T("Claude is thinking...");
                    try
                    {
                        answer = AiChat.Send(ref session, input, d => { lock (streamed) { streamed.Append(d); } }, cancel.Token);
                    }
                    catch (InvalidOperationException) when (session != null && streamed.Length == 0 && !cancel.IsCancellationRequested)
                    {
                        // The session is gone (for example the CLI's history was cleared): start a new
                        // one with the conversation so far.
                        session = null;
                        answer = AiChat.Send(ref session, Transcript(thread) + input, d => { lock (streamed) { streamed.Append(d); } }, cancel.Token);
                    }
                }
                catch (OperationCanceledException)
                {
                    error = null;
                }
                catch (Exception ex)
                {
                    Log.Error("AI chat", ex);
                    error = ex.Message;
                }
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                timer.Stop();
                answerView.Children.Remove(progressRow);
                SetBusy(null);
                user.Context = sent;
                RefreshUserMessage(userView, user);
                thread.Session = session;
                if (answer != null)
                {
                    var text = AiChat.WithoutNext(answer, out var next);
                    Render(document, text);
                    AddCopyAnswer(answerView, text);
                    thread.Messages.Add(new ChatMessage { Role = "assistant", Text = answer });
                    if (_thread == thread)
                    {
                        AddFollowUps(next);
                    }
                }
                else if (cancel.IsCancellationRequested)
                {
                    string partial;
                    lock (streamed)
                    {
                        partial = AiChat.WithoutNext(streamed.ToString(), out _);
                    }
                    var stopped = partial.Length == 0 ? Loc.T("(stopped)") : partial + "\n\n" + Loc.T("(stopped)");
                    Render(document, stopped);
                    thread.Messages.Add(new ChatMessage { Role = "assistant", Text = stopped });
                }
                else
                {
                    _messages.Children.Remove(answerView);
                    thread.Messages.Remove(user);
                    var retry = new Action(() =>
                    {
                        _messages.Children.Remove(userView);
                        Ask(question, attach);
                    });
                    _messages.Children.Add(ErrorMessage(error, retry));
                }
                if (thread.Messages.Count > 0)
                {
                    ChatStore.Save(thread);
                }
                ScrollToEnd();
                FocusInput();
            });
        }

        /// <summary>The conversation so far as text, for a new session that must catch up.</summary>
        private static string Transcript(ChatThread thread)
        {
            var sb = new StringBuilder("The conversation so far:\n");
            foreach (var m in thread.Messages.Where(m => m.Role != "error").Reverse().Skip(1).Reverse())
            {
                sb.Append(m.Role == "user" ? "\nDeveloper: " : "\nYou: ").Append(m.Text);
            }
            var text = sb.ToString();
            return (text.Length > 24000 ? text.Substring(text.Length - 24000) : text) + "\n\nThe new message:\n";
        }

        private void SetBusy(CancellationTokenSource cancel)
        {
            _busy = cancel;
            _sendIcon.Text = cancel != null ? StopGlyph : SendGlyph;
            _send.ToolTip = Loc.T(cancel != null ? "Stop the answer (Esc)" : "Send (Enter)");
        }

        // ---- Context chips ----

        private void AddChip(Panel row, ChatAttach what, string label, string tip)
        {
            var icon = Theme.Icon(CheckGlyph, 10);
            var text = new TextBlock { Text = Loc.T(label), VerticalAlignment = VerticalAlignment.Center };
            var content = new StackPanel { Orientation = Orientation.Horizontal };
            content.Children.Add(icon);
            content.Children.Add(text);
            icon.Margin = new Thickness(0, 0, 5, 0);
            var chip = new ToggleButton
            {
                Content = content,
                Template = ChipTemplate(),
                Padding = new Thickness(8, 2, 9, 3),
                Margin = new Thickness(0, 0, 6, 4),
                Cursor = Cursors.Hand,
                ToolTip = Loc.T(tip),
                FocusVisualStyle = null,
            };
            ToolTipService.SetShowOnDisabled(chip, true);
            void paint()
            {
                var on = chip.IsChecked == true;
                icon.Text = on ? CheckGlyph : AddGlyph;
                chip.Background = on ? Theme.Frozen(Theme.Tint(Theme.Blue, 0x30)) : Brushes.Transparent;
                chip.BorderBrush = on ? Theme.Frozen(Theme.Tint(Theme.Blue, 0xB0)) : Theme.Line;
                chip.Opacity = chip.IsEnabled ? 1 : 0.45;
            }
            chip.Checked += (s, e) => paint();
            chip.Unchecked += (s, e) => paint();
            chip.IsEnabledChanged += (s, e) => paint();
            // The two plans exclude each other.
            chip.Checked += (s, e) =>
            {
                var other = what == ChatAttach.EstimatedPlan ? ChatAttach.ActualPlan : what == ChatAttach.ActualPlan ? ChatAttach.EstimatedPlan : ChatAttach.None;
                if (other != ChatAttach.None && _chips.TryGetValue(other, out var o))
                {
                    o.IsChecked = false;
                }
            };
            paint();
            _chips[what] = chip;
            row.Children.Add(chip);
        }

        private static ControlTemplate _chipTemplate;

        private static ControlTemplate ChipTemplate()
        {
            if (_chipTemplate == null)
            {
                var border = new FrameworkElementFactory(typeof(Border));
                border.SetValue(Border.CornerRadiusProperty, new CornerRadius(11));
                border.SetValue(Border.BorderThicknessProperty, new Thickness(1));
                border.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
                border.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
                border.SetValue(Border.BorderBrushProperty, new TemplateBindingExtension(Control.BorderBrushProperty));
                border.AppendChild(new FrameworkElementFactory(typeof(ContentPresenter)));
                _chipTemplate = new ControlTemplate(typeof(ToggleButton)) { VisualTree = border };
                _chipTemplate.Seal();
            }
            return _chipTemplate;
        }

        /// <summary>Updates the connection line and which chips can be used, from the active query window.</summary>
        private void Refresh()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!IsVisible)
            {
                return;
            }
            EditorContext editor = null;
            ConnectionTarget connection = null;
            try
            {
                editor = EditorContext.Active();
                connection = ConnectionResolver.TryGetActive();
            }
            catch (Exception ex)
            {
                Log.Error("reading the active query window", ex);
            }
            var canQuery = connection != null && connection.CanQuery;
            _chips[ChatAttach.Query].IsEnabled = editor != null;
            _chips[ChatAttach.Schema].IsEnabled = editor != null;
            _chips[ChatAttach.EstimatedPlan].IsEnabled = editor != null && canQuery;
            _chips[ChatAttach.ActualPlan].IsEnabled = editor != null && canQuery;
            _chips[ChatAttach.QueryStore].IsEnabled = canQuery;
            if (_chips[ChatAttach.Query].Content is StackPanel content && content.Children[1] is TextBlock label)
            {
                label.Text = Loc.T(editor != null && editor.HasSelection ? "Selection" : "Query");
            }

            _connection.Children.Clear();
            var description = connection?.Description ?? _thread?.Connection;
            var rule = description == null ? null : ConnectionColors.Find(description);
            var dot = new Border { Width = 8, Height = 8, CornerRadius = new CornerRadius(4), VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 1, 6, 0) };
            dot.Background = rule != null ? Theme.BrushFromText(rule.Color) : Theme.Strong;
            _connection.Children.Add(dot);
            if (rule != null && !string.IsNullOrEmpty(rule.Label))
            {
                var badge = Theme.Badge(rule.Label, ((SolidColorBrush)Theme.BrushFromText(rule.Color)).Color, 9);
                badge.Margin = new Thickness(0, 0, 6, 0);
                _connection.Children.Add(badge);
            }
            _connection.Children.Add(new TextBlock
            {
                Text = description ?? Loc.T("No query window connected"),
                Opacity = 0.7,
                FontSize = 11,
                TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center,
            });
        }

        // ---- Messages ----

        private UIElement Welcome()
        {
            var panel = new StackPanel { Margin = new Thickness(0, 18, 0, 0), MaxWidth = 560, HorizontalAlignment = HorizontalAlignment.Left };
            var icon = Theme.Icon(ChatGlyph, 26, Theme.Frozen(Theme.Purple));
            icon.HorizontalAlignment = HorizontalAlignment.Left;
            panel.Children.Add(icon);
            panel.Children.Add(new TextBlock { Text = Loc.T("Talk with AI about your query"), FontSize = 15, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 8, 0, 4) });
            panel.Children.Add(Theme.Dim(Loc.T("Pick what to send below the box (the query, table definitions, a plan), ask, then keep asking: the AI remembers the conversation. It cannot read or change the database itself.")));
            panel.Children.Add(new TextBlock { Text = Loc.T("Start with"), FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 16, 0, 6) });
            panel.Children.Add(Starter(PlanGlyph, "Analyze the plan", "Where the query spends its time, and which indexes or rewrites help.",
                "Analyze the execution plan of this query: where does the cost go, and what should I change?", ChatAttach.Query | ChatAttach.Schema | ChatAttach.EstimatedPlan));
            panel.Children.Add(Starter(ExplainGlyph, "Explain the query", "What it does step by step, and the risks.",
                "Explain what this query does, step by step, and any risks you see.", ChatAttach.Query | ChatAttach.Schema));
            panel.Children.Add(Starter(ReviewGlyph, "Review and improve", "Bugs, performance problems and a better version.",
                "Review this query for correctness and performance, and suggest an improved version.", ChatAttach.Query | ChatAttach.Schema));
            panel.Children.Add(Starter(ClockGlyph, "Find the slowest queries", "The most expensive queries of the last 7 days from Query Store.",
                "Which queries cost the most in the last 7 days, and what should I fix first?", ChatAttach.QueryStore));
            var keys = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 14, 0, 0) };
            keys.Children.Add(new TextBlock { Text = Loc.T("Open this panel:"), Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 6, 0) });
            keys.Children.Add(Theme.Keycap("Ctrl+B, Ctrl+I"));
            panel.Children.Add(keys);
            return panel;
        }

        private Button Starter(string glyph, string title, string description, string question, ChatAttach attach)
        {
            var text = new StackPanel { Margin = new Thickness(10, 0, 0, 0) };
            text.Children.Add(new TextBlock { Text = Loc.T(title), FontWeight = FontWeights.SemiBold });
            text.Children.Add(new TextBlock { Text = Loc.T(description), Opacity = 0.7, TextWrapping = TextWrapping.Wrap });
            var row = new DockPanel();
            var icon = Theme.Icon(glyph, 16, Theme.Frozen(Theme.Blue));
            icon.VerticalAlignment = VerticalAlignment.Top;
            icon.Margin = new Thickness(0, 2, 0, 0);
            row.Children.Add(icon);
            row.Children.Add(text);
            var button = Theme.FlatButton("", null);
            button.Content = row;
            button.Padding = new Thickness(12, 9, 12, 9);
            button.Margin = new Thickness(0, 0, 0, 6);
            button.HorizontalContentAlignment = HorizontalAlignment.Stretch;
            button.Click += (s, e) => Ask(Loc.T(question), attach);
            return button;
        }

        private StackPanel UserMessage(ChatMessage message)
        {
            var bubble = new Border
            {
                Background = Theme.Frozen(Theme.Tint(Theme.Blue, 0x2C)),
                CornerRadius = new CornerRadius(8),
                Padding = new Thickness(11, 7, 11, 8),
                HorizontalAlignment = HorizontalAlignment.Right,
                Child = new TextBox
                {
                    Text = message.Text,
                    IsReadOnly = true,
                    TextWrapping = TextWrapping.Wrap,
                    Background = Brushes.Transparent,
                    BorderThickness = new Thickness(0),
                    Padding = new Thickness(0),
                },
            };
            ((TextBox)bubble.Child).SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            var view = new StackPanel { Margin = new Thickness(48, 4, 0, 10), HorizontalAlignment = HorizontalAlignment.Right };
            view.Children.Add(bubble);
            RefreshUserMessage(view, message);
            return view;
        }

        /// <summary>The labels of what was sent, and the sent context on request.</summary>
        private static void RefreshUserMessage(StackPanel view, ChatMessage message)
        {
            while (view.Children.Count > 1)
            {
                view.Children.RemoveAt(1);
            }
            if (message.Attached == null || message.Attached.Count == 0)
            {
                return;
            }
            var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 4, 2, 0) };
            row.Children.Add(Theme.Icon(AttachGlyph, 10, Theme.Strong));
            row.Children.Add(new TextBlock { Text = string.Join("  ·  ", message.Attached), Opacity = 0.65, FontSize = 11, Margin = new Thickness(4, 0, 0, 0), TextWrapping = TextWrapping.Wrap });
            view.Children.Add(row);
            if (string.IsNullOrEmpty(message.Context))
            {
                return;
            }
            var details = new TextBox
            {
                Text = message.Context,
                IsReadOnly = true,
                FontFamily = Theme.Mono,
                FontSize = 11,
                MaxHeight = 260,
                Background = Theme.Faint,
                BorderBrush = Theme.Line,
                Padding = new Thickness(6, 4, 6, 4),
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                Visibility = Visibility.Collapsed,
                Margin = new Thickness(0, 4, 0, 0),
            };
            details.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            var toggle = new TextBlock { Text = Loc.T("Show what was sent"), FontSize = 11, Cursor = Cursors.Hand, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 2, 2, 0), TextDecorations = TextDecorations.Underline, Opacity = 0.75 };
            toggle.MouseLeftButtonUp += (s, e) =>
            {
                var show = details.Visibility != Visibility.Visible;
                details.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
                toggle.Text = Loc.T(show ? "Hide what was sent" : "Show what was sent");
            };
            view.Children.Add(toggle);
            view.Children.Add(details);
        }

        private StackPanel AssistantMessage(string markdown, out FlowDocument document)
        {
            var who = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 4) };
            who.Children.Add(Theme.Badge("AI", Theme.Purple, 9.5));
            who.Children.Add(new TextBlock { Text = "Claude", FontWeight = FontWeights.SemiBold, Margin = new Thickness(6, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center });
            document = new FlowDocument { PagePadding = new Thickness(0) };
            document.SetResourceReference(FlowDocument.FontFamilyProperty, VsFonts.EnvironmentFontFamilyKey);
            document.SetResourceReference(FlowDocument.FontSizeProperty, VsFonts.EnvironmentFontSizeKey);
            document.SetResourceReference(FlowDocument.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            var body = new RichTextBox
            {
                Document = document,
                IsReadOnly = true,
                IsDocumentEnabled = true,
                Background = Brushes.Transparent,
                BorderThickness = new Thickness(0),
                Padding = new Thickness(0),
                VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            };
            body.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            Render(document, markdown);
            var view = new StackPanel { Margin = new Thickness(0, 4, 0, 14) };
            view.Children.Add(who);
            view.Children.Add(body);
            return view;
        }

        private static void AddCopyAnswer(StackPanel view, string text)
        {
            var copy = Theme.FlatButton(Loc.T("Copy answer"), Theme.CopyGlyph);
            copy.HorizontalAlignment = HorizontalAlignment.Left;
            copy.Margin = new Thickness(0, 6, 0, 0);
            copy.Background = Brushes.Transparent;
            copy.BorderThickness = new Thickness(0);
            copy.Opacity = 0.8;
            copy.Click += (s, e) => Copy(text);
            view.Children.Add(copy);
        }

        private UIElement ErrorMessage(string message, Action retry)
        {
            var row = new DockPanel();
            var icon = Theme.Icon(Theme.WarningGlyph, 14, Theme.Frozen(Theme.Amber));
            icon.VerticalAlignment = VerticalAlignment.Top;
            row.Children.Add(icon);
            if (retry != null)
            {
                var again = Theme.FlatButton(Loc.T("Try again"), RetryGlyph);
                again.Margin = new Thickness(10, 0, 0, 0);
                again.VerticalAlignment = VerticalAlignment.Top;
                again.Click += (s, e) => retry();
                DockPanel.SetDock(again, Dock.Right);
                row.Children.Add(again);
            }
            row.Children.Add(new TextBlock { Text = Loc.T(message ?? "Claude did not answer."), TextWrapping = TextWrapping.Wrap, Margin = new Thickness(8, 0, 0, 0) });
            var pill = Theme.Pill(row, Theme.Amber);
            pill.HorizontalAlignment = HorizontalAlignment.Stretch;
            pill.Padding = new Thickness(10, 7, 10, 7);
            pill.Margin = new Thickness(0, 0, 0, 12);
            return pill;
        }

        /// <summary>A note that something could not be sent; it disappears with the next message.</summary>
        private void ShowNotice(string message)
        {
            if (_messages.Children.Count == 1 && _thread == null)
            {
                _messages.Children.Clear();
            }
            _messages.Children.Add(ErrorMessage(message, null));
            ScrollToEnd();
        }

        private void AddFollowUps(IList<string> questions)
        {
            RemoveFollowUps();
            if (questions == null || questions.Count == 0)
            {
                return;
            }
            _followUps = new WrapPanel { Margin = new Thickness(0, -6, 0, 12) };
            foreach (var q in questions)
            {
                var button = Theme.FlatButton(q, null);
                button.Margin = new Thickness(0, 0, 6, 6);
                button.ToolTip = Loc.T("Ask this");
                ((TextBlock)((StackPanel)button.Content).Children[0]).TextWrapping = TextWrapping.Wrap;
                var question = q;
                button.Click += (s, e) => Ask(question, ChatAttach.None);
                _followUps.Children.Add(button);
            }
            _messages.Children.Add(_followUps);
        }

        private void RemoveFollowUps()
        {
            if (_followUps != null)
            {
                _messages.Children.Remove(_followUps);
                _followUps = null;
            }
        }

        private bool IsNearEnd() => _scroll.VerticalOffset >= _scroll.ScrollableHeight - 60;

        private void ScrollToEnd() => Dispatcher.BeginInvoke(new Action(() => _scroll.ScrollToEnd()), DispatcherPriority.Background);

        private static Button AccentButton(UIElement content)
        {
            var border = new FrameworkElementFactory(typeof(Border));
            border.SetValue(Border.CornerRadiusProperty, new CornerRadius(4));
            border.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
            border.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
            var presenter = new FrameworkElementFactory(typeof(ContentPresenter));
            presenter.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            presenter.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            border.AppendChild(presenter);
            var template = new ControlTemplate(typeof(Button)) { VisualTree = border };
            var hover = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
            hover.Setters.Add(new Setter(UIElement.OpacityProperty, 0.85));
            template.Triggers.Add(hover);
            return new Button
            {
                Template = template,
                Content = content,
                Background = Theme.Frozen(Theme.Blue),
                Width = 30,
                Height = 28,
                Cursor = Cursors.Hand,
                ToolTip = Loc.T("Send (Enter)"),
                FocusVisualStyle = null,
            };
        }

        // ---- Markdown ----

        private static readonly Regex Inline = new Regex(@"\*\*(.+?)\*\*|`([^`]+)`", RegexOptions.Compiled);

        /// <summary>Renders the Markdown Claude writes: headings, lists, tables, bold, code and SQL blocks.</summary>
        private static void Render(FlowDocument document, string markdown)
        {
            document.Blocks.Clear();
            var lines = (markdown ?? "").Replace("\r\n", "\n").Split('\n');
            var paragraph = new List<string>();
            void flush()
            {
                if (paragraph.Count > 0)
                {
                    var p = new Paragraph { Margin = new Thickness(0, 0, 0, 8) };
                    AddInline(p.Inlines, string.Join(" ", paragraph));
                    document.Blocks.Add(p);
                    paragraph.Clear();
                }
            }
            for (var i = 0; i < lines.Length; i++)
            {
                var line = lines[i];
                var trimmed = line.Trim();
                if (trimmed.StartsWith("```", StringComparison.Ordinal))
                {
                    flush();
                    var language = trimmed.Substring(3).Trim();
                    var code = new List<string>();
                    i++;
                    while (i < lines.Length && !lines[i].Trim().StartsWith("```", StringComparison.Ordinal))
                    {
                        code.Add(lines[i]);
                        i++;
                    }
                    document.Blocks.Add(new BlockUIContainer(CodeBlock(string.Join(Environment.NewLine, code), language)) { Margin = new Thickness(0, 2, 0, 10) });
                    continue;
                }
                if (trimmed.StartsWith("|", StringComparison.Ordinal))
                {
                    flush();
                    var rows = new List<string>();
                    while (i < lines.Length && lines[i].Trim().StartsWith("|", StringComparison.Ordinal))
                    {
                        rows.Add(lines[i].Trim());
                        i++;
                    }
                    i--;
                    document.Blocks.Add(Table(rows));
                    continue;
                }
                if (trimmed.Length == 0)
                {
                    flush();
                    continue;
                }
                var heading = Regex.Match(trimmed, @"^(#{1,4})\s+(.*)$");
                if (heading.Success)
                {
                    flush();
                    var h = new Paragraph { FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 6, 0, 6) };
                    h.FontSize = heading.Groups[1].Length <= 2 ? 15 : 13.5;
                    AddInline(h.Inlines, heading.Groups[2].Value);
                    document.Blocks.Add(h);
                    continue;
                }
                var bullet = Regex.Match(line, @"^(\s*)([-*•]|\d+[.)])\s+(.*)$");
                if (bullet.Success)
                {
                    flush();
                    var depth = bullet.Groups[1].Value.Replace("\t", "    ").Length / 2;
                    var marker = char.IsDigit(bullet.Groups[2].Value[0]) ? bullet.Groups[2].Value : "•";
                    var item = new Paragraph { Margin = new Thickness(12 + depth * 14, 0, 0, 3), TextIndent = -12 };
                    item.Inlines.Add(new Run(marker + " ") { FontWeight = marker == "•" ? FontWeights.Bold : FontWeights.Normal });
                    AddInline(item.Inlines, bullet.Groups[3].Value);
                    document.Blocks.Add(item);
                    continue;
                }
                if (Regex.IsMatch(trimmed, @"^(-{3,}|\*{3,}|_{3,})$"))
                {
                    flush();
                    continue;
                }
                paragraph.Add(trimmed);
            }
            flush();
        }

        private static void AddInline(InlineCollection inlines, string text)
        {
            var at = 0;
            foreach (Match m in Inline.Matches(text))
            {
                if (m.Index > at)
                {
                    inlines.Add(new Run(text.Substring(at, m.Index - at)));
                }
                if (m.Groups[1].Success)
                {
                    var bold = new Bold();
                    AddInline(bold.Inlines, m.Groups[1].Value);
                    inlines.Add(bold);
                }
                else
                {
                    inlines.Add(new Run(m.Groups[2].Value) { FontFamily = Theme.Mono, Background = Theme.Subtle });
                }
                at = m.Index + m.Length;
            }
            if (at < text.Length)
            {
                inlines.Add(new Run(text.Substring(at)));
            }
        }

        private static Block Table(IList<string> rows)
        {
            var cells = rows
                .Where(r => !Regex.IsMatch(r, @"^\|[\s:\-|]+\|?$"))
                .Select(r => r.Trim('|').Split('|').Select(c => c.Trim()).ToArray())
                .ToList();
            var columns = cells.Count == 0 ? 0 : cells.Max(c => c.Length);
            var table = new Table { CellSpacing = 0, Margin = new Thickness(0, 2, 0, 10), BorderBrush = Theme.Line, BorderThickness = new Thickness(1) };
            for (var c = 0; c < columns; c++)
            {
                table.Columns.Add(new TableColumn());
            }
            var group = new TableRowGroup();
            for (var r = 0; r < cells.Count; r++)
            {
                var row = new TableRow { Background = r == 0 ? Theme.Subtle : r % 2 == 0 ? Theme.Faint : Brushes.Transparent };
                for (var c = 0; c < columns; c++)
                {
                    var p = new Paragraph { Margin = new Thickness(0), FontWeight = r == 0 ? FontWeights.SemiBold : FontWeights.Normal };
                    AddInline(p.Inlines, c < cells[r].Length ? cells[r][c] : "");
                    row.Cells.Add(new TableCell(p) { Padding = new Thickness(6, 3, 6, 3), BorderBrush = Theme.Line, BorderThickness = new Thickness(0, 0, c < columns - 1 ? 1 : 0, 0) });
                }
                group.Rows.Add(row);
            }
            table.RowGroups.Add(group);
            return table;
        }

        /// <summary>A code block; SQL gets buttons to put it in the editor.</summary>
        private static UIElement CodeBlock(string code, string language)
        {
            var sql = language.Length == 0 || language.Equals("sql", StringComparison.OrdinalIgnoreCase) || language.Equals("tsql", StringComparison.OrdinalIgnoreCase) || language.Equals("t-sql", StringComparison.OrdinalIgnoreCase);
            var text = new TextBox
            {
                Text = code,
                IsReadOnly = true,
                FontFamily = Theme.Mono,
                FontSize = 12.5,
                Background = Brushes.Transparent,
                BorderThickness = new Thickness(0),
                Padding = new Thickness(10, 8, 10, 8),
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                MaxHeight = 380,
            };
            text.SetResourceReference(Control.ForegroundProperty, EnvironmentColors.ToolWindowTextBrushKey);
            var bar = new DockPanel { Background = Theme.Faint };
            var name = new TextBlock { Text = sql ? "SQL" : language.ToUpperInvariant(), Opacity = 0.65, FontSize = 11, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(10, 0, 0, 0) };
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 3, 4, 3) };
            if (sql)
            {
                var insert = SmallButton(Loc.T("Insert into editor"), InsertGlyph, Loc.T("Insert at the caret of the query window, selected so you can review it. Undo removes it."));
                insert.Click += (s, e) => InsertSql(code);
                var open = SmallButton(Loc.T("Open in new window"), Theme.OpenGlyph, null);
                open.Click += (s, e) =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    EditorContext.OpenInNewQuery(code + Environment.NewLine);
                };
                buttons.Children.Add(insert);
                buttons.Children.Add(open);
            }
            var copy = SmallButton(Loc.T("Copy"), Theme.CopyGlyph, null);
            copy.Click += (s, e) => Copy(code);
            buttons.Children.Add(copy);
            DockPanel.SetDock(buttons, Dock.Right);
            bar.Children.Add(buttons);
            bar.Children.Add(name);
            var panel = new DockPanel();
            DockPanel.SetDock(bar, Dock.Top);
            panel.Children.Add(bar);
            panel.Children.Add(text);
            return new Border { Child = panel, BorderBrush = Theme.Line, BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(4), Background = Theme.Faint, ClipToBounds = true };
        }

        private static Button SmallButton(string text, string glyph, string tip)
        {
            var button = Theme.FlatButton(text, glyph);
            button.Background = Brushes.Transparent;
            button.BorderThickness = new Thickness(0);
            button.Padding = new Thickness(6, 2, 6, 2);
            button.Margin = new Thickness(2, 0, 0, 0);
            button.FontSize = 11.5;
            button.ToolTip = tip;
            return button;
        }

        private static void Copy(string text)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                Clipboard.SetText(text);
                EditorContext.Status("copied.");
            }
            catch (COMException ex)
            {
                Log.Error("copying", ex);
            }
        }

        /// <summary>Inserts SQL at the caret of the query window on lines of its own, selected for review.</summary>
        private static void InsertSql(string sql)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.OpenInNewQuery(sql + Environment.NewLine);
                return;
            }
            var at = editor.Caret;
            var before = at > 0 && editor.Text[at - 1] != '\n' ? Environment.NewLine : "";
            var text = before + sql.TrimEnd() + Environment.NewLine;
            editor.Replace(at, at, text);
            editor.Select(at + before.Length, at + text.Length);
            EditorContext.Status("Claude's SQL inserted; review it before running. Undo removes it.");
        }
    }
}
