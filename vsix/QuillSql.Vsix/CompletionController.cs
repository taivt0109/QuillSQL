using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.ComponentModelHost;
using Microsoft.VisualStudio.Editor;
using Microsoft.VisualStudio.Language.Intellisense;
using Microsoft.VisualStudio.OLE.Interop;
using Microsoft.VisualStudio.Text;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.TextManager.Interop;
using Microsoft.VisualStudio.Threading;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Command filter on one SQL editor view: watches typing, asks the engine for
    /// suggestions and routes navigation keys to the popup while it is open.
    /// </summary>
    internal sealed class CompletionController : IOleCommandTarget, IVsTextViewEvents
    {
        /// <summary>Words after which a space opens the suggestions: tables after these...</summary>
        private static readonly string[] TriggerAfterSpace =
        {
            "FROM", "JOIN", "EXEC", "EXECUTE", "INTO", "UPDATE", "APPLY", "USING", "TABLE",
            // ...and columns, aliases and functions in conditions, lists and expressions.
            "SELECT", "WHERE", "AND", "OR", "NOT", "ON", "BY", "HAVING", "SET", "CASE", "WHEN", "THEN", "ELSE", "DISTINCT",
            // ...and the words that must follow these.
            "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "IS", "ORDER", "GROUP", "PARTITION", "CREATE", "ALTER", "DROP",
            "INSERT", "UNION", "BEGIN", "COMMIT", "ROLLBACK",
        };

        /// <summary>Words after which an opening parenthesis opens the suggestions: hints, OVER and EXISTS.</summary>
        private static readonly string[] TriggerAfterParenthesis = { "WITH", "OPTION", "OVER", "EXISTS" };

        /// <summary>Characters after which a space opens the suggestions: comparisons and lists.</summary>
        private const string TriggerAfterSpaceSymbols = "=<>,(+-*/";

        private readonly IVsTextView _view;
        private readonly CompletionPopup _popup = new CompletionPopup();
        private readonly SignatureTip _signature = new SignatureTip();
        private int _signatureVersion;
        private readonly int[] _firstVisible = { -1, -1 };
        private IOleCommandTarget _next;
        private IConnectionPoint _events;
        private uint _eventsCookie;
        private int _replaceStart;
        private int _version;

        /// <summary>Latest suggestion labels, for diagnostics and tests.</summary>
        public static string LastShown { get; private set; } = "";

        private CompletionController(IVsTextView view)
        {
            _view = view;
            _popup.Committed += () => Commit(true);
            _popup.Clicked += () =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                // After the click's own work (a tab, Copy): the keyboard goes back to the editor.
                _ = System.Windows.Threading.Dispatcher.CurrentDispatcher.BeginInvoke(new Action(() =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    // Not when the click opened something else (the script in a new window).
                    if (_popup.HasKeyboard)
                    {
                        _view.SendExplicitFocus();
                    }
                }), System.Windows.Threading.DispatcherPriority.Input);
            };
        }

        /// <summary>The controller of each attached view, for the test hook.</summary>
        private static readonly Dictionary<IVsTextView, CompletionController> Attached = new Dictionary<IVsTextView, CompletionController>();

        public static CompletionController ForView(IVsTextView view) => Attached.TryGetValue(view, out var controller) ? controller : null;

        /// <summary>The suggestions showing, the selected one marked with *; "closed" when the list is closed.</summary>
        public string ListState() => !_popup.IsOpen ? "closed"
            : string.Join(",", _popup.Items.Select(i => (ReferenceEquals(i, _popup.Selected) ? "*" : "") + i.Label));

        /// <summary>The parameter hints showing, or "closed".</summary>
        public string TipState() => _signature.IsOpen ? "open " + _signature.Text : "closed";

        public static CompletionController Attach(IVsTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var controller = new CompletionController(view);
            if (view.AddCommandFilter(controller, out controller._next) != VSConstants.S_OK)
            {
                return null;
            }
            Attached[view] = controller;
            if (view is IConnectionPointContainer container)
            {
                var iid = typeof(IVsTextViewEvents).GUID;
                container.FindConnectionPoint(ref iid, out controller._events);
                controller._events?.Advise(controller, out controller._eventsCookie);
            }
            return controller;
        }

        public void Detach()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            Attached.Remove(_view);
            _popup.Close();
            _signature.Close();
            _view.RemoveCommandFilter(this);
            _events?.Unadvise(_eventsCookie);
        }

        private bool _loggedLanguage;

        private bool IsSqlBuffer(out IVsTextLines lines)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            lines = null;
            if (_view.GetBuffer(out lines) != VSConstants.S_OK || lines == null)
            {
                return false;
            }
            lines.GetLanguageServiceID(out var id);
            var sql = Array.IndexOf(EditorContext.SqlLanguageServices, id) >= 0;
            if (!sql && !_loggedLanguage)
            {
                _loggedLanguage = true;
                Log.Write("view language service " + id.ToString("B") + " is not T-SQL; no suggestions");
            }
            return sql;
        }

        public int QueryStatus(ref Guid group, uint count, OLECMD[] commands, IntPtr text)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var result = _next.QueryStatus(ref group, count, commands, text);
            // SSMS turns off Go To Definition and Quick Info in query windows; F12 opens the
            // object's script and Quick Info its card, so offer both whatever SSMS says.
            if (Settings.Current.Enabled && commands != null && commands.Length > 0 && IsSqlBuffer(out _)
                && ((group == VSConstants.GUID_VSStandardCommandSet97 && commands[0].cmdID == (uint)VSConstants.VSStd97CmdID.GotoDefn)
                    || (group == VSConstants.VSStd2K && commands[0].cmdID == (uint)VSConstants.VSStd2KCmdID.QUICKINFO)))
            {
                commands[0].cmdf = (uint)(OLECMDF.OLECMDF_SUPPORTED | OLECMDF.OLECMDF_ENABLED);
                return VSConstants.S_OK;
            }
            return result;
        }

        public int Exec(ref Guid group, uint id, uint options, IntPtr input, IntPtr output)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var std2k = group == VSConstants.VSStd2K;
            var command = (VSConstants.VSStd2KCmdID)id;
            if (!Settings.Current.Enabled)
            {
                return _next.Exec(ref group, id, options, input, output);
            }
            // F12 on a table, view, procedure or function opens its script.
            if (group == VSConstants.GUID_VSStandardCommandSet97 && id == (uint)VSConstants.VSStd97CmdID.GotoDefn && IsSqlBuffer(out _))
            {
                try
                {
                    if (ObjectScripts.ScriptAtCaret(false))
                    {
                        return VSConstants.S_OK;
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("go to definition", ex);
                }
            }
            if (std2k && (command == VSConstants.VSStd2KCmdID.COMPLETEWORD || command == VSConstants.VSStd2KCmdID.SHOWMEMBERLIST))
            {
                Log.Verbose("command " + command);
            }
            try
            {
                if (std2k && _popup.IsOpen)
                {
                    switch (command)
                    {
                        case VSConstants.VSStd2KCmdID.UP: _popup.Move(-1); return VSConstants.S_OK;
                        case VSConstants.VSStd2KCmdID.DOWN: _popup.Move(1); return VSConstants.S_OK;
                        case VSConstants.VSStd2KCmdID.PAGEUP: _popup.Move(-8); return VSConstants.S_OK;
                        case VSConstants.VSStd2KCmdID.PAGEDN: _popup.Move(8); return VSConstants.S_OK;
                        case VSConstants.VSStd2KCmdID.RETURN:
                        case VSConstants.VSStd2KCmdID.TAB:
                            // With nothing selected the list only shows guesses: the key does its usual job.
                            if (_popup.Selected != null && (command == VSConstants.VSStd2KCmdID.TAB ? Settings.Current.InsertWithTab : Settings.Current.InsertWithEnter))
                            {
                                Commit(true);
                                return VSConstants.S_OK;
                            }
                            _popup.Close();
                            break;
                        case VSConstants.VSStd2KCmdID.TYPECHAR:
                            // Optional keys that insert the suggestion and then type themselves.
                            var typedChar = (char)Convert.ToUInt16(Marshal.GetObjectForNativeVariant(input));
                            var settings = Settings.Current;
                            if (_popup.Selected != null && ((typedChar == ' ' && settings.InsertWithSpace) || (typedChar == '.' && settings.InsertWithDot)
                                || (typedChar == ',' && settings.InsertWithComma) || (typedChar == '(' && settings.InsertWithParenthesis)))
                            {
                                // The key types itself next: the word alone, without its parentheses or quotes.
                                Commit(false);
                            }
                            break;
                        case VSConstants.VSStd2KCmdID.CANCEL:
                            _popup.Close();
                            return VSConstants.S_OK;
                        case VSConstants.VSStd2KCmdID.LEFT:
                        case VSConstants.VSStd2KCmdID.RIGHT:
                        case VSConstants.VSStd2KCmdID.HOME:
                        case VSConstants.VSStd2KCmdID.END:
                            _popup.Close();
                            break;
                    }
                }
                if (std2k && command == VSConstants.VSStd2KCmdID.CANCEL && CardPopup.IsOpen)
                {
                    CardPopup.Close();
                    return VSConstants.S_OK;
                }
                if (std2k && command == VSConstants.VSStd2KCmdID.CANCEL && _signature.IsOpen)
                {
                    _signature.Close();
                    return VSConstants.S_OK;
                }
                if (std2k && (command == VSConstants.VSStd2KCmdID.COMPLETEWORD || command == VSConstants.VSStd2KCmdID.SHOWMEMBERLIST))
                {
                    Trigger(true);
                    return VSConstants.S_OK;
                }
                if (std2k && command == VSConstants.VSStd2KCmdID.TAB && !_popup.IsOpen && ExpandAtCaret())
                {
                    return VSConstants.S_OK;
                }
                if (std2k && command == VSConstants.VSStd2KCmdID.QUICKINFO && TriggerQuickInfo())
                {
                    return VSConstants.S_OK;
                }
            }
            catch (Exception ex)
            {
                Log.Error("before command", ex);
            }

            var typed = std2k && command == VSConstants.VSStd2KCmdID.TYPECHAR ? (char)Convert.ToUInt16(Marshal.GetObjectForNativeVariant(input)) : '\0';
            string closing = null;
            try
            {
                if (typed != '\0' && Overtype(typed))
                {
                    return VSConstants.S_OK;
                }
                closing = typed != '\0' ? ClosingFor(typed) : null;
            }
            catch (Exception ex)
            {
                Log.Error("auto-close", ex);
            }

            // A keyword, function or data type typed in lower case is written in upper case once a
            // space, a line break or a punctuation mark ends it.
            string keywordText = null;
            var keywordEnd = -1;
            var keywordNext = typed != '\0' ? typed : command == VSConstants.VSStd2KCmdID.RETURN ? '\n' : '\t';
            try
            {
                if (Settings.Current.UppercaseKeywords && std2k
                    && ((typed != '\0' && " (),;".IndexOf(typed) >= 0) || command == VSConstants.VSStd2KCmdID.RETURN || command == VSConstants.VSStd2KCmdID.TAB)
                    && TryGetText(out var before, out var at) && EndsWithLowerCaseWord(before, at))
                {
                    keywordText = before;
                    keywordEnd = at;
                }
            }
            catch (Exception ex)
            {
                Log.Error("keyword case", ex);
            }

            var result = _next.Exec(ref group, id, options, input, output);

            if (keywordText != null)
            {
                UppercaseKeyword(keywordText, keywordEnd, keywordNext);
            }

            try
            {
                if (typed != '\0')
                {
                    if (closing != null)
                    {
                        InsertClosing(closing);
                    }
                    OnTyped(typed);
                    if (typed == '(' || typed == ',' || _signature.IsOpen || (typed == ' ' && LineStartsWithExec()))
                    {
                        UpdateSignature();
                    }
                }
                else if (std2k && _popup.IsOpen && (command == VSConstants.VSStd2KCmdID.BACKSPACE || command == VSConstants.VSStd2KCmdID.DELETE))
                {
                    Trigger(false);
                }
                if (std2k && _signature.IsOpen && typed == '\0' && (command == VSConstants.VSStd2KCmdID.BACKSPACE || command == VSConstants.VSStd2KCmdID.DELETE
                    || command == VSConstants.VSStd2KCmdID.LEFT || command == VSConstants.VSStd2KCmdID.RIGHT))
                {
                    UpdateSignature();
                }
            }
            catch (Exception ex)
            {
                Log.Error("after command", ex);
            }
            return result;
        }

        /// <summary>
        /// Quick Info from the keyboard, when SSMS passes the command on: the object card in its
        /// own popup, since SSMS closes Quick Info sessions started by a shortcut.
        /// </summary>
        private bool TriggerQuickInfo()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return IsSqlBuffer(out _) && ShowCard(_view);
        }

        /// <summary>Opens the object card (Quick Info) at the caret of a query window.</summary>
        public static bool ShowCard(IVsTextView textView)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (textView == null || !(Package.GetGlobalService(typeof(SComponentModel)) is IComponentModel model))
            {
                return false;
            }
            var view = model.GetService<IVsEditorAdaptersFactoryService>()?.GetWpfTextView(textView);
            Log.Verbose("object card requested");
            return CardPopup.Show(view);
        }

        /// <summary>
        /// Tab after a snippet prefix expands the snippet; Tab after `*` lists the columns.
        /// Otherwise Tab is left to the editor.
        /// </summary>
        private bool ExpandAtCaret()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.For(_view);
            if (editor == null || editor.HasSelection || editor.Caret == 0 || char.IsWhiteSpace(editor.Text[editor.Caret - 1]))
            {
                return false;
            }
            var before = editor.Text[editor.Caret - 1];
            if (!(char.IsLetterOrDigit(before) || before == '_' || before == '*'))
            {
                return false;
            }
            var key = before == '*' ? Commands.MetadataKey(out _) : "";
            var reply = EngineProcess.Shared.Request("tab",
                "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret + ",\"env\":" + Commands.SnippetEnv(""));
            var edit = reply == null ? null : Json.Parse<TabReply>(reply)?.Result;
            if (edit != null)
            {
                Log.Verbose("tab expanded to " + edit.Text);
            }
            return Commands.ApplyEdit(editor, edit);
        }

        private static bool IsWordChar(char c) => char.IsLetterOrDigit(c) || c == '_' || c == '@' || c == '#' || c == '$';

        /// <summary>Where a closing character was inserted automatically, so typing it moves over it.</summary>
        private int _autoClosed = -1;

        /// <summary>
        /// The text to insert after `typed`, or null. Quotes and brackets are closed only before
        /// whitespace or punctuation, and never inside a comment or an open string.
        /// </summary>
        private string ClosingFor(char typed)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var settings = Settings.Current;
            var wanted = (typed == '\'' && settings.CloseSingleQuote) || (typed == '[' && settings.CloseSquareBracket)
                || (typed == '(' && settings.CloseParenthesis) || (typed == '*' && settings.CloseComment);
            if (!wanted || !TryGetText(out var text, out var caret))
            {
                return null;
            }
            var lineStart = caret == 0 ? 0 : text.LastIndexOf('\n', caret - 1) + 1;
            var line = text.Substring(lineStart, caret - lineStart);
            if (line.Contains("--"))
            {
                return null;
            }
            var next = caret < text.Length ? text[caret] : '\n';
            var nextOk = char.IsWhiteSpace(next) || next == ')' || next == ',' || next == ';' || next == ']';
            var previous = caret > 0 ? text[caret - 1] : ' ';
            switch (typed)
            {
                case '*':
                    return previous == '/' ? "*/" : null;
                case '\'':
                    // An odd number of quotes before the caret means this one closes a string.
                    var inString = line.Count(c => c == '\'') % 2 == 1;
                    var startsString = !IsWordChar(previous) || (previous == 'N' && (caret < 2 || !IsWordChar(text[caret - 2])));
                    return !inString && startsString && nextOk ? "'" : null;
                case '[':
                    return nextOk ? "]" : null;
                case '(':
                    return nextOk ? ")" : null;
            }
            return null;
        }

        private void InsertClosing(string closing)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.For(_view);
            if (editor == null || (editor.Caret < editor.Text.Length && editor.Text.Substring(editor.Caret).StartsWith(closing, StringComparison.Ordinal)))
            {
                // SSMS closed it already.
                return;
            }
            editor.Replace(editor.Caret, editor.Caret, closing);
            editor.Select(editor.Caret, editor.Caret);
            _autoClosed = closing.Length == 1 ? editor.Caret : -1;
        }

        /// <summary>Typing the closing character that was inserted automatically just moves over it.</summary>
        private bool Overtype(char typed)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_autoClosed < 0 || !Settings.Current.OvertypeClosing || !(typed == '\'' || typed == ']' || typed == ')'))
            {
                return false;
            }
            var editor = EditorContext.For(_view);
            // The closing character moves right as the text inside is typed: it is the one at the
            // caret when only that text, on the same line and without another closer, lies between.
            var between = editor == null || editor.Caret < _autoClosed ? null : editor.Text.Substring(_autoClosed, editor.Caret - _autoClosed);
            var hit = between != null && between.IndexOf('\n') < 0 && between.IndexOf(typed) < 0
                && editor.Caret < editor.Text.Length && editor.Text[editor.Caret] == typed;
            _autoClosed = -1;
            if (hit)
            {
                editor.Select(editor.Caret + 1, editor.Caret + 1);
            }
            return hit;
        }

        private void OnTyped(char c)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!Settings.Current.AutoShowSuggestions)
            {
                if (_popup.IsOpen && (IsWordChar(c) || c == '.'))
                {
                    Trigger(false);
                }
                else
                {
                    _popup.Close();
                }
                return;
            }
            if (IsWordChar(c) || c == '.' || c == '[')
            {
                Trigger(false);
                return;
            }
            _popup.Close();
            if ((c == ' ' || c == '(') && TryGetText(out var text, out var caret))
            {
                var before = text.Substring(0, caret);
                if (c == '(')
                {
                    // The caret is after the "(" typed, or between it and the ")" inserted with it.
                    var open = before.LastIndexOf('(');
                    before = open < 0 ? "" : before.Substring(0, open);
                }
                before = before.TrimEnd();
                var word = new string(before.Reverse().TakeWhile(char.IsLetter).Reverse().ToArray());
                var last = before.Length == 0 ? '\0' : before[before.Length - 1];
                // A word only counts when it stands alone: "brand" does not end with the keyword AND.
                var alone = word.Length == before.Length || !IsWordChar(before[before.Length - word.Length - 1]);
                var words = c == ' ' ? TriggerAfterSpace : TriggerAfterParenthesis;
                if ((alone && words.Contains(word, StringComparer.OrdinalIgnoreCase)) || (c == ' ' && word.Length == 0 && TriggerAfterSpaceSymbols.IndexOf(last) >= 0))
                {
                    Trigger(false);
                }
            }
        }

        private bool TryGetText(out string text, out int caret)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            text = null;
            caret = 0;
            if (!IsSqlBuffer(out var lines)
                || lines.GetLastLineIndex(out var lastLine, out var lastIndex) != VSConstants.S_OK
                || lines.GetLineText(0, 0, lastLine, lastIndex, out text) != VSConstants.S_OK
                || _view.GetCaretPos(out var line, out var column) != VSConstants.S_OK
                || lines.GetPositionOfLineIndex(line, column, out caret) != VSConstants.S_OK)
            {
                return false;
            }
            return true;
        }

        private void Trigger(bool explicitRequest)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!TryGetText(out var text, out var caret))
            {
                return;
            }
            var target = ConnectionResolver.TryGetActive();
            if (target != null)
            {
                // Ctrl+Space also checks at once whether the objects changed since they were read.
                MetadataCache.EnsureLoaded(target, explicitRequest);
            }
            var key = target?.Key ?? "";
            var others = string.Join(",", OtherDatabases(target, text).Select(Json.Str));
            var version = ++_version;
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                try
                {
                    await TaskScheduler.Default;
                    // Typing outpaces the engine; only the newest request is worth answering.
                    if (version != _version)
                    {
                        return;
                    }
                    var reply = EngineProcess.Shared.Request("complete",
                        "\"key\":" + Json.Str(key) + ",\"others\":[" + others + "],\"text\":" + Json.Str(text) + ",\"caret\":" + caret + ",\"options\":" + Settings.Current.CompletionOptionsJson());
                    var parsed = reply == null ? null : Json.Parse<CompletionReply>(reply);
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    if (version != _version)
                    {
                        return;
                    }
                    if (parsed?.Error != null)
                    {
                        Log.Write("engine: " + parsed.Error);
                    }
                    Show(parsed?.Result, caret, explicitRequest);
                }
                catch (Exception ex)
                {
                    Log.Error("completion", ex);
                }
            }).FileAndForget("QuillSQL/Completion");
        }

        private static readonly System.Text.RegularExpressions.Regex Qualifier =
            new System.Text.RegularExpressions.Regex(@"(?:\[([^\]\r\n]+)\]|\b([A-Za-z_][\w@$#]*))\s*\.", System.Text.RegularExpressions.RegexOptions.Compiled);

        /// <summary>
        /// The keys of other databases of the server that the script names (`Archive.dbo.t`), loading
        /// their metadata the first time; those not loaded yet are left out until they are.
        /// </summary>
        private static IEnumerable<string> OtherDatabases(ConnectionTarget target, string text)
        {
            if (target == null || !target.CanQuery || !MetadataLoader.Databases.TryGetValue(target.Key, out var databases))
            {
                return Enumerable.Empty<string>();
            }
            var current = (target.Key.Split('|').ElementAtOrDefault(1) ?? "");
            var named = Qualifier.Matches(text).Cast<System.Text.RegularExpressions.Match>()
                .Select(m => m.Groups[1].Success ? m.Groups[1].Value : m.Groups[2].Value)
                .Distinct(StringComparer.OrdinalIgnoreCase)
                .Where(n => !string.Equals(n, current, StringComparison.OrdinalIgnoreCase) && databases.Contains(n, StringComparer.OrdinalIgnoreCase))
                .Take(5);
            var keys = new List<string>();
            foreach (var name in named)
            {
                var other = target.ForDatabase(databases.First(d => string.Equals(d, name, StringComparison.OrdinalIgnoreCase)));
                if (MetadataCache.EnsureLoaded(other))
                {
                    keys.Add(other.Key);
                }
            }
            return keys;
        }

        private void Show(CompletionResult result, int caret, bool explicitRequest)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var items = result?.Items;
            if (items == null || items.Length == 0 || !TryGetText(out var text, out var now) || now != caret)
            {
                _popup.Close();
                return;
            }
            var typed = text.Substring(result.Start, caret - result.Start);
            // Nothing to offer when the only suggestion is exactly what was typed. One that differs
            // only in case stays, so Tab or Enter turns `select` into SELECT and `customerid` into CustomerId.
            if (!explicitRequest && items.Length == 1 && string.Equals(items[0].Insert, typed, StringComparison.Ordinal))
            {
                _popup.Close();
                return;
            }
            _view.GetBuffer(out var lines);
            lines.GetLineIndexOfPosition(result.Start, out var line, out var column);
            var point = new POINT[1];
            _view.GetPointOfLineColumn(line, column, point);
            _view.GetLineHeight(out var lineHeight);
            var window = _view.GetWindowHandle();
            var screen = new NativePoint { X = point[0].x, Y = point[0].y + lineHeight };
            ClientToScreen(window, ref screen);
            _replaceStart = result.Start;
            // Select what the engine is sure of, or what was picked last time for the same letters;
            // a list of guesses opens with nothing selected. Ctrl+Space always selects.
            var select = explicitRequest ? 0 : result.Selected ?? -1;
            if (typed.Length > 0 && Picks.Last(typed) is string picked)
            {
                var at = Array.FindIndex(items, i => i.Insert == picked);
                if (at >= 0)
                {
                    select = at;
                }
            }
            var target = ConnectionResolver.TryGetActive();
            // Typed before the database's objects are read: say so at the top of the list.
            if (MetadataCache.IsLoading(target))
            {
                items = new[] { new CompletionItem { Label = Loc.T("Reading the database's objects..."), Insert = "", Kind = "loading", Detail = target.Description } }.Concat(items).ToArray();
                select = select >= 0 ? select + 1 : -1;
            }
            _popup.Show(items, window, screen.X, screen.Y, Settings.Current.ShowDefinitionBox, select, target);
            LastShown = string.Join(",", items.Take(20).Select(i => i.Label));
            Log.Verbose("suggest " + items.Length + ": " + LastShown);
        }

        /// <summary>
        /// Inserts the selected suggestion. `shaped` (Tab, Enter, a double click) writes what the word
        /// takes too, `IN ()` or `N''`, with the caret where the engine's `$CURSOR$` stands.
        /// </summary>
        private void Commit(bool shaped)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var item = _popup.Selected;
            var listed = _popup.Items.ToList();
            _popup.Close();
            if (item == null || item.Kind == "loading" || !TryGetText(out var before, out var caret) || caret < _replaceStart)
            {
                return;
            }
            if (item.Kind == "picker")
            {
                // Check the columns to insert; after `c.` each one gets the same qualifier.
                var chosen = ColumnPicker.Pick(listed.Where(i => i.Kind == "column").ToList());
                if (chosen == null)
                {
                    return;
                }
                var qualifier = System.Text.RegularExpressions.Regex.Match(before.Substring(0, _replaceStart), @"(?:(?:\[[^\]]+\]|[\w@#$]+)\.)+$").Value;
                item = new CompletionItem { Label = item.Label, Kind = "column", Insert = string.Join(", " + qualifier, chosen) };
            }
            else
            {
                Picks.Remember(before.Substring(_replaceStart, caret - _replaceStart), item.Insert);
                if (Settings.Current.LearnFromPicks)
                {
                    // Counted by the engine, for the order of the list next time.
                    var fields = "\"key\":" + Json.Str(ConnectionResolver.TryGetActive()?.Key ?? "") + ",\"kind\":" + Json.Str(item.Kind ?? "") + ",\"label\":" + Json.Str(item.Label ?? "");
                    System.Threading.Tasks.Task.Run(() => EngineProcess.Shared.Request("picked", fields));
                }
            }
            // `ALTER PROCEDURE name`: fill in the object's whole definition.
            var alter = System.Text.RegularExpressions.Regex.Match(before.Substring(0, _replaceStart), @"\bALTER\s+(PROC|PROCEDURE|VIEW|FUNCTION)\s+$", System.Text.RegularExpressions.RegexOptions.IgnoreCase);
            if (alter.Success && Settings.Current.GenerateAlter && (item.Kind == "procedure" || item.Kind == "view" || item.Kind == "function" || item.Kind == "table_function"))
            {
                var name = (item.Detail ?? "").Split(new[] { " (" }, StringSplitOptions.None)[0];
                ObjectScripts.ReplaceWithAlter(_view, alter.Index, caret, name);
                return;
            }
            if (item.Kind == "snippet")
            {
                var editor = EditorContext.For(_view);
                var reply = editor == null ? null : EngineProcess.Shared.Request("tab",
                    "\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + caret + ",\"start\":" + _replaceStart
                    + ",\"snippet\":" + Json.Str(item.Label) + ",\"env\":" + Commands.SnippetEnv(""));
                Commands.ApplyEdit(editor, reply == null ? null : Json.Parse<TabReply>(reply)?.Result);
                return;
            }
            const string marker = "$CURSOR$";
            var insert = item.Insert ?? "";
            if (insert.Contains(marker) && !shaped)
            {
                insert = item.Kind == "value" ? insert.Replace(marker, "") : item.Label;
            }
            var at = insert.IndexOf(marker, StringComparison.Ordinal);
            insert = insert.Replace(marker, "");
            _view.GetBuffer(out var lines);
            lines.GetLineIndexOfPosition(_replaceStart, out var startLine, out var startColumn);
            lines.GetLineIndexOfPosition(caret, out var endLine, out var endColumn);
            var buffer = Marshal.StringToCoTaskMemUni(insert);
            try
            {
                lines.ReplaceLines(startLine, startColumn, endLine, endColumn, buffer, insert.Length, null);
            }
            finally
            {
                Marshal.FreeCoTaskMem(buffer);
            }
            Log.Verbose("inserted " + insert);
            if (at >= 0)
            {
                EditorContext.For(_view)?.Select(_replaceStart + at, _replaceStart + at);
                return;
            }
            // Functions are inserted with their parentheses; put the caret between them.
            if (item.Insert.EndsWith("()", StringComparison.Ordinal) && (item.Kind == "function"))
            {
                var editor = EditorContext.For(_view);
                var end = _replaceStart + item.Insert.Length - 1;
                editor?.Select(end, end);
                UpdateSignature();
            }
        }

        /// <summary>True when a word with a lower-case letter (int, datetime2, Select) ends at `end`.</summary>
        private static bool EndsWithLowerCaseWord(string text, int end)
        {
            var lower = false;
            var i = end;
            while (i > 0 && (char.IsLetterOrDigit(text[i - 1]) || text[i - 1] == '_'))
            {
                lower |= char.IsLower(text[i - 1]);
                i--;
            }
            return end - i > 1 && lower;
        }

        /// <summary>Writes the keyword that ends at `end` of `text` in upper case, if it still reads the same.</summary>
        private void UppercaseKeyword(string text, int end, char next)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var sent = text.Substring(0, end);
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                try
                {
                    await TaskScheduler.Default;
                    var reply = EngineProcess.Shared.Request("keyword_case", "\"text\":" + Json.Str(sent) + ",\"caret\":" + end + ",\"next\":" + Json.Str(next.ToString()));
                    var found = reply == null ? null : Json.Parse<KeywordCaseReply>(reply)?.Result;
                    if (found == null)
                    {
                        return;
                    }
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    // Typed on or edited meanwhile: change it only where the same word still stands. Case
                    // is ignored: the keyword before it may just have been written in upper case.
                    if (!TryGetText(out var now, out _) || now.Length < found.End
                        || string.Compare(now, found.Start, sent, found.Start, found.End - found.Start, StringComparison.OrdinalIgnoreCase) != 0
                        || string.Compare(now, 0, sent, 0, found.Start, StringComparison.OrdinalIgnoreCase) != 0)
                    {
                        return;
                    }
                    _view.GetBuffer(out var lines);
                    lines.GetLineIndexOfPosition(found.Start, out var startLine, out var startColumn);
                    lines.GetLineIndexOfPosition(found.End, out var endLine, out var endColumn);
                    var buffer = Marshal.StringToCoTaskMemUni(found.Text);
                    try
                    {
                        lines.ReplaceLines(startLine, startColumn, endLine, endColumn, buffer, found.Text.Length, null);
                    }
                    finally
                    {
                        Marshal.FreeCoTaskMem(buffer);
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("keyword case", ex);
                }
            }).FileAndForget("QuillSQL/KeywordCase");
        }

        private bool LineStartsWithExec()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!TryGetText(out var text, out var caret))
            {
                return false;
            }
            var lineStart = text.LastIndexOf('\n', Math.Max(0, caret - 1)) + 1;
            var line = text.Substring(lineStart, caret - lineStart).TrimStart();
            return line.StartsWith("EXEC", StringComparison.OrdinalIgnoreCase);
        }

        /// <summary>Shows, moves or closes the parameter hints for the call around the caret.</summary>
        private void UpdateSignature()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!Settings.Current.ParameterHints || !TryGetText(out var text, out var caret))
            {
                _signature.Close();
                return;
            }
            var key = ConnectionResolver.TryGetActive()?.Key ?? "";
            var version = ++_signatureVersion;
            ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                try
                {
                    await TaskScheduler.Default;
                    var reply = EngineProcess.Shared.Request("signature", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + caret);
                    var signature = reply == null ? null : Json.Parse<SignatureReply>(reply)?.Result;
                    await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                    if (version != _signatureVersion)
                    {
                        return;
                    }
                    // Typed on while the engine answered: ask again for where the caret is now.
                    if (TryGetText(out _, out var moved) && moved != caret)
                    {
                        UpdateSignature();
                        return;
                    }
                    Log.Verbose("parameter hints at " + caret + ": " + (signature?.Name ?? "none"));
                    if (signature == null || _view.GetCaretPos(out var line, out var column) != VSConstants.S_OK)
                    {
                        _signature.Close();
                        return;
                    }
                    var point = new POINT[1];
                    _view.GetPointOfLineColumn(line, column, point);
                    var window = _view.GetWindowHandle();
                    var screen = new NativePoint { X = point[0].x, Y = point[0].y };
                    ClientToScreen(window, ref screen);
                    _signature.Show(signature, window, screen.X, screen.Y);
                }
                catch (Exception ex)
                {
                    Log.Error("parameter hints", ex);
                }
            }).FileAndForget("QuillSQL/Signature");
        }

        public void OnSetFocus(IVsTextView view) { }

        public void OnKillFocus(IVsTextView view)
        {
            if (_signature.IsOpen)
            {
                Log.Verbose("parameter hints closed: focus lost");
            }
            // A click in the list or its card takes the focus for a moment; the list stays open.
            if (!_popup.IsMouseOver)
            {
                _popup.Close();
            }
            _signature.Close();
        }

        public void OnSetBuffer(IVsTextView view, IVsTextLines buffer)
        {
            _popup.Close();
            _signature.Close();
        }

        public void OnChangeScrollInfo(IVsTextView view, int bar, int minUnit, int maxUnits, int visibleUnits, int firstVisibleUnit)
        {
            // Typing a longer line changes the horizontal range without scrolling; only a move of the
            // view puts the list and the tip in the wrong place.
            if (bar < 0 || bar > 1 || _firstVisible[bar] == firstVisibleUnit)
            {
                return;
            }
            _firstVisible[bar] = firstVisibleUnit;
            if (_signature.IsOpen)
            {
                Log.Verbose("parameter hints closed: scrolled (bar " + bar + ")");
            }
            _popup.Close();
            _signature.Close();
        }

        public void OnChangeCaretLine(IVsTextView view, int newLine, int oldLine)
        {
            _popup.Close();
            _signature.Close();
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct NativePoint
        {
            public int X;
            public int Y;
        }

        [DllImport("user32.dll")]
        private static extern bool ClientToScreen(IntPtr hwnd, ref NativePoint point);
    }
}
