using System;
using System.ComponentModel.Design;
using System.Runtime.InteropServices;
using System.Threading;
using EnvDTE;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Threading;
using Task = System.Threading.Tasks.Task;
using TaskScheduler = System.Threading.Tasks.TaskScheduler;

namespace QuillSql.Vsix
{
    [PackageRegistration(UseManagedResourcesOnly = true, AllowsBackgroundLoading = true)]
    [InstalledProductRegistration("QuillSQL", "T-SQL formatting, suggestions, refactoring and code analysis with a Rust engine.", Version)]
    [ProvideMenuResource("Menus.ctmenu", 1)]
    // The AI chat docks on the right, like the tool windows people keep open beside a script.
    [ProvideToolWindow(typeof(AiChatPane), Style = VsDockStyle.Tabbed, Window = "3ae79031-e1bc-11d0-8f78-00a0c9110057", Orientation = ToolWindowOrientation.Right)]
    // Compare windows open as document tabs, several at a time.
    [ProvideToolWindow(typeof(ComparePane), MultiInstances = true, Style = VsDockStyle.MDI)]
    // Load with the shell so suggestions work in the first query window.
    [ProvideAutoLoad(Microsoft.VisualStudio.VSConstants.UICONTEXT.ShellInitialized_string, PackageAutoLoadFlags.BackgroundLoad)]
    [Guid("a4d6f0c1-7e2b-4c39-8f15-2b9e6d3a1c74")]
    public sealed class QuillSqlPackage : AsyncPackage
    {
        /// <summary>Must match the VSIX manifest and Cargo.toml; scripts\set-version.ps1 changes all three.</summary>
        public const string Version = "0.4.7";

        private static readonly Guid CommandSet = new Guid("c81e3b77-94a2-4f60-b1d8-5a7c2e9f0d36");
        private const int FormatCommandId = 0x0100;
        private const int RefreshCommandId = 0x0101;
        private const int ExpandWildcardsCommandId = 0x0102;
        private const int QualifyCommandId = 0x0103;
        private const int AddAsCommandId = 0x0104;
        private const int RemoveAsCommandId = 0x0105;
        private const int EncapsulateCommandId = 0x0106;
        private const int RenameCommandId = 0x0107;
        private const int InlineExecCommandId = 0x0108;
        private const int InsertSnippetCommandId = 0x0109;
        private const int AnalyzeCommandId = 0x010A;
        private const int FixIssuesCommandId = 0x010B;
        private const int ToggleAnalysisCommandId = 0x010C;
        private const int InvalidObjectsCommandId = 0x010D;
        private const int HistoryCommandId = 0x010E;
        private const int AskAiCommandId = 0x0110;
        private const int ExplainAiCommandId = 0x0111;
        private const int ReviewAiCommandId = 0x0112;
        private const int PlanAiCommandId = 0x0113;
        private const int ActualPlanAiCommandId = 0x0114;
        private const int SlowQueriesAiCommandId = 0x0115;
        private const int CheckUpdatesCommandId = 0x0116;
        private const int ScriptObjectCommandId = 0x0120;
        private const int InsertSemicolonsCommandId = 0x0121;
        private const int UnformatCommandId = 0x0122;
        private const int AddBracketsCommandId = 0x0123;
        private const int RemoveBracketsCommandId = 0x0124;
        private const int UppercaseCommandId = 0x0125;
        private const int LowercaseCommandId = 0x0126;
        private const int RemoveCommentsCommandId = 0x0127;
        private const int SpExecuteSqlCommandId = 0x0128;
        private const int ExecuteStatementCommandId = 0x0129;
        private const int ExecuteBatchCommandId = 0x012A;
        private const int SummarizeCommandId = 0x012B;
        private const int FormatAllCommandId = 0x012C;
        private const int DuplicateTabCommandId = 0x012D;
        private const int CloseUnmodifiedCommandId = 0x012E;
        private const int CreateSnippetCommandId = 0x012F;
        private const int AddCommasCommandId = 0x0130;
        private const int AddQuotesCommasCommandId = 0x0131;
        private const int DisableFormattingCommandId = 0x0132;
        private const int ToggleEnabledCommandId = 0x0133;
        private const int OptionsCommandId = 0x0134;
        private const int LogFolderCommandId = 0x0135;
        private const int ShowCardCommandId = 0x0136;
        private const int AiChatCommandId = 0x0137;
        private const int TabColorCommandId = 0x0138;
        private const int SchemaCompareCommandId = 0x0139;
        private const int DataCompareCommandId = 0x013A;

        private EditorWatcher _watcher;

