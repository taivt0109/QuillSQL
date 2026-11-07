//! Comparing SQL text: module definitions (views, procedures, functions, triggers) and
//! expressions (defaults, check constraints, computed columns, index filters).

use super::options::{Opt, Options};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Word,
    Space,
    Str,
    LineComment,
    BlockComment,
    /// `[name]` or `"name"`.
    Quoted,
    Punct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Tok,
    pub text: String,
}

impl Token {
    fn is_trivia(&self) -> bool {
        matches!(self.kind, Tok::Space | Tok::LineComment | Tok::BlockComment)
    }

    fn is_word(&self, w: &str) -> bool {
        self.kind == Tok::Word && self.text.eq_ignore_ascii_case(w)
    }
}

/// Splits SQL into tokens; never fails (unterminated strings and comments run to the end).
pub fn tokenize(sql: &str) -> Vec<Token> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<Token>, kind, s: &[char]| out.push(Token { kind, text: s.iter().collect() });
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push(&mut out, Tok::Space, &chars[start..i]);
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                i += 1;
            }
            push(&mut out, Tok::LineComment, &chars[start..i]);
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            push(&mut out, Tok::BlockComment, &chars[start..i.min(chars.len())]);
        } else if c == '\'' || ((c == 'N' || c == 'n') && chars.get(i + 1) == Some(&'\'')) {
            i += if c == '\'' { 1 } else { 2 };
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(&mut out, Tok::Str, &chars[start..i]);
        } else if c == '[' || c == '"' {
            let close = if c == '[' { ']' } else { '"' };
            i += 1;
            while i < chars.len() {
                if chars[i] == close {
                    if chars.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(&mut out, Tok::Quoted, &chars[start..i]);
        } else if c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$') {
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '@' | '#' | '$')) {
                i += 1;
            }
            // 1.5e10 and 0x1F stay one token.
            if c.is_ascii_digit() {
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.') {
                    i += 1;
                }
            }
            push(&mut out, Tok::Word, &chars[start..i]);
        } else {
            i += 1;
            push(&mut out, Tok::Punct, &chars[start..i]);
        }
    }
    out
}

/// The unquoted name of an identifier token.
fn unquote(t: &Token) -> String {
    match t.kind {
        Tok::Quoted => {
            let inner = &t.text[1..t.text.len().saturating_sub(1)];
            if t.text.starts_with('[') { inner.replace("]]", "]") } else { inner.replace("\"\"", "\"") }
        }
        _ => t.text.clone(),
    }
}

/// What kind of SQL text is being compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    /// CREATE VIEW / PROCEDURE / FUNCTION / TRIGGER: the object name in the header is ignored,
    /// so an object renamed with sp_rename (whose stored text keeps the old name) is not reported.
    Module,
    /// A default, check, computed column or filter predicate, as stored by SQL Server.
    Expression,
}

/// The first difference between two texts, with a little context for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDifference {
    pub source: String,
    pub target: String,
}

/// Compares two texts under the options; `None` when they are equal.
pub fn compare(a: &str, b: &str, kind: TextKind, opts: &Options) -> Option<TextDifference> {
    if a == b || a.trim() == b.trim() {
        return None;
    }
    let ta = normalize(a, kind, opts);
    let tb = normalize(b, kind, opts);
    let case_sensitive = opts.has(Opt::CaseSensitiveObjectDefinition);
    // String literals always compare exactly: unlike SQL Compare, 'abc' and 'ABC' in a default
    // or a procedure body are reported, because they store or return different data.
    let same = |x: &Token, y: &Token| {
        x.kind == y.kind
            && if case_sensitive || x.kind == Tok::Str { x.text == y.text } else { x.text.to_lowercase() == y.text.to_lowercase() }
    };
    let n = ta.len().min(tb.len());
    let first = (0..n).find(|&i| !same(&ta[i], &tb[i])).unwrap_or(n);
    if first == n && ta.len() == tb.len() {
        return None;
    }
    Some(TextDifference { source: summary(&ta, first), target: summary(&tb, first) })
}

