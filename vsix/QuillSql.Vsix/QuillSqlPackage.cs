using System;
using System.ComponentModel.Design;
using System.Runtime.InteropServices;
using System.Threading;
using EnvDTE;
using Microsoft.VisualStudio.Shell;
using Task = System.Threading.Tasks.Task;

namespace QuillSql.Vsix
{
    [PackageRegistration(UseManagedResourcesOnly = true, AllowsBackgroundLoading = true)]
    [InstalledProductRegistration("QuillSQL", "T-SQL formatting, suggestions, refactoring and code analysis with a Rust engine.", Version)]
    [ProvideMenuResource("Menus.ctmenu", 1)]
    // Load with the shell so suggestions work in the first query window.
    [ProvideAutoLoad(Microsoft.VisualStudio.VSConstants.UICONTEXT.ShellInitialized_string, PackageAutoLoadFlags.BackgroundLoad)]
    [Guid("a4d6f0c1-7e2b-4c39-8f15-2b9e6d3a1c74")]
    public sealed class QuillSqlPackage : AsyncPackage
    {
        /// <summary>Must match the VSIX manifest and Cargo.toml; scripts\set-version.ps1 changes all three.</summary>
        public const string Version = "0.2.0";

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

        private EditorWatcher _watcher;

        protected override async Task InitializeAsync(CancellationToken cancellationToken, IProgress<ServiceProgressData> progress)
        {
            await base.InitializeAsync(cancellationToken, progress);
            var commands = await GetServiceAsync(typeof(IMenuCommandService)) as OleMenuCommandService;
            await JoinableTaskFactory.SwitchToMainThreadAsync(cancellationToken);
            void add(int id, Action run) => commands?.AddCommand(new MenuCommand((s, e) => run(), new CommandID(CommandSet, id)));
            commands?.AddCommand(new MenuCommand(OnFormat, new CommandID(CommandSet, FormatCommandId)));
            commands?.AddCommand(new MenuCommand(OnRefresh, new CommandID(CommandSet, RefreshCommandId)));
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
            add(AskAiCommandId, () => AiAssistant.AskForSql(this));
            add(ExplainAiCommandId, () => AiAssistant.Explain(this));
            add(ReviewAiCommandId, () => AiAssistant.Review(this));
            add(PlanAiCommandId, () => AiAssistant.AnalyzePlan(this, false));
            add(ActualPlanAiCommandId, () => AiAssistant.AnalyzePlan(this, true));
            add(SlowQueriesAiCommandId, () => AiAssistant.ReviewSlowQueries(this));
            add(CheckUpdatesCommandId, () => Updater.Start(this, true));
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

                var formatted = await FormatterProcess.RunAsync(original, DisposalToken);
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

        private static void SetStatus(DTE dte, string message)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (dte != null)
            {
                dte.StatusBar.Text = message;
            }
        }
    }
}
