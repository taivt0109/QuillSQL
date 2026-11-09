//! Code analysis: token-level checks with stable rule codes grouped by prefix (BP, DEP, EI, MI,
//! PE, ST, SC) so that suppressions and team conventions carry over. Messages are our own.
//! Where a fix cannot change what the script does, the issue carries it.

use std::collections::HashMap;

use serde::Serialize;

use crate::complete::{Analysis, Schema, Source, bare, eq, is_name, quote, resolve};
use crate::keywords::is_reserved;
use crate::lexer::Kind;
use crate::refactor::{Qualified, end_of, owning_select, qualify_edits, query_range, wildcard_edits};
use crate::transform::{next_sig, prev_sig};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fix {
    pub title: String,
    /// UTF-16 range to replace.
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub start: usize,
    pub end: usize,
    pub fix: Option<Fix>,
}

/// Every rule this module checks: code, severity and title.
pub const RULES: &[(&str, &str, &str)] = &[
    ("BP002", "warning", "ORDER BY uses column positions"),
    ("BP004", "warning", "INSERT has no column list"),
    ("BP005", "warning", "SELECT * in a select list"),
    ("BP006", "warning", "TOP without ORDER BY"),
    ("BP007", "warning", "Variable-length type without a length"),
    ("BP008", "warning", "CAST or CONVERT to a variable-length type without a length"),
    ("BP009", "info", "Variable-length type of length 1 or 2"),
    ("BP010", "warning", "@@IDENTITY can return a value from a trigger"),
    ("BP011", "warning", "Comparison with NULL is never true"),
    ("BP012", "warning", "CASE without ELSE"),
    ("BP013", "warning", "EXECUTE of a string"),
    ("BP015", "info", "Cursor scope not specified"),
    ("BP016", "info", "RETURN without a value in a procedure"),
    ("BP017", "warning", "DELETE without WHERE"),
    ("BP018", "warning", "UPDATE without WHERE"),
    ("BP022", "info", "MONEY or SMALLMONEY type"),
    ("BP023", "info", "FLOAT or REAL type"),
    ("BP024", "info", "SQL_VARIANT type"),
    ("DEP001", "warning", "Table hint without WITH"),
    ("DEP003", "warning", "GROUP BY ALL is deprecated"),
    ("DEP006", "warning", "SETUSER is deprecated"),
    ("DEP014", "warning", "SET ROWCOUNT is deprecated for INSERT, UPDATE and DELETE"),
    ("DEP019", "warning", "Deprecated system table"),
    ("DEP021", "warning", "String literal used as a column alias"),
    ("DEP025", "warning", "Deprecated system procedure"),
    ("EI024", "warning", "Procedure name starts with sp_"),
    ("EI029", "info", "ISNUMERIC accepts values that do not convert"),
    ("EI030", "warning", "ORDER BY in a view without TOP"),
    ("MI001", "info", "Table variable is declared but not used"),
    ("MI002", "info", "Temporary table is created but not used"),
    ("MI003", "info", "Unqualified column name"),
    ("MI005", "info", "Variable is declared but not used"),
    ("MI007", "info", "WAITFOR is used"),
    ("PE001", "info", "Procedure called without a schema"),
    ("PE002", "info", "Table or view referenced without a schema"),
    ("PE003", "info", "SELECT INTO creates a table"),
    ("PE004", "info", "Index hint"),
    ("PE005", "info", "Join hint"),
    ("PE006", "info", "Table hint"),
    ("PE007", "info", "Query hint"),
    ("PE008", "info", "SET NOCOUNT OFF"),
    ("PE013", "info", "COUNT used to test for rows; EXISTS is cheaper"),
    ("PE014", "info", "SET FORCEPLAN"),
    ("PE019", "info", "IN (SELECT ...); consider EXISTS"),
    ("PE021", "info", "WITH RECOMPILE"),
    ("ST001", "info", "Old-style join (FROM a, b)"),
    ("ST002", "info", "Column alias assigned with ="),
    ("ST003", "info", "Procedure body is not inside BEGIN...END"),
    ("ST006", "info", "TOP without parentheses"),
    ("ST008", "info", "Procedure called with positional parameters"),
    ("ST010", "info", "Table source without an alias"),
    ("ST011", "info", "Temporary table; a table variable may do"),
    ("ST013", "info", "Non-ANSI != operator"),
    ("SC004", "info", "TODO, FIXME or BUG comment"),
];

fn rule(code: &str) -> (&'static str, &'static str, &'static str) {
    *RULES.iter().find(|r| r.0 == code).expect("known rule")
}

const VARIABLE_LENGTH: &[&str] = &["VARCHAR", "NVARCHAR", "CHAR", "NCHAR", "VARBINARY", "BINARY"];

const TABLE_HINTS: &[&str] = &[
    "NOLOCK", "READUNCOMMITTED", "READCOMMITTED", "READCOMMITTEDLOCK", "REPEATABLEREAD", "SERIALIZABLE", "HOLDLOCK",
    "UPDLOCK", "XLOCK", "ROWLOCK", "PAGLOCK", "TABLOCK", "TABLOCKX", "NOWAIT", "READPAST", "FORCESEEK", "FORCESCAN",
    "SNAPSHOT", "NOEXPAND", "INDEX",
];

