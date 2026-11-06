using System;
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
        };

        /// <summary>Characters after which a space opens the suggestions: comparisons and lists.</summary>
        private const string TriggerAfterSpaceSymbols = "=<>,(+-*/";

        private readonly IVsTextView _view;
        private readonly CompletionPopup _popup = new CompletionPopup();
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
            _popup.Committed += Commit;
        }

        public static CompletionController Attach(IVsTextView view)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var controller = new CompletionController(view);
            if (view.AddCommandFilter(controller, out controller._next) != VSConstants.S_OK)
            {
                return null;
            }
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
            _popup.Close();
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
                            if (command == VSConstants.VSStd2KCmdID.TAB ? Settings.Current.InsertWithTab : Settings.Current.InsertWithEnter)
                            {
                                Commit();
                                return VSConstants.S_OK;
                            }
                            _popup.Close();
                            break;
                        case VSConstants.VSStd2KCmdID.TYPECHAR:
                            // Optional keys that insert the suggestion and then type themselves.
                            var typedChar = (char)Convert.ToUInt16(Marshal.GetObjectForNativeVariant(input));
                            var settings = Settings.Current;
                            if ((typedChar == ' ' && settings.InsertWithSpace) || (typedChar == '.' && settings.InsertWithDot)
                                || (typedChar == ',' && settings.InsertWithComma) || (typedChar == '(' && settings.InsertWithParenthesis))
                            {
                                Commit();
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

            var result = _next.Exec(ref group, id, options, input, output);

            try
            {
                if (typed != '\0')
                {
                    if (closing != null)
                    {
                        InsertClosing(closing);
                    }
                    OnTyped(typed);
                }
                else if (std2k && _popup.IsOpen && (command == VSConstants.VSStd2KCmdID.BACKSPACE || command == VSConstants.VSStd2KCmdID.DELETE))
                {
                    Trigger(false);
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
            var hit = editor != null && editor.Caret == _autoClosed && editor.Caret < editor.Text.Length && editor.Text[editor.Caret] == typed;
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
            if (c == ' ' && TryGetText(out var text, out var caret))
            {
                var before = text.Substring(0, caret).TrimEnd();
                var word = new string(before.Reverse().TakeWhile(char.IsLetter).Reverse().ToArray());
                var last = before.Length == 0 ? '\0' : before[before.Length - 1];
                // A word only counts when it stands alone: "brand" does not end with the keyword AND.
                var alone = word.Length == before.Length || !IsWordChar(before[before.Length - word.Length - 1]);
                if ((alone && TriggerAfterSpace.Contains(word, StringComparer.OrdinalIgnoreCase)) || (word.Length == 0 && TriggerAfterSpaceSymbols.IndexOf(last) >= 0))
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
                MetadataCache.EnsureLoaded(target);
            }
            var key = target?.Key ?? "";
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
                        "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text) + ",\"caret\":" + caret + ",\"options\":" + Settings.Current.CompletionOptionsJson());
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
            // Nothing to offer when the only suggestion is exactly what was typed.
            if (!explicitRequest && items.Length == 1 && string.Equals(items[0].Insert, typed, StringComparison.OrdinalIgnoreCase))
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
            _popup.Show(items, window, screen.X, screen.Y, Settings.Current.ShowDefinitionBox);
            LastShown = string.Join(",", items.Take(20).Select(i => i.Label));
            Log.Verbose("suggest " + items.Length + ": " + LastShown);
        }

        private void Commit()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var item = _popup.Selected;
            _popup.Close();
            if (item == null || !TryGetText(out var before, out var caret) || caret < _replaceStart)
            {
                return;
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
            _view.GetBuffer(out var lines);
            lines.GetLineIndexOfPosition(_replaceStart, out var startLine, out var startColumn);
            lines.GetLineIndexOfPosition(caret, out var endLine, out var endColumn);
            var buffer = Marshal.StringToCoTaskMemUni(item.Insert);
            try
            {
                lines.ReplaceLines(startLine, startColumn, endLine, endColumn, buffer, item.Insert.Length, null);
            }
            finally
            {
                Marshal.FreeCoTaskMem(buffer);
            }
            Log.Verbose("inserted " + item.Insert);
            // Functions are inserted with their parentheses; put the caret between them.
            if (item.Insert.EndsWith("()", StringComparison.Ordinal) && (item.Kind == "function"))
            {
                var editor = EditorContext.For(_view);
                var end = _replaceStart + item.Insert.Length - 1;
                editor?.Select(end, end);
            }
        }

        public void OnSetFocus(IVsTextView view) { }

        public void OnKillFocus(IVsTextView view) => _popup.Close();

        public void OnSetBuffer(IVsTextView view, IVsTextLines buffer) => _popup.Close();

        public void OnChangeScrollInfo(IVsTextView view, int bar, int minUnit, int maxUnits, int visibleUnits, int firstVisibleUnit) => _popup.Close();

        public void OnChangeCaretLine(IVsTextView view, int newLine, int oldLine) => _popup.Close();

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
