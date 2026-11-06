using System;
using System.Collections.Generic;
using System.Text.RegularExpressions;

namespace QuillSql.Compare
{
    /// <summary>
    /// What differs inside a changed line: both lines are split into SQL tokens (words, numbers,
    /// strings, white space, punctuation), the tokens are aligned, and the unmatched ones are marked.
    /// </summary>
    internal static class InlineDiff
    {
        public struct Piece
        {
            public string Text;
            public bool Different;
        }

        private static readonly Regex Token = new Regex(@"\s+|\[[^\]]*\]?|'(?:[^']|'')*'?|[A-Za-z_@#][\w@#$]*|\d+(?:\.\d+)?|.", RegexOptions.Compiled);

        private static List<string> Split(string s)
        {
            var list = new List<string>();
            foreach (Match m in Token.Matches(s ?? "")) list.Add(m.Value);
            return list;
        }

        /// <summary>The two lines in pieces; white space only differences count when <paramref name="ignoreSpace"/> is false.</summary>
        public static void Compare(string left, string right, out List<Piece> a, out List<Piece> b, bool ignoreSpace = true)
        {
            var ta = Split(left);
            var tb = Split(right);
            string key(string t) => ignoreSpace && t.Trim().Length == 0 ? " " : t;
            a = new List<Piece>();
            b = new List<Piece>();
            // Long lines are marked whole.
            if ((long)ta.Count * tb.Count > 250_000)
            {
                a.Add(new Piece { Text = left ?? "", Different = true });
                b.Add(new Piece { Text = right ?? "", Different = true });
                return;
            }
            var lcs = new int[ta.Count + 1, tb.Count + 1];
            for (var i = ta.Count - 1; i >= 0; i--)
            {
                for (var j = tb.Count - 1; j >= 0; j--)
                {
                    lcs[i, j] = key(ta[i]) == key(tb[j]) ? lcs[i + 1, j + 1] + 1 : Math.Max(lcs[i + 1, j], lcs[i, j + 1]);
                }
            }
            int x = 0, y = 0;
            while (x < ta.Count && y < tb.Count)
            {
                if (key(ta[x]) == key(tb[y]))
                {
                    Add(a, ta[x++], false);
                    Add(b, tb[y++], false);
                }
                else if (lcs[x + 1, y] >= lcs[x, y + 1])
                {
                    Add(a, ta[x++], true);
                }
                else
                {
                    Add(b, tb[y++], true);
                }
            }
            while (x < ta.Count) Add(a, ta[x++], true);
            while (y < tb.Count) Add(b, tb[y++], true);
        }

        /// <summary>Joins neighbouring pieces of the same kind; changed white space alone is not marked.</summary>
        private static void Add(List<Piece> list, string text, bool different)
        {
            if (different && text.Trim().Length == 0) different = false;
            if (list.Count > 0 && list[list.Count - 1].Different == different)
            {
                var last = list[list.Count - 1];
                last.Text += text;
                list[list.Count - 1] = last;
            }
            else
            {
                list.Add(new Piece { Text = text, Different = different });
            }
        }
    }
}