const DEPRECATED_TABLES: &[&str] = &[
    "SYSOBJECTS", "SYSCOLUMNS", "SYSINDEXES", "SYSUSERS", "SYSDATABASES", "SYSPROCESSES", "SYSCOMMENTS", "SYSTYPES",
    "SYSLOGINS", "SYSFOREIGNKEYS", "SYSCONSTRAINTS", "SYSFILES", "SYSPERMISSIONS", "SYSPROTECTS", "SYSDEPENDS",
];

const DEPRECATED_PROCEDURES: &[&str] = &[
    "SP_ADDLOGIN", "SP_DROPLOGIN", "SP_ADDUSER", "SP_DROPUSER", "SP_GRANTDBACCESS", "SP_REVOKEDBACCESS",
    "SP_ADDROLE", "SP_DROPROLE", "SP_DBOPTION", "SP_ATTACH_DB", "SP_CHANGEOBJECTOWNER", "SP_DEFAULTDB",
    "SP_DEFAULTLANGUAGE", "SP_DENYLOGIN", "SP_GRANTLOGIN", "SP_PASSWORD", "SP_ADDTYPE", "SP_LOCK",
];

struct Checker<'a> {
    text: &'a str,
    a: Analysis,
    /// UTF-16 offset of every token boundary, keyed by byte offset.
    utf16: HashMap<usize, usize>,
    issues: Vec<Issue>,
}

impl<'a> Checker<'a> {
    fn new(text: &'a str) -> Self {
        let a = Analysis::new(text);
        let mut utf16 = HashMap::new();
        let mut byte = 0;
        let mut units = 0;
        for t in &a.tokens {
            byte += t.ws.len();
            units += t.ws.encode_utf16().count();
            utf16.insert(byte, units);
            byte += t.text.len();
            units += t.text.encode_utf16().count();
            utf16.insert(byte, units);
        }
        utf16.insert(0, 0);
        Checker { text, a, utf16, issues: Vec::new() }
    }

    fn u16(&self, byte: usize) -> usize {
        self.utf16.get(&byte).copied().unwrap_or_else(|| self.text[..byte].encode_utf16().count())
    }

    fn report(&mut self, code: &str, from: usize, to: usize, detail: Option<String>, fix: Option<(usize, usize, String, &str)>) {
        let (code, severity, title) = rule(code);
        let start = self.u16(self.a.starts.get(from).copied().unwrap_or_else(|| end_of(&self.a, from)));
        let end = self.u16(end_of(&self.a, to));
        let fix = fix.map(|(s, e, text, title)| Fix { title: title.to_string(), start: self.u16(s), end: self.u16(e), text });
        let message = match detail {
            Some(d) => format!("{title}: {d}"),
            None => title.to_string(),
        };
        self.issues.push(Issue { code, severity, message, start, end, fix });
    }

    /// The word at token `i` in upper case; empty for another kind of token or past the end (a
    /// statement still being typed, such as `INSERT `).
    fn word(&self, i: usize) -> &str {
        match self.a.tokens.get(i) {
            Some(t) if t.kind == Kind::Word => &self.a.up[i],
            _ => "",
        }
    }

    fn next_word(&self, i: usize) -> &str {
        next_sig(&self.a.tokens, i).map_or("", |n| self.word(n))
    }

    fn prev_word(&self, i: usize) -> &str {
        prev_sig(&self.a.tokens, i).map_or("", |p| self.word(p))
    }

    /// True when token `i` is a data type: after a variable or column name in a
    /// declaration, after `AS` in CAST, or after RETURNS.
    fn is_type_position(&self, stmt: (usize, usize), i: usize) -> bool {
        let Some(p) = prev_sig(&self.a.tokens, i) else { return false };
        let first = self.word(stmt.0);
        match self.a.tokens[p].kind {
            Kind::Variable => true,
            Kind::Word if self.a.up[p] == "AS" => {
                // CAST(x AS type) or `DECLARE @x AS type`
                prev_sig(&self.a.tokens, p).is_some_and(|q| self.a.tokens[q].kind == Kind::Variable) || self.in_cast(stmt, i)
            }
            Kind::Word if self.a.up[p] == "RETURNS" => true,
            Kind::Word | Kind::Bracket | Kind::Quoted if !is_reserved(self.word(p)) => {
                matches!(first, "CREATE" | "ALTER" | "DECLARE") && self.inside_column_list(stmt, i)
            }
            Kind::LParen => self.word_before_paren(p) == "CONVERT",
            _ => false,
        }
    }

    fn word_before_paren(&self, open: usize) -> &str {
        self.prev_word(open)
    }