        protected override async Task InitializeAsync(CancellationToken cancellationToken, IProgress<ServiceProgressData> progress)
        {
            await base.InitializeAsync(cancellationToken, progress);
            var commands = await GetServiceAsync(typeof(IMenuCommandService)) as OleMenuCommandService;
            await JoinableTaskFactory.SwitchToMainThreadAsync(cancellationToken);
            void add(int id, Action run) => commands?.AddCommand(Localized(new OleMenuCommand((s, e) => run(), new CommandID(CommandSet, id))));
            commands?.AddCommand(Localized(new OleMenuCommand(OnFormat, new CommandID(CommandSet, FormatCommandId))));
            commands?.AddCommand(Localized(new OleMenuCommand(OnRefresh, new CommandID(CommandSet, RefreshCommandId))));
            add(ExpandWildcardsCommandId, Commands.ExpandWildcards);
            add(QualifyCommandId, Commands.QualifyObjectNames);
            add(AddAsCommandId, () => Commands.TableAliases(true));
            add(RemoveAsCommandId, () => Commands.TableAliases(false));
            add(EncapsulateCommandId, Commands.EncapsulateAsProcedure);
            add(RenameCommandId, Commands.Rename);
            add(InlineExecCommandId, () => Commands.InlineExec(this));
            add(InsertSnippetCommandId, Commands.InsertSnippet);
            add(AnalyzeCommandId, CodeAnalysis.ShowInErrorList);
            add(FixIssuesCommandId, Commands.FixIssues);
            add(ToggleAnalysisCommandId, CodeAnalysis.ToggleUnderlines);
            add(InvalidObjectsCommandId, () => Commands.FindInvalidObjects(this));
            add(HistoryCommandId, QueryHistory.ShowWindow);
            add(AiChatCommandId, () => AiChatPane.Open(this));
            add(TabColorCommandId, TabColorDialog.ForActiveWindow);
            add(SchemaCompareCommandId, () => ComparePane.Open(this, false));
            add(DataCompareCommandId, () => ComparePane.Open(this, true));
            add(AskAiCommandId, () => AiAssistant.AskForSql(this));
            add(ExplainAiCommandId, () => AiAssistant.Explain(this));
            add(ReviewAiCommandId, () => AiAssistant.Review(this));
            add(PlanAiCommandId, () => AiAssistant.AnalyzePlan(this, false));
            add(ActualPlanAiCommandId, () => AiAssistant.AnalyzePlan(this, true));
            add(SlowQueriesAiCommandId, () => AiAssistant.ReviewSlowQueries(this));
            add(CheckUpdatesCommandId, () => Updater.Start(this, true));
            add(ScriptObjectCommandId, () => ObjectScripts.ScriptAtCaret(true));
            add(InsertSemicolonsCommandId, () => TextCommands.Rewrite("semicolons", "semicolons inserted"));
            add(UnformatCommandId, () => TextCommands.Rewrite("unformat", "unformatted"));
            add(AddBracketsCommandId, () => TextCommands.Rewrite("add_brackets", "brackets added"));
            add(RemoveBracketsCommandId, () => TextCommands.Rewrite("remove_brackets", "brackets removed"));
            add(UppercaseCommandId, () => TextCommands.Rewrite("uppercase", "keywords uppercased"));
            add(LowercaseCommandId, () => TextCommands.Rewrite("lowercase", "keywords lowercased"));
            add(RemoveCommentsCommandId, () => TextCommands.Rewrite("remove_comments", "comments removed"));
            add(SpExecuteSqlCommandId, () => TextCommands.Rewrite("sp_executesql", "sp_executesql converted"));
            add(ExecuteStatementCommandId, () => TextCommands.ExecuteAtCaret(false));
            add(ExecuteBatchCommandId, () => TextCommands.ExecuteAtCaret(true));
            add(SummarizeCommandId, TextCommands.SummarizeScript);
            add(FormatAllCommandId, TextCommands.FormatAllTabs);
            add(DuplicateTabCommandId, TextCommands.DuplicateTab);
            add(CloseUnmodifiedCommandId, TextCommands.CloseUnmodifiedTabs);
            add(CreateSnippetCommandId, TextCommands.CreateSnippet);
            add(AddCommasCommandId, () => TextCommands.AddCommas(false));
            add(AddQuotesCommasCommandId, () => TextCommands.AddCommas(true));
            add(DisableFormattingCommandId, TextCommands.DisableFormatting);
            add(ToggleEnabledCommandId, TextCommands.ToggleEnabled);
            add(OptionsCommandId, () => OptionsWindow.Show(this));
            add(LogFolderCommandId, TextCommands.OpenLogFolder);
            add(ShowCardCommandId, () =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                Log.Verbose("show object card command");
                if (!CompletionController.ShowCard(EditorContext.Active()?.View))
                {
                    EditorContext.Status("open a query window first.");
                }
            });
            try
            {
                CodeAnalysis.Initialize(this);
                _watcher = EditorWatcher.Start();
                TurnOffNativeAutoList();
                if (await GetServiceAsync(typeof(DTE)) is DTE dte)
                {
                    QueryHistory.Start(dte);
                }
                TabColorizer.Start();
                ObjectExplorerMenu.HostPackage = this;
                ObjectExplorerMenu.Start();
                Updater.Start(this, false);
            }
            catch (Exception ex)
            {
                Log.Error("starting suggestions", ex);
            }
        }

