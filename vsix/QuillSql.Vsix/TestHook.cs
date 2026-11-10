using System;
using System.IO;
using System.IO.Pipes;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.OLE.Interop;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.TextManager.Interop;
using Task = System.Threading.Tasks.Task;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Lets the end-to-end tests drive the editor without a screen: when SSMS is started with the
    /// QUILLSQL_TEST_PIPE environment variable, commands come one a line on that named pipe and keys
    /// go through the editor's command chain, as the keyboard's do. Without the variable nothing
    /// listens, so a normal SSMS never has the pipe.
    /// </summary>
    internal static class TestHook
    {
        public static void Start()
        {
            var name = Environment.GetEnvironmentVariable("QUILLSQL_TEST_PIPE");
            if (string.IsNullOrEmpty(name))
            {
                return;
            }
            Log.Write("test hook listening on pipe " + name);
            new Thread(() => Serve(name)) { IsBackground = true, Name = "QuillSQL test hook" }.Start();
        }

        private static void Serve(string name)
        {
            while (true)
            {
                try
                {
                    using (var pipe = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte))
                    {
                        pipe.WaitForConnection();
                        var reader = new StreamReader(pipe, new UTF8Encoding(false));
                        var writer = new StreamWriter(pipe, new UTF8Encoding(false)) { AutoFlush = true };
                        string line;
                        while ((line = reader.ReadLine()) != null)
                        {
                            string reply;
                            try
                            {
                                reply = ThreadHelper.JoinableTaskFactory.Run(() => HandleAsync(line));
                            }
                            catch (Exception ex)
                            {
                                reply = "error " + ex.Message;
                            }
                            writer.WriteLine((reply ?? "").Replace("\\", "\\\\").Replace("\r", "\\r").Replace("\n", "\\n"));
                        }
                    }
                }
                catch (IOException ex)
                {
                    Log.Write("test hook: " + ex.Message);
                }
            }
        }

        /// <summary>One command: `name|argument`; the reply is one line.</summary>
        private static async Task<string> HandleAsync(string line)
        {
            var bar = line.IndexOf('|');
            var command = bar < 0 ? line : line.Substring(0, bar);
            var argument = bar < 0 ? "" : line.Substring(bar + 1).Replace("\\n", "\n").Replace("\\r", "\r");
            await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
            switch (command)
            {
                case "open":
                {
                    // open|server|database: a new query window connected there.
                    var parts = argument.Split('|');
                    var before = EditorContext.Active()?.View;
                    if (!EditorContext.OpenConnected("", parts[0], parts.ElementAtOrDefault(1), 0, null, true))
                    {
                        return "failed";
                    }
                    for (var i = 0; i < 80; i++)
                    {
                        var now = EditorContext.Active()?.View;
                        if (now != null && now != before)
                        {
                            return "ok";
                        }
                        await Task.Delay(250);
                    }
                    return "no editor";
                }
                case "set":
                {
                    // set|text with | at the caret (the end without one).
                    var editor = EditorContext.Active();
                    if (editor == null)
                    {
                        return "no editor";
                    }
                    var caret = argument.IndexOf('|');
                    var text = argument.Replace("|", "");
                    editor.Replace(0, editor.Text.Length, text);
                    EditorContext.Active()?.Select(caret < 0 ? text.Length : caret, caret < 0 ? text.Length : caret);
                    return "ok";
                }
                case "type":
                    foreach (var c in argument)
                    {
                        if (!Exec(VSConstants.VSStd2KCmdID.TYPECHAR, c))
                        {
                            return "no editor";
                        }
                        // Let the suggestions and hints answer, as they would for someone typing.
                        await Task.Delay(c == ' ' || c == '(' || c == ',' ? 600 : 120);
                    }
                    await Task.Delay(600);
                    return "ok";
                case "key":
                {
                    VSConstants.VSStd2KCmdID id;
                    switch (argument)
                    {
                        case "tab": id = VSConstants.VSStd2KCmdID.TAB; break;
                        case "enter": id = VSConstants.VSStd2KCmdID.RETURN; break;
                        case "esc": id = VSConstants.VSStd2KCmdID.CANCEL; break;
                        case "backspace": id = VSConstants.VSStd2KCmdID.BACKSPACE; break;
                        case "down": id = VSConstants.VSStd2KCmdID.DOWN; break;
                        case "up": id = VSConstants.VSStd2KCmdID.UP; break;
                        case "ctrlspace": id = VSConstants.VSStd2KCmdID.COMPLETEWORD; break;
                        default: return "unknown key " + argument;
                    }
                    if (!Exec(id, null))
                    {
                        return "no editor";
                    }
                    await Task.Delay(800);
                    return "ok";
                }
                case "wait":
                    await Task.Delay(int.TryParse(argument, out var ms) ? ms : 1000);
                    return "ok";
                case "text":
                    return EditorContext.Active()?.Text ?? "";
                case "list":
                {
                    // The suggestions showing, the selected one first with a *; "closed" when none show.
                    var view = EditorContext.Active()?.View;
                    var controller = view == null ? null : CompletionController.ForView(view);
                    return controller?.ListState() ?? "no editor";
                }
                case "tip":
                {
                    var view = EditorContext.Active()?.View;
                    var controller = view == null ? null : CompletionController.ForView(view);
                    return controller?.TipState() ?? "no editor";
                }
                case "card":
                {
                    // card|picture.png: the card at the caret, as text; a picture of it goes to the path.
                    if (!CompletionController.ShowCard(EditorContext.Active()?.View))
                    {
                        return "no editor";
                    }
                    for (var i = 0; i < 40 && !CardPopup.IsOpen; i++)
                    {
                        await Task.Delay(250);
                    }
                    await Task.Delay(1500);
                    return CardPopup.Snapshot(argument);
                }
                case "bindings":
                {
                    // bindings|Shift+F5: the commands bound to the keys, as "scope::keys name" lines.
                    var found = new System.Collections.Generic.List<string>();
                    foreach (EnvDTE.Command c in EditorContext.Dte.Commands)
                    {
                        object[] bindings;
                        try
                        {
                            bindings = c.Bindings as object[];
                        }
                        catch
                        {
                            continue;
                        }
                        foreach (var b in bindings ?? new object[0])
                        {
                            var text = b as string ?? "";
                            if (text.EndsWith("::" + argument, StringComparison.OrdinalIgnoreCase))
                            {
                                found.Add(text + " " + c.Name);
                            }
                        }
                    }
                    return string.Join("\n", found);
                }
                case "save":
                    SessionKeeper.Save();
                    return "ok";
                case "reopen":
                {
                    var count = SessionKeeper.ReopenLost(false);
                    await Task.Delay(3000);
                    return count.ToString();
                }
                case "status":
                    return EditorContext.Dte?.StatusBar.Text ?? "";
                case "tabs":
                    return TabColorizer.ColoredTabs.ToString();
                case "command":
                    EditorContext.Dte?.ExecuteCommand(argument);
                    await Task.Delay(1500);
                    return "ok";
                default:
                    return "unknown command " + command;
            }
        }

        /// <summary>Sends an editor command (a typed character, Tab, Enter...) through the active view's chain.</summary>
        private static bool Exec(VSConstants.VSStd2KCmdID id, char? typed)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (!(EditorContext.Active()?.View is IOleCommandTarget target))
            {
                return false;
            }
            var group = VSConstants.VSStd2K;
            var input = IntPtr.Zero;
            try
            {
                if (typed != null)
                {
                    input = Marshal.AllocCoTaskMem(32);
                    Marshal.GetNativeVariantForObject((ushort)typed.Value, input);
                }
                target.Exec(ref group, (uint)id, 0, input, IntPtr.Zero);
                return true;
            }
            finally
            {
                if (input != IntPtr.Zero)
                {
                    Marshal.FreeCoTaskMem(input);
                }
            }
        }
    }
}