    fn enclosing_paren(&self, stmt: (usize, usize), i: usize) -> Option<usize> {
        let mut depth = 0usize;
        for j in (stmt.0..i).rev() {
            match self.a.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen if depth == 0 => return Some(j),
                Kind::LParen => depth -= 1,
                _ => {}
            }
        }
        None
    }

    fn in_cast(&self, stmt: (usize, usize), i: usize) -> bool {
        self.enclosing_paren(stmt, i).is_some_and(|p| matches!(self.word_before_paren(p), "CAST" | "TRY_CAST"))
    }

    /// Inside `CREATE TABLE t (` or `DECLARE @t TABLE (`, at column-definition level.
    fn inside_column_list(&self, stmt: (usize, usize), i: usize) -> bool {
        self.enclosing_paren(stmt, i).is_some_and(|p| {
            let before = prev_sig(&self.a.tokens, p);
            before.is_some_and(|b| self.word(b) == "TABLE" || is_name(&self.a.tokens[b]))
                && (stmt.0..p).any(|j| self.word(j) == "TABLE")
        })
    }

    fn statement_has(&self, stmt: (usize, usize), words: &[&str]) -> bool {
        let mut depth = 0usize;
        for j in stmt.0..stmt.1 {
            match self.a.tokens[j].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => depth = depth.saturating_sub(1),
                Kind::Word if depth == 0 && words.contains(&self.a.up[j].as_str()) => return true,
                _ => {}
            }
        }
        false
    }

    fn check_tokens(&mut self, stmt: (usize, usize), in_procedure: bool) {
        let first = self.word(stmt.0).to_string();
        for i in stmt.0..stmt.1 {
            let t = self.a.tokens[i].clone();
            let w = self.word(i).to_string();
            match t.kind {
                Kind::Op if t.text == "!=" => {
                    self.report("ST013", i, i, None, Some((self.a.starts[i], end_of(&self.a, i), "<>".into(), "Use <>")));
                }
                Kind::Variable if t.text.eq_ignore_ascii_case("@@IDENTITY") => {
                    self.report("BP010", i, i, Some("use SCOPE_IDENTITY()".into()), Some((self.a.starts[i], end_of(&self.a, i), "SCOPE_IDENTITY()".into(), "Use SCOPE_IDENTITY()")));
                }
                Kind::LineComment | Kind::BlockComment => {
                    let upper = t.text.to_ascii_uppercase();
                    let tagged = ["TODO", "FIXME", "BUG", "HACK"].iter().any(|tag| {
                        upper.match_indices(tag).any(|(at, _)| {
                            let before = upper[..at].chars().last();
                            let after = upper[at + tag.len()..].chars().next();
                            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
                        })
                    });
                    if tagged {
                        self.report("SC004", i, i, None, None);
                    }
                }
                _ => {}
            }
            if t.kind == Kind::Op && matches!(t.text.as_str(), "=" | "<>" | "!=") && self.next_word(i) == "NULL" && self.is_condition(stmt, i) {
                let null = next_sig(&self.a.tokens, i).expect("NULL");
                let replacement = if t.text == "=" { "IS NULL" } else { "IS NOT NULL" };
                self.report("BP011", i, null, Some(format!("use {replacement}")), Some((self.a.starts[i], end_of(&self.a, null), replacement.into(), "Use IS [NOT] NULL")));
            }
            if t.kind != Kind::Word && t.kind != Kind::Bracket {
                continue;
            }
            let bare_upper = bare(&t).to_ascii_uppercase();
            match w.as_str() {
                "ISNUMERIC" if self.a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::LParen) => self.report("EI029", i, i, Some("TRY_CONVERT is stricter".into()), None),
                "SETUSER" => self.report("DEP006", i, i, None, None),
                "WAITFOR" => self.report("MI007", i, i, None, None),
                "OPTION" if self.a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::LParen) && first != "CREATE" => self.report("PE007", i, i, None, None),
                "EXEC" | "EXECUTE" if self.a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::LParen) => self.report("BP013", i, i, Some("prefer sp_executesql with parameters".into()), None),
                "RECOMPILE" if self.prev_word(i) == "WITH" => self.report("PE021", i, i, None, None),
                "LOOP" | "HASH" | "MERGE" | "REMOTE" if self.next_word(i) == "JOIN" => self.report("PE005", i, i, None, None),
                "GROUP" if self.next_word(i) == "BY" => {
                    let by = next_sig(&self.a.tokens, i).expect("BY");
                    if self.next_word(by) == "ALL" {
                        self.report("DEP003", i, next_sig(&self.a.tokens, by).expect("ALL"), None, None);
                    }
                }
                "SET" if i == stmt.0 => {
                    let option = self.next_word(i).to_string();
                    let value_at = next_sig(&self.a.tokens, i).and_then(|n| next_sig(&self.a.tokens, n));
                    let value = value_at.map_or(String::new(), |v| self.word(v).to_string());
                    let end = value_at.unwrap_or(i);
                    match (option.as_str(), value.as_str()) {
                        ("NOCOUNT", "OFF") => self.report("PE008", i, end, None, None),
                        ("FORCEPLAN", _) => self.report("PE014", i, end, None, None),
                        ("ROWCOUNT", _) => self.report("DEP014", i, end, Some("use TOP".into()), None),
                        _ => {}
                    }
                }
                "TOP" => {
                    if let Some(n) = next_sig(&self.a.tokens, i).filter(|&n| matches!(self.a.tokens[n].kind, Kind::Number | Kind::Variable)) {
                        let fixed = format!("({})", self.a.tokens[n].text);
                        self.report("ST006", i, n, None, Some((self.a.starts[n], end_of(&self.a, n), fixed, "Add parentheses")));
                    }
                    if matches!(self.prev_word(i), "SELECT" | "DISTINCT" | "ALL") {
                        let (q, q_end) = query_range(&self.a, stmt, i);
                        if !self.statement_has((q, q_end), &["ORDER"]) {
                            self.report("BP006", i, i, None, None);
                        }
                    }
                }
                "CASE" => {
                    let mut level = 0i32;
                    let mut has_else = false;
                    for j in i..stmt.1 {
                        match self.word(j) {
                            "CASE" => level += 1,
                            "ELSE" if level == 1 => has_else = true,
                            "END" => {
                                level -= 1;
                                if level == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if !has_else {
                        self.report("BP012", i, i, Some("unmatched values become NULL".into()), None);
                    }
                }
                "RETURN" if in_procedure => {
                    let next = next_sig(&self.a.tokens, i);
                    let bare_return = next.is_none_or(|n| matches!(self.a.tokens[n].kind, Kind::Semi | Kind::Go) || (self.a.tokens[n].kind == Kind::Word && is_reserved(&self.a.up[n]) && !matches!(self.a.up[n].as_str(), "NULL" | "CASE" | "CAST" | "CONVERT")));
                    if bare_return {
                        self.report("BP016", i, i, None, None);
                    }
                }
                "CURSOR" if self.prev_word(i) != "AS" && first == "DECLARE" => {
                    let mut j = i + 1;
                    let mut scoped = false;
                    while j < stmt.1 && self.word(j) != "FOR" {
                        scoped |= matches!(self.word(j), "LOCAL" | "GLOBAL");
                        j += 1;
                    }
                    if !scoped {
                        self.report("BP015", i, i, Some("add LOCAL".into()), None);
                    }
                }
                _ => {}
            }
            let after_dbo = i >= 2 && self.a.tokens[i - 1].kind == Kind::Dot && eq(&bare(&self.a.tokens[i - 2]), "dbo");
            if DEPRECATED_TABLES.contains(&bare_upper.as_str()) && (matches!(self.prev_word(i), "FROM" | "JOIN") || after_dbo) {
                self.report("DEP019", i, i, Some(format!("use the sys catalog views instead of {}", bare(&t))), None);
            }
            if DEPRECATED_PROCEDURES.contains(&bare_upper.as_str()) && matches!(self.prev_word(i), "EXEC" | "EXECUTE") {
                self.report("DEP025", i, i, Some(bare(&t)), None);
            }
            if self.is_type_position(stmt, i) {
                let has_length = self.a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::LParen);
                if VARIABLE_LENGTH.contains(&bare_upper.as_str()) {
                    if !has_length {
                        let code = if self.in_cast(stmt, i) || self.a.tokens.get(i - 1).is_some_and(|p| p.kind == Kind::LParen) { "BP008" } else { "BP007" };
                        self.report(code, i, i, Some(format!("{} defaults to a length of 1 or 30", t.text)), None);
                    } else if self.a.tokens.get(i + 2).is_some_and(|n| n.kind == Kind::Number && matches!(n.text.as_str(), "1" | "2")) {
                        self.report("BP009", i, i + 3, None, None);
                    }
                }
                match bare_upper.as_str() {
                    "MONEY" | "SMALLMONEY" => self.report("BP022", i, i, Some("use DECIMAL".into()), None),
                    "FLOAT" | "REAL" => self.report("BP023", i, i, Some("values are approximate".into()), None),
                    "SQL_VARIANT" => self.report("BP024", i, i, None, None),
                    _ => {}
                }
            }
        }
    }

    /// True when the operator at `i` is a comparison rather than an assignment.
    fn is_condition(&self, stmt: (usize, usize), i: usize) -> bool {
        let mut depth = 0usize;
        for j in (stmt.0..i).rev() {
            match self.a.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen if depth == 0 => return self.word_before_paren(j) == "IF" || self.word_before_paren(j) == "WHERE",
                Kind::LParen => depth -= 1,
                Kind::Comma if depth == 0 => return false,
                Kind::Word if depth == 0 => match self.a.up[j].as_str() {
                    "WHERE" | "ON" | "HAVING" | "WHEN" | "AND" | "OR" | "IF" | "WHILE" | "NOT" => return true,
                    "SET" | "SELECT" | "DECLARE" | "VALUES" | "THEN" | "ELSE" | "UPDATE" => return false,
                    _ => {}
                },
                _ => {}
            }
        }
        false
    }

    fn check_statement(&mut self, stmt: (usize, usize), schema: Option<&Schema>) {
        let first = self.word(stmt.0).to_string();
        let a_sources: Vec<Source> = self.a.sources(stmt.0, stmt.1);
        match first.as_str() {
            "DELETE" | "UPDATE" if !(first == "UPDATE" && self.next_word(stmt.0) == "STATISTICS") => {
                if !self.statement_has(stmt, &["WHERE", "JOIN"]) {
                    self.report(if first == "DELETE" { "BP017" } else { "BP018" }, stmt.0, stmt.0, Some("every row is affected".into()), None);
                }
            }
            "INSERT" => {
                let mut j = stmt.0 + 1;
                if self.word(j) == "INTO" {
                    j += 1;
                }
                let target = j;
                while j < stmt.1 && ((is_name(&self.a.tokens[j]) && !is_reserved(self.word(j))) || self.a.tokens[j].kind == Kind::Dot) {
                    j += 1;
                }
                if j > target && j < stmt.1 && matches!(self.word(j), "VALUES" | "SELECT" | "EXEC" | "EXECUTE" | "WITH") {
                    let fix = schema.and_then(|s| {
                        let parts: Vec<String> = self.a.tokens[target..j].iter().filter(|t| is_name(t)).map(bare).collect();
                        let src = Source { reference: String::new(), parts, columns: Vec::new(), at: target, alias_at: None };
                        let o = resolve(&src, s)?;
                        let cols: Vec<String> = o.columns.iter().filter(|c| !c.identity && !c.computed && !matches!(c.data_type.as_str(), "timestamp" | "rowversion")).map(|c| quote(&c.name)).collect();
                        (!cols.is_empty()).then(|| {
                            let at = end_of(&self.a, j - 1);
                            (at, at, format!(" ({})", cols.join(", ")), "Add the column list")
                        })
                    });
                    self.report("BP004", stmt.0, j - 1, None, fix);
                }
            }
            "SELECT" => {
                let mut depth = 0usize;
                for j in stmt.0..stmt.1 {
                    match self.a.tokens[j].kind {
                        Kind::LParen => depth += 1,
                        Kind::RParen => depth = depth.saturating_sub(1),
                        Kind::Word if depth == 0 && self.a.up[j] == "INTO" => {
                            self.report("PE003", j, j, None, None);
                            break;
                        }
                        Kind::Word if depth == 0 && self.a.up[j] == "FROM" => break,
                        _ => {}
                    }
                }
            }
            "CREATE" | "ALTER" => {
                let kind_at = (stmt.0 + 1..stmt.1.min(stmt.0 + 4)).find(|&j| matches!(self.word(j), "PROC" | "PROCEDURE" | "VIEW" | "TABLE"));
                if let Some(k) = kind_at {
                    match self.word(k) {
                        "PROC" | "PROCEDURE" => {
                            if let Some(n) = next_sig(&self.a.tokens, k) {
                                let mut last = n;
                                while last + 2 < stmt.1 && self.a.tokens[last + 1].kind == Kind::Dot {
                                    last += 2;
                                }
                                if bare(&self.a.tokens[last]).to_ascii_lowercase().starts_with("sp_") {
                                    self.report("EI024", n, last, Some("SQL Server looks in master first".into()), None);
                                }
                            }
                            if let Some(as_at) = (k..stmt.1).find(|&j| self.word(j) == "AS")
                                && next_sig(&self.a.tokens, as_at).is_some_and(|b| self.word(b) != "BEGIN") {
                                    self.report("ST003", k, k, None, None);
                                }
                        }
                        "VIEW" => {
                            if self.statement_has(stmt, &["ORDER"]) && !(stmt.0..stmt.1).any(|j| self.word(j) == "TOP") {
                                let order = (stmt.0..stmt.1).find(|&j| self.word(j) == "ORDER").expect("ORDER");
                                self.report("EI030", order, order, Some("views have no order; ORDER BY there is an error or ignored".into()), None);
                            }
                        }
                        "TABLE"
                            if next_sig(&self.a.tokens, k).is_some_and(|n| self.a.tokens[n].text.starts_with('#') && !self.a.tokens[n].text.starts_with("##")) => {
                                self.report("ST011", k, next_sig(&self.a.tokens, k).expect("name"), None, None);
                            }
                        _ => {}
                    }
                }
            }
            "EXEC" | "EXECUTE" => {
                let mut j = stmt.0 + 1;
                if self.a.tokens.get(j).is_some_and(|t| t.kind == Kind::Variable) && self.a.tokens.get(j + 1).is_some_and(|t| t.text == "=") {
                    j += 2;
                }
                if j < stmt.1 && is_name(&self.a.tokens[j]) {
                    let single = self.a.tokens.get(j + 1).is_none_or(|t| t.kind != Kind::Dot);
                    let name = bare(&self.a.tokens[j]);
                    let system = name.to_ascii_lowercase().starts_with("sp_") || name.to_ascii_lowercase().starts_with("xp_");
                    if single && !system && !is_reserved(&self.a.up[j]) {
                        self.report("PE001", j, j, None, None);
                    }
                    let mut k = j;
                    while k + 2 < stmt.1 && self.a.tokens[k + 1].kind == Kind::Dot {
                        k += 2;
                    }
                    let arg = next_sig(&self.a.tokens, k).filter(|&n| !matches!(self.a.tokens[n].kind, Kind::Semi | Kind::Go) && !(self.a.tokens[n].kind == Kind::Word && is_reserved(&self.a.up[n]) && self.a.up[n] != "NULL"));
                    if let Some(n) = arg {
                        let named = self.a.tokens[n].kind == Kind::Variable && self.a.tokens.get(n + 1).is_some_and(|t| t.text == "=");
                        if !named && !system {
                            self.report("ST008", n, n, None, None);
                        }
                    }
                }
            }
            _ => {}
        }

        // Sources: schema, hints, aliases, old-style joins.
        let multi = a_sources.iter().filter(|s| s.alias_at.is_some() || !s.parts.is_empty()).count() > 1;
        for s in &a_sources {
            if s.parts.is_empty() {
                continue;
            }
            let name_end = {
                let mut j = s.at;
                while j + 2 < stmt.1 && self.a.tokens[j + 1].kind == Kind::Dot {
                    j += 2;
                }
                j
            };
            let next = next_sig(&self.a.tokens, name_end);
            if let Some(open) = next.filter(|&n| self.a.tokens[n].kind == Kind::LParen) {
                let hint = next_sig(&self.a.tokens, open).map_or("", |h| self.word(h));
                if TABLE_HINTS.contains(&hint) {
                    self.report("DEP001", open, self.a.matching(open, stmt.1), None, Some((self.a.starts[open], self.a.starts[open], "WITH ".into(), "Add WITH")));
                }
            }
            if multi && s.alias_at.is_none() && self.word(stmt.0) == "SELECT" && !s.parts[0].starts_with('@') {
                self.report("ST010", s.at, name_end, None, None);
            }
        }
        for j in stmt.0..stmt.1 {
            if self.word(j) == "WITH" && self.a.tokens.get(j + 1).is_some_and(|t| t.kind == Kind::LParen) && prev_sig(&self.a.tokens, j).is_some_and(|p| matches!(self.a.tokens[p].kind, Kind::Word | Kind::Bracket | Kind::Quoted)) {
                let hint = next_sig(&self.a.tokens, j + 1).map_or("", |h| self.word(h));
                if hint == "INDEX" {
                    self.report("PE004", j, self.a.matching(j + 1, stmt.1), None, None);
                } else if TABLE_HINTS.contains(&hint) {
                    self.report("PE006", j, self.a.matching(j + 1, stmt.1), None, None);
                }
            }
            if self.word(j) == "FROM" {
                let mut depth = 0usize;
                for k in j + 1..stmt.1 {
                    match self.a.tokens[k].kind {
                        Kind::LParen => depth += 1,
                        Kind::RParen if depth == 0 => break,
                        Kind::RParen => depth -= 1,
                        Kind::Comma if depth == 0 => {
                            self.report("ST001", k, k, Some("use JOIN".into()), None);
                            break;
                        }
                        Kind::Word if depth == 0 && matches!(self.a.up[k].as_str(), "WHERE" | "GROUP" | "ORDER" | "HAVING" | "UNION" | "EXCEPT" | "INTERSECT" | "OPTION" | "FOR" | "SELECT") => break,
                        _ => {}
                    }
                }
            }
            if self.word(j) == "IN" && self.a.tokens.get(j + 1).is_some_and(|t| t.kind == Kind::LParen) && self.a.up.get(j + 2).is_some_and(|w| w == "SELECT") {
                self.report("PE019", j, j, None, None);
            }
            if self.word(j) == "COUNT" && self.a.tokens.get(j + 1).is_some_and(|t| t.kind == Kind::LParen) {
                let close = self.a.matching(j + 1, stmt.1);
                let mut after = next_sig(&self.a.tokens, close);
                // `(SELECT COUNT(*) FROM t) > 0`
                if after.is_some_and(|n| self.word(n) == "FROM") {
                    let open = self.enclosing_paren(stmt, j);
                    after = open.and_then(|o| next_sig(&self.a.tokens, self.a.matching(o, stmt.1)));
                }
                let compares_to_zero = after.is_some_and(|op| {
                    matches!(self.a.tokens[op].text.as_str(), ">" | "<>" | "!=" | "=" | ">=") && next_sig(&self.a.tokens, op).is_some_and(|v| matches!(self.a.tokens[v].text.as_str(), "0" | "1"))
                });
                if compares_to_zero && matches!(first.as_str(), "IF" | "WHILE") {
                    self.report("PE013", j, close, None, None);
                }
            }
            if self.word(j) == "ORDER" && self.next_word(j) == "BY" {
                let by = next_sig(&self.a.tokens, j).expect("BY");
                let mut k = by;
                while let Some(n) = next_sig(&self.a.tokens, k) {
                    if n >= stmt.1 {
                        break;
                    }
                    let t = &self.a.tokens[n];
                    let follows = next_sig(&self.a.tokens, n).map(|f| &self.a.tokens[f]);
                    if t.kind == Kind::Number && follows.is_none_or(|f| f.kind == Kind::Comma || matches!(f.kind, Kind::Semi | Kind::RParen | Kind::Go) || (f.kind == Kind::Word && matches!(f.text.to_ascii_uppercase().as_str(), "ASC" | "DESC" | "OFFSET"))) {
                        self.report("BP002", n, n, None, None);
                    }
                    // Next list item.
                    let mut depth = 0usize;
                    let mut m = n;
                    let mut next_item = None;
                    while m < stmt.1 {
                        match self.a.tokens[m].kind {
                            Kind::LParen => depth += 1,
                            Kind::RParen if depth == 0 => break,
                            Kind::RParen => depth -= 1,
                            Kind::Comma if depth == 0 => {
                                next_item = Some(m);
                                break;
                            }
                            Kind::Word if depth == 0 && matches!(self.a.up[m].as_str(), "OFFSET" | "OPTION" | "FOR") => break,
                            _ => {}
                        }
                        m += 1;
                    }
                    match next_item {
                        Some(c) => k = c,
                        None => break,
                    }
                }
            }
        }

        // String literals as aliases, and `alias = expression`.
        for j in stmt.0..stmt.1 {
            if owning_select(&self.a, stmt.0, j).is_none() {
                continue;
            }
            let t = &self.a.tokens[j];
            let prev = prev_sig(&self.a.tokens, j);
            let next = next_sig(&self.a.tokens, j);
            let after_top = |p: usize| match self.a.tokens[p].kind {
                Kind::Number => self.prev_word(p) == "TOP",
                Kind::RParen => (stmt.0..p).rev().find(|&o| self.a.tokens[o].kind == Kind::LParen && self.a.matching(o, stmt.1) == p).is_some_and(|o| self.prev_word(o) == "TOP"),
                _ => false,
            };
            let item_start = prev.is_some_and(|p| matches!(self.a.tokens[p].kind, Kind::Comma) || matches!(self.word(p), "SELECT" | "DISTINCT") || after_top(p));
            let assigns = next.is_some_and(|n| self.a.tokens[n].kind == Kind::Op && self.a.tokens[n].text == "=");
            if t.kind == Kind::Str && !t.text.starts_with('N') {
                let alias_after = prev.is_some_and(|p| self.word(p) == "AS" || matches!(self.a.tokens[p].kind, Kind::Word | Kind::Bracket | Kind::RParen | Kind::Number) && !item_start && !is_reserved(self.word(p)))
                    && next.is_none_or(|n| matches!(self.a.tokens[n].kind, Kind::Comma) || self.word(n) == "FROM" || self.word(n) == "INTO");
                if (item_start && assigns) || alias_after {
                    let inner = &t.text[1..t.text.len() - 1];
                    let fix = (!inner.contains(']')).then(|| (self.a.starts[j], end_of(&self.a, j), format!("[{}]", inner.replace("''", "'")), "Use a bracketed name"));
                    self.report("DEP021", j, j, None, fix);
                }
            } else if is_name(t) && item_start && assigns && !(t.kind == Kind::Word && is_reserved(&self.a.up[j])) {
                self.report("ST002", j, j, None, None);
            }
        }
    }

    fn check_unused(&mut self) {
        let a = &self.a;
        let batches = {
            let mut out = Vec::new();
            let mut start = 0;
            for (i, t) in a.tokens.iter().enumerate() {
                if t.kind == Kind::Go {
                    out.push((start, i));
                    start = i + 1;
                }
            }
            out.push((start, a.tokens.len()));
            out
        };
        let mut found: Vec<(&'static str, usize)> = Vec::new();
        for (s, e) in batches {
            for i in s..e {
                let t = &a.tokens[i];
                let declared_var = t.kind == Kind::Variable
                    && prev_sig(&a.tokens, i).is_some_and(|p| a.up[p] == "DECLARE" || (a.tokens[p].kind == Kind::Comma && (s..p).rev().find(|&q| a.tokens[q].kind == Kind::Word && a.up[q] == "DECLARE").is_some_and(|d| !(d..p).any(|q| matches!(a.tokens[q].kind, Kind::Semi)) && !(d..p).any(|q| a.up[q] == "SELECT" || a.up[q] == "SET"))));
                let temp_table = matches!(t.kind, Kind::Word) && t.text.starts_with('#') && !t.text.starts_with("##") && prev_sig(&a.tokens, i).is_some_and(|p| a.up[p] == "TABLE") && prev_sig(&a.tokens, i).and_then(|p| prev_sig(&a.tokens, p)).is_some_and(|q| a.up[q] == "CREATE");
                if !(declared_var || temp_table) {
                    continue;
                }
                let uses = (s..e).filter(|&j| j != i && a.tokens[j].kind == t.kind && eq(&a.tokens[j].text, &t.text)).count();
                if uses == 0 {
                    let is_table = declared_var && next_sig(&a.tokens, i).is_some_and(|n| a.up[n] == "TABLE" || (a.up[n] == "AS" && next_sig(&a.tokens, n).is_some_and(|m| a.up[m] == "TABLE")));
                    let is_cursor = declared_var && next_sig(&a.tokens, i).is_some_and(|n| a.up[n] == "CURSOR");
                    if is_cursor {
                        continue;
                    }
                    found.push((if temp_table { "MI002" } else if is_table { "MI001" } else { "MI005" }, i));
                }
            }
        }
        for (code, i) in found {
            let name = self.a.tokens[i].text.clone();
            self.report(code, i, i, Some(name), None);
        }
    }
}