/// About 40 characters around token `at`.
fn summary(tokens: &[Token], at: usize) -> String {
    let sep = " ";
    let mut before = String::new();
    let mut i = at;
    while i > 0 && before.len() < 20 {
        i -= 1;
        before = format!("{}{sep}{before}", tokens[i].text);
    }
    if i > 0 {
        before = format!("…{before}");
    }
    let mut after = String::new();
    let mut j = at;
    while j < tokens.len() && after.len() < 20 {
        after.push_str(&tokens[j].text);
        after.push_str(sep);
        j += 1;
    }
    if j < tokens.len() {
        after.push('…');
    }
    format!("{before}{after}").trim().to_string()
}

fn normalize(sql: &str, kind: TextKind, opts: &Options) -> Vec<Token> {
    let mut tokens = tokenize(sql);
    let ignore_ws = opts.has(Opt::IgnoreWhiteSpace) || kind == TextKind::Expression;
    let ignore_comments = opts.has(Opt::IgnoreComments) || kind == TextKind::Expression;
    // SQL Server stores expressions with brackets around names; scripts often have none.
    let strip_brackets = opts.has(Opt::IgnoreSquareBrackets) || kind == TextKind::Expression;

    // Leading and trailing trivia never matter.
    while tokens.first().is_some_and(|t| t.kind == Tok::Space || (ignore_comments && t.is_trivia())) {
        tokens.remove(0);
    }
    while tokens.last().is_some_and(|t| t.kind == Tok::Space || (ignore_comments && t.is_trivia())) {
        tokens.pop();
    }
    if kind == TextKind::Module {
        tokens = module_header(tokens, opts);
        if opts.has(Opt::IgnoreWithElementOrder) {
            tokens = sort_with_clause(tokens);
        }
        if opts.has(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings) {
            tokens = remove_set_options(tokens);
        }
    }
    if opts.has(Opt::IgnoreNotForReplication) {
        tokens = remove_words(tokens, &["NOT", "FOR", "REPLICATION"]);
    }
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    for mut t in tokens {
        match t.kind {
            Tok::LineComment | Tok::BlockComment if ignore_comments => continue,
            Tok::Space if ignore_ws => continue,
            Tok::Space => {
                // Runs of whitespace compare equal to each other only by content; collapse
                // CRLF/LF so that line-ending changes alone are not differences.
                t.text = t.text.replace("\r\n", "\n");
            }
            Tok::LineComment | Tok::BlockComment if ignore_ws => {
                t.text.retain(|c| !c.is_whitespace());
            }
            Tok::Str if ignore_ws => t.text = t.text.replace("\r\n", "\n"),
            Tok::Quoted if strip_brackets && t.text.starts_with('[') => {
                t.text = unquote(&t);
                t.kind = Tok::Word;
            }
            _ => {}
        }
        out.push(t);
    }
    if kind == TextKind::Expression {
        out = strip_parentheses(out);
        out = drop_convert_style_zero(out);
    }
    out
}