        /// <summary>
        /// Stops SSMS's own list from popping up while typing, so only one list shows. The previous
        /// value is saved next to the log; re-enable it under Tools > Options > Text Editor > Transact-SQL.
        /// </summary>
        private void TurnOffNativeAutoList()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!(GetService(typeof(DTE)) is DTE dte))
            {
                return;
            }
            var setting = dte.Properties["TextEditor", "SQL"].Item("AutoListMembers");
            if (setting.Value is bool on && on)
            {
                setting.Value = false;
                var saved = System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "native-autolist-was-on.txt");
                System.IO.File.WriteAllText(saved, DateTime.Now.ToString("s"));
                Log.Write("turned off SSMS auto list members (was on)");
            }
        }

        private void OnRefresh(object sender, EventArgs e)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            MetadataCache.Clear();
            Log.Write("suggestion metadata cleared");
            if (GetService(typeof(DTE)) is DTE dte)
            {
                dte.StatusBar.Text = "QuillSQL: suggestions will reload on next use.";
            }
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                EngineProcess.Shared.Dispose();
            }
            base.Dispose(disposing);
        }

        private void OnFormat(object sender, EventArgs e)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            _ = JoinableTaskFactory.RunAsync(FormatActiveDocumentAsync);
        }

        // In-process: an exception escaping here would surface inside SSMS, so report them all instead.
        private async Task FormatActiveDocumentAsync()
        {
            DTE dte = null;
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync(DisposalToken);
                dte = await GetServiceAsync(typeof(DTE)) as DTE;
                await JoinableTaskFactory.SwitchToMainThreadAsync(DisposalToken);

                if (dte == null)
                {
                    return;
                }

                var document = dte.ActiveDocument?.Object("TextDocument") as TextDocument;
                if (document == null)
                {
                    SetStatus(dte, "QuillSQL: open a query window first.");
                    return;
                }

                var selection = document.Selection;
                var useSelection = !selection.IsEmpty;
                var start = (useSelection ? selection.TopPoint : document.StartPoint).CreateEditPoint();
                var end = (useSelection ? selection.BottomPoint : document.EndPoint).CreateEditPoint();
                var original = start.GetText(end);

                var key = Commands.MetadataKey(out _);
                var settings = Settings.Current;
                await TaskScheduler.Default;
                var reply = EngineProcess.Shared.Request("format", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(original)
                    + ",\"expandWildcards\":" + Json.Bool(settings.FormatExpandsWildcards) + ",\"qualifyObjectNames\":" + Json.Bool(settings.FormatQualifiesNames));
                var parsed = reply == null ? null : Json.Parse<TextReply>(reply);
                await JoinableTaskFactory.SwitchToMainThreadAsync(DisposalToken);
                var formatted = parsed?.Result?.Text ?? throw new InvalidOperationException(parsed?.Error ?? "the engine did not answer");
                await JoinableTaskFactory.SwitchToMainThreadAsync(DisposalToken);

                if (formatted == original)
                {
                    SetStatus(dte, "QuillSQL: already formatted.");
                    return;
                }
                if (start.GetText(end) != original)
                {
                    SetStatus(dte, "QuillSQL: the text changed while formatting; nothing was applied.");
                    return;
                }

                // One undo step, unless the shell already has an undo context open.
                var ownsUndo = !dte.UndoContext.IsOpen;
                if (ownsUndo)
                {
                    dte.UndoContext.Open("Format T-SQL");
                }
                try
                {
                    start.ReplaceText(end, formatted, (int)vsEPReplaceTextOptions.vsEPReplaceTextKeepMarkers);
                }
                finally
                {
                    if (ownsUndo)
                    {
                        dte.UndoContext.Close();
                    }
                }
                SetStatus(dte, "QuillSQL: formatted.");
            }
            catch (OperationCanceledException)
            {
                // SSMS is shutting down.
            }
            catch (Exception ex)
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();
                SetStatus(dte, "QuillSQL: " + ex.Message);
            }
        }

        /// <summary>Shows the command's menu text in the chosen language; English comes from the .vsct.</summary>
        private static OleMenuCommand Localized(OleMenuCommand command)
        {
            command.BeforeQueryStatus += (s, e) =>
            {
                var c = (OleMenuCommand)s;
                if (MenuTexts.English.TryGetValue(c.CommandID.ID, out var english))
                {
                    c.Text = Loc.T(english);
                }
            };
            return command;
        }

        private static void SetStatus(DTE dte, string message)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (dte != null)
            {
                dte.StatusBar.Text = message.StartsWith("QuillSQL: ", StringComparison.Ordinal) ? "QuillSQL: " + Loc.T(message.Substring(10)) : message;
            }
        }
    }
}
