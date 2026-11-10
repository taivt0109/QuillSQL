using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Runtime.Serialization;
using System.Text;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Microsoft.Data.SqlClient;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    [DataContract]
    internal sealed class ProblemsReply
    {
        [DataMember(Name = "result")] public ProblemsResult Result { get; set; }
    }

    [DataContract]
    internal sealed class ProblemsResult
    {
        [DataMember(Name = "problems")] public List<string> Problems { get; set; }
    }

    /// <summary>
    /// Edits Claude makes to a script, checked before they reach the editor: the SQL is checked
    /// against the database's tables and columns and sent back once to correct what it names
    /// wrongly, then shown beside the text it replaces, to apply or not.
    /// </summary>
    internal static class AiEdits
    {
        private const string DescribePrompt =
            "You write the MS_Description of a SQL Server table or view and of its columns, for the developers who use it. Reply with one line " +
            "per description and nothing else: `@table: ...` for the table, then `ColumnName: ...` for each column asked for. One or two " +
            "concrete sentences each: what a row is; what the column holds, its unit or allowed values, and how it relates to other tables. " +
            "Use only what the definition and the sample rows show; when the meaning is a guess, say 'probably'.";

        /// <summary>What the SQL names that the database does not have, by the engine's check.</summary>
        internal static List<string> Problems(string key, string text)
        {
            if (string.IsNullOrEmpty(key))
            {
                return new List<string>();
            }
            var reply = EngineProcess.Shared.Request("check_sql", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(text));
            return (reply == null ? null : Json.Parse<ProblemsReply>(reply)?.Result?.Problems) ?? new List<string>();
        }

        /// <summary>
        /// Asks for SQL, checks the script it makes, and once asks again with what it named wrongly.
        /// `whole` is the script with the answer in place; problems the script had before do not count.
        /// </summary>
        private static string AskChecked(string input, string key, string original, Func<string, string> whole, out List<string> problems)
        {
            var known = Problems(key, original);
            List<string> check(string sql) => Problems(key, whole(sql)).Where(p => !known.Contains(p)).ToList();
            var sql = AiAssistant.StripFence(AiAssistant.Ask(AiAssistant.SqlOnly, input));
            problems = check(sql);
            if (problems.Count == 0)
            {
                return sql;
            }
            Log.Write("Claude's SQL names what the database does not have: " + string.Join("; ", problems));
            var again = AiAssistant.StripFence(AiAssistant.Ask(AiAssistant.SqlOnly, input
                + "\n\nYour answer was:\n" + sql
                + "\n\nIt names what the database does not have:\n- " + string.Join("\n- ", problems)
                + "\nUse only the tables and columns of the schema. Reply with the corrected SQL only."));
            var left = check(again);
            if (left.Count <= problems.Count)
            {
                problems = left;
                return again;
            }
            return sql;
        }

        /// <summary>
        /// Asks Claude for the SQL that replaces [start, end) of the editor's script (or goes at the
        /// caret when they are equal), checks it, and shows it to apply or not.
        /// </summary>
        public static void Edit(AsyncPackage package, string what, string title, EditorContext editor, int start, int end, string input, string key, string done)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var text = editor.Text;
            var inserting = start == end;
            string place(string sql) => inserting ? (start > 0 && text[start - 1] != '\n' ? Environment.NewLine : "") + sql + Environment.NewLine : sql;
            string whole(string sql) => text.Substring(0, start) + place(sql) + text.Substring(end);
            List<string> problems = null;
            AiAssistant.Start(package, what, () =>
            {
                var sql = AskChecked(input, key, text, whole, out var left);
                problems = left;
                return sql;
            }, answer =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                var now = EditorContext.For(editor.View);
                if (now == null || now.Text != text)
                {
                    EditorContext.Status("the script changed while Claude was answering; nothing was changed.");
                    Commands.WriteOutput(new[] { "Claude's answer (not applied):", answer });
                    return;
                }
                var replacement = place(answer);
                if (replacement == text.Substring(start, end - start))
                {
                    EditorContext.Status("Claude found nothing to change.");
                    return;
                }
                if (!Review(title, text.Substring(start, end - start), replacement, problems ?? new List<string>()))
                {
                    EditorContext.Status("nothing was changed.");
                    return;
                }
                now.Replace(start, end, replacement);
                now.Select(start, start + replacement.Length);
                EditorContext.Status(done);
            });
        }

        // ---- Review ----

        /// <summary>Set by the end-to-end test: the review window applies the change as soon as it shows.</summary>
        internal static bool AutoApply;

        /// <summary>What the last review window showed, for the end-to-end test.</summary>
        internal static string LastReview;

        private static readonly Brush Removed =Theme.Frozen(Color.FromArgb(0x48, 0xD9, 0x3B, 0x3B));
        private static readonly Brush Added = Theme.Frozen(Theme.Tint(Theme.Green, 0x48));

        /// <summary>Shows the new SQL beside the text it replaces; true when the user applies it.</summary>
        public static bool Review(string title, string before, string after, IList<string> problems)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var window = Theme.Dialog(Loc.T(title), true);
            window.Width = 1100;
            window.Height = 680;
            var top = new StackPanel { Margin = new Thickness(16, 12, 16, 10) };
            if (problems.Count > 0)
            {
                var list = new StackPanel();
                list.Children.Add(new TextBlock { Text = Loc.T("Claude's SQL still names what the database does not have:"), FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
                foreach (var p in problems.Take(8))
                {
                    list.Children.Add(new TextBlock { Text = "• " + p, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 2, 0, 0) });
                }
                var pill = Theme.Pill(list, Theme.Amber);
                pill.Padding = new Thickness(10, 7, 10, 7);
                pill.HorizontalAlignment = HorizontalAlignment.Stretch;
                top.Children.Add(pill);
            }
            else
            {
                top.Children.Add(Theme.Dim(Loc.T(before.Length == 0
                    ? "Claude's SQL, checked against the database's tables and columns. Insert puts it at the caret; Undo removes it."
                    : "Claude's change, checked against the database's tables and columns: the lines it removes are red on the left, the new ones green on the right. Undo reverts it.")));
            }
            var scroll = new ScrollViewer { Content = DiffView(before, after), HorizontalScrollBarVisibility = ScrollBarVisibility.Auto, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Padding = new Thickness(16, 0, 16, 8) };
            var apply = Theme.DialogButton(Loc.T(before.Length == 0 ? "Insert" : "Apply"), isDefault: true);
            var cancel = Theme.DialogButton(Loc.T("Cancel"), isCancel: true);
            apply.Click += (s, e) => window.DialogResult = true;
            var root = new DockPanel();
            var footer = Theme.Footer(null, apply, cancel);
            DockPanel.SetDock(footer, Dock.Bottom);
            root.Children.Add(footer);
            DockPanel.SetDock(top, Dock.Top);
            root.Children.Add(top);
            root.Children.Add(scroll);
            window.Content = root;
            EditorContext.SetOwner(window);
            window.Loaded += (s, e) => apply.Focus();
            if (AutoApply)
            {
                // The end-to-end test: the window shows, then applies itself; what it showed is kept.
                LastReview = (problems.Count == 0 ? "" : "problems: " + string.Join("; ", problems) + "\n") + "before:\n" + before + "\nafter:\n" + after;
                window.Loaded += (s, e) => window.Dispatcher.BeginInvoke(new Action(() => window.DialogResult = true));
            }
            return window.ShowModal() == true;
        }

        /// <summary>The two texts side by side, line against line: removed lines red, new lines green.</summary>
        private static UIElement DiffView(string before, string after)
        {
            var rows = Align(SplitLines(before), SplitLines(after));
            var grid = new Grid();
            grid.SetValue(System.Windows.Documents.TextElement.FontFamilyProperty, Theme.Mono);
            var sides = before.Length == 0 ? 1 : 2;
            for (var side = 0; side < sides; side++)
            {
                grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star), MinWidth = 200 });
            }
            int left = 0, right = 0;
            for (var r = 0; r < rows.Count; r++)
            {
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                var (a, b) = rows[r];
                void cell(int column, string line, int number, Brush background)
                {
                    var num = new TextBlock { Text = line == null ? "" : number.ToString(CultureInfo.InvariantCulture), Opacity = 0.45, Padding = new Thickness(6, 0, 8, 0), TextAlignment = TextAlignment.Right, Background = background };
                    var body = new TextBlock { Text = line ?? "", Padding = new Thickness(0, 0, 12, 0), Background = background };
                    Grid.SetRow(num, r);
                    Grid.SetColumn(num, column);
                    Grid.SetRow(body, r);
                    Grid.SetColumn(body, column + 1);
                    grid.Children.Add(num);
                    grid.Children.Add(body);
                }
                var same = a != null && b != null && a == b;
                if (sides == 2)
                {
                    if (a != null) left++;
                    cell(0, a, left, same || a == null ? null : Removed);
                }
                if (b != null) right++;
                cell(sides == 2 ? 2 : 0, b, right, same || b == null || sides == 1 ? null : Added);
            }
            return grid;
        }

        private static string[] SplitLines(string text) => text.Length == 0 ? new string[0] : text.Replace("\r\n", "\n").TrimEnd('\n').Split('\n');

        /// <summary>
        /// Lines of the two texts in rows: the same line on both sides, or a run of removed lines
        /// beside the run of new lines that replaces it (a side null where it has none).
        /// </summary>
        private static List<(string, string)> Align(string[] a, string[] b)
        {
            var rows = new List<(string, string)>();
            int prefix = 0, suffix = 0;
            while (prefix < a.Length && prefix < b.Length && a[prefix] == b[prefix]) prefix++;
            while (suffix < a.Length - prefix && suffix < b.Length - prefix && a[a.Length - 1 - suffix] == b[b.Length - 1 - suffix]) suffix++;
            for (var i = 0; i < prefix; i++) rows.Add((a[i], b[i]));
            int n = a.Length - prefix - suffix, m = b.Length - prefix - suffix;
            // The longest common lines of the middle; too large a middle is shown as replaced whole.
            var lcs = (long)n * m <= 4_000_000 ? new int[n + 1, m + 1] : null;
            if (lcs != null)
            {
                for (var i = n - 1; i >= 0; i--)
                    for (var j = m - 1; j >= 0; j--)
                        lcs[i, j] = a[prefix + i] == b[prefix + j] ? lcs[i + 1, j + 1] + 1 : Math.Max(lcs[i + 1, j], lcs[i, j + 1]);
            }
            var removed = new List<string>();
            var added = new List<string>();
            void flush()
            {
                for (var k = 0; k < Math.Max(removed.Count, added.Count); k++)
                {
                    rows.Add((k < removed.Count ? removed[k] : null, k < added.Count ? added[k] : null));
                }
                removed.Clear();
                added.Clear();
            }
            int x = 0, y = 0;
            while (x < n || y < m)
            {
                if (lcs != null && x < n && y < m && a[prefix + x] == b[prefix + y])
                {
                    flush();
                    rows.Add((a[prefix + x], b[prefix + y]));
                    x++;
                    y++;
                }
                else if (y >= m || (x < n && (lcs == null || lcs[x + 1, y] >= lcs[x, y + 1])))
                {
                    removed.Add(a[prefix + x++]);
                }
                else
                {
                    added.Add(b[prefix + y++]);
                }
            }
            flush();
            for (var i = suffix; i > 0; i--) rows.Add((a[a.Length - i], b[b.Length - i]));
            return rows;
        }

        // ---- Fix with AI ----

        /// <summary>The document view of the active window.</summary>
        internal static object ActiveDocView()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            return Package.GetGlobalService(typeof(SVsShellMonitorSelection)) is IVsMonitorSelection monitor
                && monitor.GetCurrentElementValue((uint)VSConstants.VSSELELEMID.SEID_DocumentFrame, out var frame) == VSConstants.S_OK
                && frame is IVsWindowFrame window
                && window.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView) == VSConstants.S_OK ? docView : null;
        }

        /// <summary>
        /// The text of a query window's Messages tab (or its results, when they go to text). SSMS keeps
        /// it in non-public members of the editor, so every step is defensive; null when not found.
        /// </summary>
        internal static string Messages(object docView)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var results = ConnectionResolver.Get(docView, "m_sqlResultsControl") ?? ConnectionResolver.Get(docView, "SqlResultsControl");
                var texts = new List<string>();
                foreach (var name in new[] { "m_textMessagesPage", "m_textResultsPage" })
                {
                    var page = ConnectionResolver.Get(results, name);
                    if (ConnectionResolver.Get(ConnectionResolver.Get(page, "TextView"), "TextView") is IVsTextView view
                        && view.GetBuffer(out var lines) == VSConstants.S_OK
                        && lines.GetLastLineIndex(out var last, out var index) == VSConstants.S_OK
                        && lines.GetLineText(0, 0, last, index, out var text) == VSConstants.S_OK)
                    {
                        texts.Add(text);
                    }
                }
                if (texts.Count == 0)
                {
                    Log.Write("no Messages tab on " + docView?.GetType().FullName + " (results control: " + results?.GetType().FullName + ")");
                    return null;
                }
                return string.Join("\n", texts);
            }
            catch (Exception ex)
            {
                Log.Error("reading the Messages tab", ex);
                return null;
            }
        }

        /// <summary>The errors in a Messages text: `Msg 207, Level 16, State 1, Line 3` and what follows it.</summary>
        internal static string Errors(string messages)
        {
            if (string.IsNullOrEmpty(messages))
            {
                return null;
            }
            var lines = messages.Replace("\r\n", "\n").Split('\n');
            var errors = new List<string>();
            for (var i = 0; i < lines.Length && errors.Count < 10; i++)
            {
                var m = Regex.Match(lines[i], @"^Msg \d+, Level (\d+), State \d+");
                if (!m.Success || int.Parse(m.Groups[1].Value, CultureInfo.InvariantCulture) < 11)
                {
                    continue;
                }
                var error = new StringBuilder(lines[i].Trim());
                for (var j = i + 1; j < lines.Length && lines[j].Trim().Length > 0 && !lines[j].StartsWith("Msg ", StringComparison.Ordinal); j++)
                {
                    error.Append('\n').Append(lines[j].Trim());
                }
                errors.Add(error.ToString());
            }
            return errors.Count == 0 ? null : string.Join("\n", errors);
        }

        /// <summary>Fixes the errors the last run of the active window raised, from its Messages tab.</summary>
        public static void FixErrors(AsyncPackage package) => Commands.Run("Fix with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var errors = Errors(Messages(ActiveDocView()));
            if (errors == null)
            {
                EditorContext.Status("no error in this window's Messages tab: run the query, then fix it with AI.");
                return;
            }
            var key = Commands.MetadataKey(out _);
            int start = editor.HasSelection ? editor.SelectionStart : 0, end = editor.HasSelection ? editor.SelectionEnd : editor.Text.Length;
            var input = "SQL Server raised these errors running the SQL below:\n" + errors
                + "\n\nSchema:\n" + AiAssistant.Context(editor, key)
                + "\n\nFix what causes them. Reply with the whole SQL below, corrected and otherwise unchanged; a short SQL comment at each fix says what was wrong. "
                + "Line numbers in the errors count from the start of each batch (GO).\n"
                + editor.Text.Substring(start, end - start);
            Edit(package, "fix the errors", "Fix with AI", editor, start, end, input, key, "Claude's fix applied; review it, then run the query again. Undo reverts it.");
        });

        /// <summary>
        /// Fixes the code analysis issues with Claude: the one underlined at the caret (in its statement),
        /// those in the selection, or else all of the script's.
        /// </summary>
        public static void FixIssues(AsyncPackage package) => Commands.Run("Fix code issues with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            if (editor == null)
            {
                EditorContext.Status("open a query window first.");
                return;
            }
            var key = Commands.MetadataKey(out _);
            var issues = CodeAnalysis.Analyze(editor.Text, key);
            if (issues == null)
            {
                EditorContext.Status("the engine did not answer.");
                return;
            }
            int start = 0, end = editor.Text.Length;
            var chosen = issues.ToList();
            var at = issues.Where(i => i.Start <= editor.Caret && editor.Caret <= i.End).ToList();
            if (editor.HasSelection)
            {
                start = editor.SelectionStart;
                end = editor.SelectionEnd;
                chosen = issues.Where(i => i.Start >= start && i.End <= end).ToList();
            }
            else if (at.Count > 0)
            {
                var reply = EngineProcess.Shared.Request("tool", "\"action\":\"statement_range\",\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret);
                var range = reply == null ? null : Json.Parse<RangeReply>(reply)?.Result;
                if (range != null && range.Start <= at[0].Start && at[0].End <= range.End)
                {
                    start = range.Start;
                    end = range.End;
                }
                chosen = issues.Where(i => i.Start >= start && i.End <= end).ToList();
            }
            if (chosen.Count == 0)
            {
                EditorContext.Status(editor.HasSelection ? "code analysis finds nothing to fix in the selection." : "code analysis finds nothing to fix.");
                return;
            }
            editor.Lines.GetLineIndexOfPosition(start, out var firstLine, out _);
            var list = chosen.Select(i =>
            {
                editor.Lines.GetLineIndexOfPosition(i.Start, out var line, out _);
                return "- line " + (line - firstLine + 1) + ": " + i.Code + " " + i.Message;
            });
            var input = "Code analysis reports these issues in the SQL below:\n" + string.Join("\n", list)
                + "\n\nSchema:\n" + AiAssistant.Context(editor, key)
                + "\n\nRewrite the SQL to fix them without changing what it does; leave an issue as it is when fixing it would change the result, "
                + "with a short SQL comment saying why. Reply with the whole SQL below, otherwise unchanged.\n"
                + editor.Text.Substring(start, end - start);
            Edit(package, "fix the code issues", "Fix Code Issues with AI", editor, start, end, input, key, "Claude's fixes applied; review them. Undo reverts them.");
        });

        // ---- Document with AI ----

        /// <summary>Describes the table or view at the caret (or one asked for) with Claude, as a script to review and run.</summary>
        public static void Document(AsyncPackage package) => Commands.Run("Document with AI", () =>
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            var target = ConnectionResolver.TryGetActive();
            if (editor == null || target == null || !target.CanQuery)
            {
                EditorContext.Status("open a connected query window first.");
                return;
            }
            var key = Commands.MetadataKey(out _);
            var reply = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret);
            var at = reply == null ? null : Json.Parse<ObjectAtReply>(reply)?.Result;
            var name = at != null && (at.Kind == "table" || at.Kind == "view") ? at.Schema + "." + at.Name
                : EditorContext.Prompt("Document with AI", "Which table or view should Claude describe? (schema.name; or put the caret on its name)", "");
            if (string.IsNullOrWhiteSpace(name))
            {
                return;
            }
            DocumentTable(package, target, name.Trim(), EditorContext.OpenInNewQuery);
        });

        /// <summary>Asks Claude for the descriptions of `name` and hands the script that sets them to `deliver`.</summary>
        internal static void DocumentTable(AsyncPackage package, ConnectionTarget target, string name, Action<string> deliver)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var withRows = AiAssistant.LoadSettings().AllowData;
            AiAssistant.Start(package, "describe " + name, () =>
            {
                var table = ReadTable(target, name, withRows);
                var wanted = table.Columns.Where(c => string.IsNullOrEmpty(c.Description)).ToList();
                var all = wanted.Count == 0 && !string.IsNullOrEmpty(table.Description);
                if (all)
                {
                    // Everything is described already: describe it all again, better.
                    wanted = table.Columns;
                }
                var input = table.Prompt + "\n\nDescribe " + (all || string.IsNullOrEmpty(table.Description) ? "the " + table.Kind.ToLowerInvariant() + " (@table) and " : "")
                    + "these columns: " + string.Join(", ", wanted.Select(c => c.Name)) + ".";
                var language = AiChat.Language();
                var answer = AiAssistant.Ask(DescribePrompt + " Write in " + language + ".", input);
                return Script(table, Parse(answer, table), withRows && table.Sample != null);
            }, script =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                deliver(script);
                EditorContext.Status("review the descriptions, then run the script to save them.");
            });
        }

        private sealed class TableColumn
        {
            public string Name, Type, Description, References;
            public bool Nullable, Key, Identity;
        }

        private sealed class TableInfo
        {
            public string Schema, Name, Kind, Description, Prompt, Sample;
            public List<TableColumn> Columns = new List<TableColumn>();
            public List<string> ReferencedBy = new List<string>();
        }

        private static TableInfo ReadTable(ConnectionTarget target, string name, bool withRows)
        {
            var table = new TableInfo();
            using (var connection = target.CreateConnection())
            {
                connection.Open();
                using (var command = new SqlCommand(@"
DECLARE @id int = OBJECT_ID(@t);
SELECT OBJECT_SCHEMA_NAME(o.object_id), o.name, o.type, CONVERT(nvarchar(4000), ep.value),
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1))
FROM sys.objects AS o
LEFT JOIN sys.extended_properties AS ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0 AND ep.name = 'MS_Description'
WHERE o.object_id = @id AND o.type IN ('U', 'V');
SELECT c.name, TYPE_NAME(c.user_type_id), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, CONVERT(nvarchar(4000), ep.value),
    CASE WHEN EXISTS (SELECT 1 FROM sys.index_columns AS ic JOIN sys.indexes AS i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
                      WHERE i.is_primary_key = 1 AND ic.object_id = c.object_id AND ic.column_id = c.column_id) THEN 1 ELSE 0 END,
    (SELECT TOP (1) OBJECT_SCHEMA_NAME(f.referenced_object_id) + '.' + OBJECT_NAME(f.referenced_object_id) + '.' + COL_NAME(f.referenced_object_id, f.referenced_column_id)
     FROM sys.foreign_key_columns AS f WHERE f.parent_object_id = c.object_id AND f.parent_column_id = c.column_id)
FROM sys.columns AS c
LEFT JOIN sys.extended_properties AS ep ON ep.class = 1 AND ep.major_id = c.object_id AND ep.minor_id = c.column_id AND ep.name = 'MS_Description'
WHERE c.object_id = @id ORDER BY c.column_id;
SELECT DISTINCT OBJECT_SCHEMA_NAME(parent_object_id) + '.' + OBJECT_NAME(parent_object_id) FROM sys.foreign_keys WHERE referenced_object_id = @id;", connection) { CommandTimeout = 30 })
                {
                    command.Parameters.AddWithValue("@t", name);
                    using (var r = command.ExecuteReader())
                    {
                        if (!r.Read())
                        {
                            throw new InvalidOperationException("No table or view " + name + " in this database.");
                        }
                        table.Schema = r.GetString(0);
                        table.Name = r.GetString(1);
                        table.Kind = r.GetString(2).Trim() == "V" ? "VIEW" : "TABLE";
                        table.Description = r.IsDBNull(3) ? null : r.GetString(3);
                        var rows = r.IsDBNull(4) ? 0 : Convert.ToInt64(r.GetValue(4), CultureInfo.InvariantCulture);
                        r.NextResult();
                        while (r.Read())
                        {
                            table.Columns.Add(new TableColumn
                            {
                                Name = r.GetString(0),
                                Type = MetadataLoader.TypeText(r.GetString(1), r.GetInt16(2), r.GetByte(3), r.GetByte(4)),
                                Nullable = r.GetBoolean(5),
                                Identity = r.GetBoolean(6),
                                Description = r.IsDBNull(7) ? null : r.GetString(7),
                                Key = r.GetInt32(8) == 1,
                                References = r.IsDBNull(9) ? null : r.GetString(9),
                            });
                        }
                        r.NextResult();
                        while (r.Read())
                        {
                            table.ReferencedBy.Add(r.GetString(0));
                        }
                        var prompt = new StringBuilder();
                        prompt.Append(table.Kind).Append(' ').Append(table.Schema).Append('.').Append(table.Name).Append(", about ").Append(rows.ToString("N0", CultureInfo.InvariantCulture)).Append(" rows");
                        prompt.Append(table.Description == null ? "\n" : "\nIts description: " + table.Description + "\n");
                        foreach (var c in table.Columns)
                        {
                            prompt.Append("  ").Append(c.Name).Append(' ').Append(c.Type).Append(c.Nullable ? " NULL" : " NOT NULL")
                                .Append(c.Key ? " PRIMARY KEY" : "").Append(c.Identity ? " IDENTITY" : "")
                                .Append(c.References == null ? "" : " REFERENCES " + c.References)
                                .Append(c.Description == null ? "" : " -- " + c.Description).Append('\n');
                        }
                        if (table.ReferencedBy.Count > 0)
                        {
                            prompt.Append("Referenced by: ").Append(string.Join(", ", table.ReferencedBy)).Append('\n');
                        }
                        table.Prompt = prompt.ToString();
                    }
                }
                if (withRows)
                {
                    table.Sample = SampleRows(connection, table);
                    if (table.Sample != null)
                    {
                        table.Prompt += "Five sample rows:\n" + table.Sample;
                    }
                }
            }
            return table;
        }

        /// <summary>Five rows, each value cut to 60 characters, read without waiting on writers.</summary>
        private static string SampleRows(SqlConnection connection, TableInfo table)
        {
            try
            {
                var quoted = "[" + table.Schema.Replace("]", "]]") + "].[" + table.Name.Replace("]", "]]") + "]";
                using (var command = new SqlCommand("SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; SELECT TOP (5) * FROM " + quoted + ";", connection) { CommandTimeout = 15 })
                using (var r = command.ExecuteReader())
                {
                    var sb = new StringBuilder();
                    while (r.Read())
                    {
                        var values = new List<string>();
                        for (var i = 0; i < r.FieldCount; i++)
                        {
                            var v = r.IsDBNull(i) ? "NULL" : Convert.ToString(r.GetValue(i), CultureInfo.InvariantCulture) ?? "";
                            values.Add(r.GetName(i) + "=" + (v.Length > 60 ? v.Substring(0, 60) + "..." : v));
                        }
                        sb.Append("  ").Append(string.Join("; ", values)).Append('\n');
                    }
                    return sb.Length == 0 ? null : sb.ToString();
                }
            }
            catch (SqlException ex)
            {
                Log.Write("sample rows of " + table.Name + ": " + ex.Message);
                return null;
            }
        }

        /// <summary>The descriptions in Claude's answer: `@table: ...` and `Column: ...` lines, by column name ("" for the table).</summary>
        private static Dictionary<string, string> Parse(string answer, TableInfo table)
        {
            var found = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            foreach (var raw in AiAssistant.StripFence(answer).Replace("\r\n", "\n").Split('\n'))
            {
                var line = raw.Trim().TrimStart('-', '*', ' ').Trim();
                var colon = line.IndexOf(':');
                if (colon <= 0)
                {
                    continue;
                }
                var name = line.Substring(0, colon).Trim().Trim('`', '[', ']', '*', '"');
                var text = line.Substring(colon + 1).Trim();
                if (text.Length == 0)
                {
                    continue;
                }
                if (string.Equals(name, "@table", StringComparison.OrdinalIgnoreCase))
                {
                    found[""] = text;
                }
                else if (table.Columns.Any(c => string.Equals(c.Name, name, StringComparison.OrdinalIgnoreCase)))
                {
                    found[table.Columns.First(c => string.Equals(c.Name, name, StringComparison.OrdinalIgnoreCase)).Name] = text;
                }
            }
            if (found.Count == 0)
            {
                throw new InvalidOperationException("Claude's answer held no description.");
            }
            return found;
        }

        private static string N(string text) => "N'" + text.Replace("'", "''") + "'";

        /// <summary>A script that adds each description, or updates the one already there.</summary>
        private static string Script(TableInfo table, Dictionary<string, string> descriptions, bool sampled)
        {
            var nl = Environment.NewLine;
            var quoted = N("[" + table.Schema.Replace("]", "]]") + "].[" + table.Name.Replace("]", "]]") + "]");
            var sb = new StringBuilder();
            sb.Append("-- The MS_Description of ").Append(table.Schema).Append('.').Append(table.Name).Append(", written by Claude from its definition")
              .Append(sampled ? " and five sample rows" : "").Append('.').Append(nl)
              .Append("-- Read and correct them, then run this script to save them.").Append(nl).Append(nl);
            foreach (var entry in descriptions.OrderBy(d => d.Key.Length == 0 ? -1 : table.Columns.FindIndex(c => c.Name == d.Key)))
            {
                var column = entry.Key.Length == 0 ? null : entry.Key;
                var minor = column == null ? "0" : "COLUMNPROPERTY(OBJECT_ID(" + quoted + "), " + N(column) + ", 'ColumnId')";
                var levels = "@level0type = N'SCHEMA', @level0name = " + N(table.Schema) + ", @level1type = N'" + table.Kind + "', @level1name = " + N(table.Name)
                    + (column == null ? "" : ", @level2type = N'COLUMN', @level2name = " + N(column));
                var value = "@name = N'MS_Description', @value = " + N(entry.Value) + ", ";
                sb.Append("-- ").Append(column ?? table.Name).Append(nl)
                  .Append("IF EXISTS (SELECT 1 FROM sys.extended_properties WHERE class = 1 AND major_id = OBJECT_ID(").Append(quoted).Append(") AND minor_id = ").Append(minor).Append(" AND name = N'MS_Description')").Append(nl)
                  .Append("    EXEC sys.sp_updateextendedproperty ").Append(value).Append(levels).Append(';').Append(nl)
                  .Append("ELSE").Append(nl)
                  .Append("    EXEC sys.sp_addextendedproperty ").Append(value).Append(levels).Append(';').Append(nl).Append(nl);
            }
            return sb.ToString();
        }
    }
}