/// `CREATE OR ALTER` → `CREATE`, and drops the object name after `CREATE <kind>` (and the
/// owner after a trigger's `ON` when owners are ignored).
fn module_header(mut tokens: Vec<Token>, opts: &Options) -> Vec<Token> {
    let significant: Vec<usize> = (0..tokens.len()).filter(|&i| !tokens[i].is_trivia()).collect();
    let Some(create) = significant.iter().position(|&i| tokens[i].is_word("CREATE")) else { return tokens };
    let mut drop = vec![false; tokens.len()];
    let mut k = create + 1;
    if significant.get(k).is_some_and(|&i| tokens[i].is_word("OR"))
        && significant.get(k + 1).is_some_and(|&i| tokens[i].is_word("ALTER"))
    {
        for &i in &significant[k..k + 2] {
            drop[i] = true;
        }
        k += 2;
    }
    let kind_word = significant.get(k).map(|&i| tokens[i].text.to_ascii_uppercase());
    k += 1;
    // The (possibly qualified) name: ident (. ident)*
    let mut name_end = k;
    while let Some(&i) = significant.get(name_end) {
        if !matches!(tokens[i].kind, Tok::Word | Tok::Quoted) {
            break;
        }
        drop[i] = true;
        if significant.get(name_end + 1).is_some_and(|&j| tokens[j].text == ".") {
            drop[significant[name_end + 1]] = true;
            name_end += 2;
        } else {
            name_end += 1;
            break;
        }
    }
    if kind_word.as_deref() == Some("TRIGGER")
        && let Some(on) = (name_end..significant.len()).find(|&p| tokens[significant[p]].is_word("ON"))
    {
        let mut p = on + 1;
        if opts.has(Opt::IgnoreOwners) {
            // ON [owner].[table]: keep only the last part.
            while significant.get(p + 1).is_some_and(|&j| tokens[j].text == ".") {
                drop[significant[p]] = true;
                drop[significant[p + 1]] = true;
                p += 2;
            }
        }
        // The table is named with or without brackets: `ON [T]` is `ON T`.
        if let Some(&i) = significant.get(p)
            && tokens[i].kind == Tok::Quoted
            && tokens[i].text.starts_with('[')
        {
            tokens[i].text = unquote(&tokens[i]);
            tokens[i].kind = Tok::Word;
        }
    }
    // Remove dropped tokens and the whitespace that preceded each of them.
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    for (i, t) in tokens.into_iter().enumerate() {
        if drop[i] {
            if out.last().is_some_and(|l| l.kind == Tok::Space) {
                out.pop();
            }
            continue;
        }
        out.push(t);
    }
    out
}

/// Sorts the options of the first `WITH a, b, c` clause of a module header.
fn sort_with_clause(tokens: Vec<Token>) -> Vec<Token> {
    const ELEMENTS: &[&str] = &["ENCRYPTION", "SCHEMABINDING", "VIEW_METADATA", "RECOMPILE", "NATIVE_COMPILATION", "RETURNS", "CALLED", "EXECUTE", "EXEC", "INLINE"];
    let sig: Vec<usize> = (0..tokens.len()).filter(|&i| !tokens[i].is_trivia()).collect();
    let Some(w) = sig.iter().position(|&i| tokens[i].is_word("WITH")) else { return tokens };
    // Each element runs to the next comma at depth 0, until a token that cannot belong to an element.
    let mut elements: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut p = w + 1;
    while let Some(&i) = sig.get(p) {
        let t = &tokens[i];
        if cur.is_empty() && !ELEMENTS.iter().any(|e| t.is_word(e)) {
            break;
        }
        if t.text == "," {
            elements.push(std::mem::take(&mut cur));
        } else if (t.is_word("AS") && !cur.iter().any(|&c| tokens[c].is_word("EXECUTE") || tokens[c].is_word("EXEC")))
            || t.is_word("FOR")
            || t.is_word("BEGIN")
            || t.text == "("
        {
            break;
        } else {
            cur.push(i);
        }
        p += 1;
    }
    if !cur.is_empty() {
        elements.push(cur);
    }
    if elements.len() < 2 {
        return tokens;
    }
    let first = elements[0][0];
    let last = *elements.last().unwrap().last().unwrap();
    let mut texts: Vec<String> = elements
        .iter()
        .map(|e| e.iter().map(|&i| tokens[i].text.to_ascii_uppercase()).collect::<Vec<_>>().join(" "))
        .collect();
    texts.sort();
    let mut out: Vec<Token> = tokens[..first].to_vec();
    out.push(Token { kind: Tok::Word, text: texts.join(",") });
    out.extend_from_slice(&tokens[last + 1..]);
    out
}

