using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

// Drives SSMS through QuillSQL's test hook (a named pipe): no screen, no focus, no connect dialog.
static class HookDriver
{
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, int msg, IntPtr w, IntPtr l);

    static StreamReader reader;
    static StreamWriter writer;
    static int failures;
    static bool keep;

    static string Send(string command)
    {
        writer.WriteLine(command.Replace("\r", "\\r").Replace("\n", "\\n"));
        var reply = reader.ReadLine() ?? "";
        return reply.Replace("\\r", "\r").Replace("\\n", "\n").Replace("\\\\", "\\");
    }

    static void Check(string name, bool ok, string detail)
    {
        if (!ok) failures++;
        Console.WriteLine((ok ? "PASS " : "FAIL ") + name + (ok ? "" : "\n   got: " + detail.Replace("\r\n", "\n").Replace("\n", "\n        ")));
    }

    /// <summary>Starts SSMS with the test hook and connects to it.</summary>
    static Process Launch(string ide)
    {
        var pipeName = "quillsql-test-" + Guid.NewGuid().ToString("N");
        var start = new ProcessStartInfo(Path.Combine(ide, "SSMS.exe"), "-nosplash /rootsuffix Exp") { UseShellExecute = false };
        start.EnvironmentVariables["QUILLSQL_TEST_PIPE"] = pipeName;
        var ssms = Process.Start(start);
        Console.WriteLine("ssms pid " + ssms.Id);
        // The startup connect dialog is not needed: the hook opens its own connected window.
        var closer = new Thread(() =>
        {
            while (true)
            {
                EnumWindows((h, l) =>
                {
                    int p; GetWindowThreadProcessId(h, out p);
                    if (p == ssms.Id && IsWindowVisible(h))
                    {
                        var s = new StringBuilder(256); GetWindowText(h, s, 256);
                        if (s.ToString() == "Connect to Server") PostMessage(h, 0x0010, IntPtr.Zero, IntPtr.Zero);
                    }
                    return true;
                }, IntPtr.Zero);
                Thread.Sleep(1000);
            }
        }) { IsBackground = true };
        closer.Start();
        var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.InOut);
        pipe.Connect(300000);
        reader = new StreamReader(pipe, new UTF8Encoding(false));
        writer = new StreamWriter(pipe, new UTF8Encoding(false)) { AutoFlush = true };
        Console.WriteLine("hook connected");
        return ssms;
    }

    static int Main(string[] a)
    {
        Console.OutputEncoding = new UTF8Encoding(false);
        var ide = a[0];
        // --keep: check the column card, then leave SSMS open with a query to look at.
        keep = a.Length > 1 && a[1] == "--keep";
        // Windows kept by earlier test runs (each ends by killing SSMS) are not this run's.
        var sessions = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuillSQL", "Sessions-test");
        if (Directory.Exists(sessions)) Directory.Delete(sessions, true);
        Process ssms = null;
        try
        {
            ssms = Launch(ide);
            var opened = Send(@"open|(localdb)\MSSQLLocalDB|QuillTest");
            Console.WriteLine("open: " + opened);
            if (opened != "ok") return 2;
            // Metadata loads in the background; Ctrl+Space waits for it.
            for (var i = 0; i < 60; i++)
            {
                Send("set|select * from dbo.Cus"); Send("key|ctrlspace");
                if (Send("list").Contains("Customers")) break;
                Send("key|esc"); Send("wait|1000");
            }
            Send("key|esc");
            var ready = "";
            for (var i = 0; i < 40 && !ready.Contains("ready"); i++) { ready = Send("status"); if (!ready.Contains("ready")) Send("wait|500"); }
            Check("a new window says when its suggestions are ready", ready.Contains("suggestions ready") && ready.Contains("objects"), ready);

            Func<string, string, string> typed = (start0, text) => { Send("set|" + start0); Send("type|" + text); return Send("text"); };

            // Shift+F5 runs the statement at the caret, as in other SQL tools, with no other command on it.
            var shiftF5 = Send("bindings|Shift+F5");
            Console.WriteLine("Shift+F5: " + shiftF5.Replace("\n", "; "));
            Check("Shift+F5 runs the statement at the caret", shiftF5.Contains("QuillSQL.ExecuteCurrentStatement"), shiftF5);

            // The card of a column, with its long description (MS_Description).
            Send("set|select c.Na|me from dbo.Customers c");
            var card = Send("card|" + Path.Combine(Path.GetTempPath(), "quillsql-e2e", "column-card.png"));
            Check("column card with its description", card.Contains("COLUMN") && card.Contains("nvarchar(100)") && card.Contains("CRM"), card);
            Send("key|esc");
            if (keep)
            {
                // Left open to look at: a query to rest the mouse on.
                Send("set|SELECT c.CustomerId, c.Name, c.Created\r\nFROM dbo.Customers AS c\r\nJOIN sales.Orders AS o ON o.CustomerId = c.CustomerId\r\nWHERE o.Total > 10;");
                Console.WriteLine(failures == 0 ? "ALL PASSED" : failures + " FAILED");
                return failures == 0 ? 0 : 1;
            }

            var t = typed("", "select * from dbo.Customers c where c.Name = 'from x' or c.Name is null;");
            Check("keywords in upper case, strings alone", t == "SELECT * FROM dbo.Customers c WHERE c.Name = 'from x' OR c.Name IS NULL;", t);

            t = typed("", "declare @d datetime2 = getdate(); select dateadd(day, 1, @d), cast(c.Name as varchar(10)), c.Name as date from dbo.Customers c;");
            Check("functions and data types in upper case", t == "DECLARE @d DATETIME2 = GETDATE(); SELECT DATEADD(day, 1, @d), CAST(c.Name AS VARCHAR(10)), c.Name AS date FROM dbo.Customers c;", t);

            typed("", "insert into dbo.Custo"); Send("key|tab"); t = Send("text");
            Check("INSERT INTO + Tab writes the statement", t.StartsWith("INSERT INTO dbo.Customers") && t.Contains("VALUES") && t.Contains("DEFAULT -- Created"), t);

            typed("", "insert Custo"); Send("key|enter"); t = Send("text");
            Check("INSERT without INTO + Enter", t.Contains("dbo.Customers") && t.Contains("VALUES"), t);

            // Tab writes what the word takes, the caret inside: typing goes there.
            typed("", "select * from dbo.Customers c where c.CustomerId i"); Send("key|tab"); Send("type|1"); t = Send("text");
            Check("IN + Tab writes the parentheses", t.EndsWith("c.CustomerId IN (1)"), t);
            typed("", "select * from dbo.Customers c where c.Name = "); Send("key|tab"); Send("type|x"); t = Send("text");
            Check("a text column's value is quoted", t.EndsWith("c.Name = N'x'"), t);

            Send("set|select | from dbo.Customers AS c"); Send("type|Na"); Send("key|enter"); t = Send("text");
            Check("column written with its table's alias", t == "select c.Name from dbo.Customers AS c", t);

            typed("", "exec dbo.usp_GetOrders @CustomerId = 1, "); var tip = Send("tip"); var list = Send("list");
            Check("EXEC parameter hints", tip.StartsWith("open") && tip.Contains("@Max"), tip);
            Check("EXEC offers only the parameters not given", list.Contains("@Max") && !list.Contains("@CustomerId"), list);
            Send("key|esc");

            typed("", "select dateadd(day, "); tip = Send("tip");
            Check("function parameter hints", tip.StartsWith("open DATEADD"), tip);
            Send("key|esc");

            typed("create table #t (Id int, Code varchar(10));\r\n", "select * from #t t where t.co"); Send("key|tab"); t = Send("text");
            Check("temp table columns", t.EndsWith("t.Code"), t);

            typed("", "select * from dbo.Customers c where c.Name l"); Send("key|tab"); t = Send("text");
            Check("operators after an operand", t.EndsWith("c.Name LIKE ''"), t);

            typed("", "select * from master.sys.tabl"); Send("key|tab"); t = Send("text");
            Check("other database, shortest name first", t.StartsWith("SELECT * FROM master.sys.tables"), t);

            // A column added while the window is open, found after Ctrl+Space checks for changes.
            Process.Start(new ProcessStartInfo("sqlcmd", "-S \"(localdb)\\MSSQLLocalDB\" -d QuillTest -Q \"ALTER TABLE dbo.Customers ADD Phone varchar(20)\"") { UseShellExecute = false, CreateNoWindow = true }).WaitForExit();
            Send("set|select * from dbo.Customers c where c.ph"); Send("key|ctrlspace"); Send("wait|5000"); Send("key|esc");
            Send("type|o"); Send("key|tab"); t = Send("text");
            Check("schema change picked up", t.EndsWith("c.Phone"), t);

            typed("", "exec sp_who"); list = Send("list"); Send("key|esc");
            Check("system procedures from the server after EXEC", list.Contains("sp_who2"), list);

            typed("", "sp_who"); list = Send("list"); Send("key|esc");
            Check("procedures where a statement starts, without EXEC", list.Contains("sp_who2"), list);
            typed("", "select 1 from dbo.Customers c where c.Name = 'x' and sp_who"); list = Send("list"); Send("key|esc");
            Check("no procedures inside an expression", !list.Contains("sp_who2"), list);

            Send("command|Tools.RefreshTSQLSuggestions");
            var status = "";
            for (var i = 0; i < 40 && !status.Contains("refreshed"); i++) { Send("wait|500"); status = Send("status"); }
            Check("refresh says when it is done", status.Contains("suggestions refreshed") && status.Contains("objects"), status);

            Send("wait|4000");
            var tabs = Send("tabs");
            Check("query tab colored", int.TryParse(tabs, out var n) && n > 0, tabs);

            // SSMS killed with a query window open: the next start reopens it.
            Send("set|select 'restore me' as probe"); Send("save");
            ssms.Kill(); ssms.WaitForExit(30000);
            ssms = Launch(ide);
            var reopened = Send("reopen");
            t = Send("text");
            Check("query windows reopened after SSMS was killed", reopened != "0" && t.Contains("restore me"), reopened + " / " + t);

            Console.WriteLine(failures == 0 ? "ALL PASSED" : failures + " FAILED");
            return failures == 0 ? 0 : 1;
        }
        catch (Exception ex)
        {
            Console.WriteLine("FAILED: " + ex.Message);
            return 3;
        }
        finally
        {
            if (!keep) try { ssms?.Kill(); } catch { }
        }
    }
}