/// Every issue in the script, ordered by position.
pub fn analyze(text: &str, schema: Option<&Schema>) -> Vec<Issue> {
    let mut c = Checker::new(text);
    let statements = c.a.statements();
    let mut in_procedure = false;
    for &stmt in &statements {
        if stmt.0 >= stmt.1 {
            continue;
        }
        let first = c.word(stmt.0).to_string();
        if c.a.tokens[stmt.0].kind == Kind::Go {
            in_procedure = false;
        }
        if matches!(first.as_str(), "CREATE" | "ALTER") && (stmt.0 + 1..stmt.1.min(stmt.0 + 4)).any(|j| matches!(c.word(j), "PROC" | "PROCEDURE")) {
            in_procedure = true;
        }
        c.check_tokens(stmt, in_procedure);
        c.check_statement(stmt, schema);
        if stmt.1 > 0 && c.a.tokens[stmt.1 - 1].kind == Kind::Go {
            in_procedure = false;
        }
    }
    c.check_unused();

    // Rules that reuse the refactorings.
    let (stars, _) = wildcard_edits(&c.a, text, schema, None);
    for (from, to, replacement) in stars {
        let i = (0..c.a.tokens.len()).find(|&i| c.a.starts[i] == from).expect("token start");
        let j = (i..c.a.tokens.len()).find(|&j| end_of(&c.a, j) == to).expect("token end");
        // `EXISTS (SELECT * ...)` reads no columns, so it is fine.
        let select = owning_select(&c.a, 0, i);
        let in_exists = select.and_then(|s| prev_sig(&c.a.tokens, s)).filter(|&p| c.a.tokens[p].kind == Kind::LParen).is_some_and(|p| c.word_before_paren(p) == "EXISTS");
        if !in_exists {
            c.report("BP005", i, j, None, Some((from, to, replacement, "Expand the wildcard")));
        }
    }
    for i in 0..c.a.tokens.len() {
        let t = &c.a.tokens[i];
        let star = t.kind == Kind::Op && t.text == "*";
        if star && !c.issues.iter().any(|x| x.code == "BP005" && x.start <= c.u16(c.a.starts[i]) && c.u16(c.a.starts[i]) < x.end) {
            // Wildcards whose columns are unknown are still reported, without a fix.
            let stmt = c.a.statements().into_iter().find(|&(s, e)| s <= i && i < e).unwrap_or((0, c.a.tokens.len()));
            let prev = prev_sig(&c.a.tokens, i);
            let listed = prev.is_some_and(|p| matches!(c.a.tokens[p].kind, Kind::Comma | Kind::Dot) || matches!(c.word(p), "SELECT" | "DISTINCT"));
            if listed && owning_select(&c.a, stmt.0, i).is_some() {
                let select = owning_select(&c.a, stmt.0, i).expect("select");
                let in_exists = prev_sig(&c.a.tokens, select).filter(|&p| c.a.tokens[p].kind == Kind::LParen).is_some_and(|p| c.word_before_paren(p) == "EXISTS");
                if !in_exists {
                    c.report("BP005", i, i, None, None);
                }
            }
        }
    }
    let unqualified: Vec<(usize, String, Qualified)> = match schema {
        Some(s) => qualify_edits(&c.a, s),
        None => Vec::new(),
    };
    for (at, prefix, what) in &unqualified {
        let i = (0..c.a.tokens.len()).find(|&i| c.a.starts[i] == *at).expect("token start");
        match what {
            Qualified::Column => c.report("MI003", i, i, None, Some((*at, *at, prefix.clone(), "Qualify the column"))),
            Qualified::Object => c.report("PE002", i, i, None, Some((*at, *at, prefix.clone(), "Add the schema"))),
            Qualified::Procedure => {
                if let Some(x) = c.issues.iter_mut().find(|x| x.code == "PE001" && x.start == c.utf16[at]) {
                    x.fix = Some(Fix { title: "Add the schema".into(), start: x.start, end: x.start, text: prefix.clone() });
                }
            }
        }
    }
    if schema.is_none() {
        // Without metadata, flag single-part table names that are not CTEs or temp tables.
        for (s, e) in c.a.statements() {
            let ctes: Vec<String> = c.a.ctes(s, e).into_iter().map(|x| x.reference).collect();
            for src in c.a.sources(s, e) {
                if src.parts.len() == 1 && !src.parts[0].starts_with('#') && !src.parts[0].starts_with('@') && !ctes.iter().any(|x| eq(x, &src.parts[0])) && c.a.tokens[src.at].kind != Kind::LParen && c.a.tokens.get(src.at + 1).is_none_or(|t| t.kind != Kind::LParen) && !DEPRECATED_TABLES.contains(&src.parts[0].to_ascii_uppercase().as_str()) {
                    c.report("PE002", src.at, src.at, None, None);
                }
            }
        }
    }
    c.issues.sort_by(|x, y| (x.start, x.code).cmp(&(y.start, y.code)));
    c.issues.dedup_by(|x, y| x.code == y.code && x.start == y.start);
    c.issues
}

/// Applies every available fix (or only those for `codes`) that does not overlap another.
pub fn apply_fixes(text: &str, schema: Option<&Schema>, codes: Option<&[String]>) -> (String, usize) {
    let mut fixes: Vec<Fix> = analyze(text, schema)
        .into_iter()
        .filter(|i| codes.is_none_or(|c| c.iter().any(|x| x == i.code)))
        .filter_map(|i| i.fix)
        .collect();
    fixes.sort_by_key(|f| (f.start, f.end));
    let mut chosen: Vec<Fix> = Vec::new();
    for f in fixes {
        let overlaps = chosen.last().is_some_and(|l| f.start < l.end || (f.start == l.start && f.end == l.end));
        if !overlaps {
            chosen.push(f);
        }
    }
    let mut units: Vec<u16> = text.encode_utf16().collect();
    let n = chosen.len();
    for f in chosen.iter().rev() {
        units.splice(f.start..f.end, f.text.encode_utf16());
    }
    (String::from_utf16_lossy(&units), n)
}