/// Drops `SET ANSI_NULLS ON|OFF` and `SET QUOTED_IDENTIFIER ON|OFF`.
fn remove_set_options(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].is_word("SET") {
            let sig: Vec<usize> = (i + 1..tokens.len()).filter(|&j| !tokens[j].is_trivia()).take(2).collect();
            if sig.len() == 2
                && (tokens[sig[0]].is_word("ANSI_NULLS") || tokens[sig[0]].is_word("QUOTED_IDENTIFIER"))
                && (tokens[sig[1]].is_word("ON") || tokens[sig[1]].is_word("OFF"))
            {
                i = sig[1] + 1;
                continue;
            }
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// Removes every occurrence of a word sequence (trivia between the words allowed).
fn remove_words(tokens: Vec<Token>, words: &[&str]) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    'outer: while i < tokens.len() {
        if tokens[i].is_word(words[0]) {
            let mut j = i + 1;
            for w in &words[1..] {
                while j < tokens.len() && tokens[j].is_trivia() {
                    j += 1;
                }
                if j >= tokens.len() || !tokens[j].is_word(w) {
                    out.push(tokens[i].clone());
                    i += 1;
                    continue 'outer;
                }
                j += 1;
            }
            i = j;
            continue;
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// Removes parentheses that do not change meaning: around the whole expression, and around a
/// single token or another parenthesised group that is not a function call's argument list.
fn strip_parentheses(mut t: Vec<Token>) -> Vec<Token> {
    loop {
        let before = t.len();
        // Outer parentheses around everything.
        while t.len() >= 2 && t[0].text == "(" && t[t.len() - 1].text == ")" && matching(&t, 0) == Some(t.len() - 1) {
            t = t[1..t.len() - 1].to_vec();
        }
        let mut i = 0;
        while i < t.len() {
            if t[i].text == "(" {
                let call = i > 0 && matches!(t[i - 1].kind, Tok::Word | Tok::Quoted) && !is_operator_word(&t[i - 1]);
                if let Some(close) = matching(&t, i) {
                    let inner_is_group = t[i + 1].text == "(" && matching(&t, i + 1) == Some(close - 1);
                    let single = close == i + 2 && t[i + 1].text != "(";
                    // `(( x ))` → `( x )`, also in a call: `f((x))` → `f(x)`.
                    if inner_is_group || (single && !call) {
                        t.remove(close);
                        t.remove(i);
                        continue;
                    }
                }
            }
            i += 1;
        }
        if t.len() == before {
            return t;
        }
    }
}

fn is_operator_word(t: &Token) -> bool {
    ["AND", "OR", "NOT", "IN", "IS", "LIKE", "BETWEEN", "EXISTS", "WHEN", "THEN", "ELSE", "CASE", "SELECT"]
        .iter()
        .any(|w| t.is_word(w))
}

fn matching(t: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, tok) in t.iter().enumerate().skip(open) {
        match tok.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `CONVERT(t, x, 0)` equals `CONVERT(t, x)`.
fn drop_convert_style_zero(mut t: Vec<Token>) -> Vec<Token> {
    let mut i = 0;
    while i + 1 < t.len() {
        if t[i].is_word("CONVERT") && t[i + 1].text == "("
            && let Some(close) = matching(&t, i + 1)
            && close >= 3
            && t[close - 1].text == "0"
            && t[close - 2].text == ","
        {
            t.drain(close - 2..close);
        }
        i += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::options::Options;

    fn eq(a: &str, b: &str, kind: TextKind, opts: &Options) -> bool {
        compare(a, b, kind, opts).is_none()
    }

    #[test]
    fn module_whitespace_and_header() {
        let d = Options::default_set();
        assert!(eq("CREATE VIEW dbo.v AS SELECT 1 AS a", "create   view [dbo].[v]\nAS\n  SELECT 1 AS a", TextKind::Module, &d));
        assert!(eq("CREATE OR ALTER PROCEDURE p AS SELECT 1", "CREATE PROCEDURE p AS SELECT 1", TextKind::Module, &d));
        // Renamed with sp_rename: the header still says the old name.
        assert!(eq("CREATE PROCEDURE dbo.OldName AS SELECT 1", "CREATE PROCEDURE dbo.NewName AS SELECT 1", TextKind::Module, &d));
        assert!(!eq("CREATE VIEW v AS SELECT 1", "CREATE VIEW v AS SELECT 2", TextKind::Module, &d));
        // Without IgnoreWhiteSpace, a changed line break is a difference.
        let none = Options::none();
        assert!(!eq("CREATE VIEW v AS SELECT 1", "CREATE VIEW v AS\nSELECT 1", TextKind::Module, &none));
        assert!(eq("CREATE VIEW v AS\r\nSELECT 1", "CREATE VIEW v AS\nSELECT 1", TextKind::Module, &none));
    }

    #[test]
    fn trigger_header_brackets() {
        let o = Options::default_set().with(Opt::IgnoreOwners);
        let a = "CREATE TRIGGER dbo.T_Ins ON dbo.T AFTER INSERT AS SELECT 1";
        let b = "CREATE TRIGGER [dbo].[T_Ins] ON [dbo].[T] AFTER INSERT AS SELECT 1";
        assert!(eq(a, b, TextKind::Module, &o));
        assert!(!eq(a, "CREATE TRIGGER dbo.T_Ins ON dbo.U AFTER INSERT AS SELECT 1", TextKind::Module, &o));
    }

    #[test]
    fn comments_and_brackets_follow_options() {
        let d = Options::default_set();
        let a = "CREATE VIEW v AS SELECT a -- note\nFROM t";
        let b = "CREATE VIEW v AS SELECT a FROM t";
        assert!(!eq(a, b, TextKind::Module, &d));
        assert!(eq(a, b, TextKind::Module, &d.clone().with(Opt::IgnoreComments)));
        let c = "CREATE VIEW v AS SELECT [a] FROM [t]";
        assert!(!eq(b, c, TextKind::Module, &d));
        assert!(eq(b, c, TextKind::Module, &d.with(Opt::IgnoreSquareBrackets)));
    }

    #[test]
    fn with_clause_order() {
        let d = Options::default_set();
        let a = "CREATE VIEW v WITH SCHEMABINDING, VIEW_METADATA AS SELECT 1 AS a";
        let b = "CREATE VIEW v WITH VIEW_METADATA, SCHEMABINDING AS SELECT 1 AS a";
        assert!(eq(a, b, TextKind::Module, &d));
        assert!(!eq(a, b, TextKind::Module, &d.without(Opt::IgnoreWithElementOrder)));
    }

    #[test]
    fn expressions() {
        let d = Options::default_set();
        assert!(eq("((0))", "(0)", TextKind::Expression, &d));
        assert!(eq("(getdate())", "getdate()", TextKind::Expression, &d));
        assert!(eq("([Balance]>=(0))", "(Balance >= 0)", TextKind::Expression, &d));
        assert!(eq("(CONVERT([varchar](10),[a],(0)))", "(convert(varchar(10),a))", TextKind::Expression, &d));
        assert!(eq("(len([Name]))", "len(Name)", TextKind::Expression, &d));
        assert!(!eq("((1))", "((2))", TextKind::Expression, &d));
        // A function call keeps its argument list.
        assert!(!eq("(f(1))", "(f 1)", TextKind::Expression, &d));
        // Grouping that changes precedence is kept.
        assert!(!eq("((a+b)*c)", "(a+b*c)", TextKind::Expression, &d));
    }

    #[test]
    fn case() {
        let d = Options::default_set();
        assert!(eq("(UPPER([a]))", "(upper(a))", TextKind::Expression, &d));
        assert!(!eq("(UPPER([a]))", "(upper(a))", TextKind::Expression, &d.clone().with(Opt::CaseSensitiveObjectDefinition)));
        // Literals are data: always exact.
        assert!(!eq("('abc')", "('ABC')", TextKind::Expression, &d));
    }
}
