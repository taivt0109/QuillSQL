using System.Collections.Generic;

namespace QuillSql.Vsix
{
    /// <summary>The English menu texts of the .vsct, by command id, for <see cref="Loc"/>.</summary>
    internal static class MenuTexts
    {
        public static readonly Dictionary<int, string> English = new Dictionary<int, string>
        {
            [0x0100] = "Format T-SQL",
            [0x0101] = "Refresh T-SQL Suggestions",
            [0x0109] = "Insert Snippet...",
            [0x0102] = "Expand Wildcards",
            [0x0103] = "Qualify Object Names",
            [0x0104] = "Add AS to Aliases",
            [0x0105] = "Remove AS from Aliases",
            [0x0107] = "Smart Rename...",
            [0x0106] = "Encapsulate as Stored Procedure...",
            [0x0108] = "Inline EXEC",
            [0x010A] = "Analyze Code",
            [0x010B] = "Fix Code Issues",
            [0x010C] = "Toggle Code Analysis Underlines",
            [0x010D] = "Find Invalid Objects",
            [0x010E] = "Query History...",
            [0x0110] = "Ask AI...",
            [0x0111] = "Explain with AI",
            [0x0112] = "Review with AI",
            [0x0113] = "Analyze Plan with AI",
            [0x0114] = "Analyze Actual Plan with AI (runs the query)...",
            [0x0115] = "Find Slow Queries with AI (Query Store)",
            [0x0116] = "Check for Updates",
            [0x013B] = "About QuillSQL",
            [0x0120] = "Script Object at Caret",
            [0x0121] = "Insert Semicolons",
            [0x0122] = "Unformat",
            [0x0123] = "Add Square Brackets",
            [0x0124] = "Remove Square Brackets",
            [0x0125] = "UPPERCASE Keywords",
            [0x0126] = "lowercase Keywords",
            [0x0127] = "Remove Comments",
            [0x0128] = "Convert sp_executesql to SQL",
            [0x0129] = "Execute Current Statement",
            [0x012A] = "Execute Current Batch",
            [0x012B] = "Summarize Script",
            [0x012C] = "Format All Open Tabs",
            [0x012D] = "Duplicate Tab",
            [0x012E] = "Close Unmodified Tabs",
            [0x012F] = "Create Snippet from Selection...",
            [0x0130] = "Add Commas to Lines",
            [0x0131] = "Add Quotes and Commas to Lines",
            [0x0132] = "Keep Selection Unformatted",
            [0x0133] = "Enable or Disable QuillSQL",
            [0x0134] = "Options...",
            [0x0135] = "Open Log Folder",
            [0x0136] = "Show Object Card",
            [0x0137] = "AI Chat",
            [0x0138] = "Tab Color for This Connection...",
            [0x0139] = "Schema Compare...",
            [0x013A] = "Data Compare...",
        };
    }
}
