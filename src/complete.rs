//! Context-aware suggestions for the SSMS extension.
//!
//! The extension loads database metadata and sends it once per connection; each
//! request then carries the script text and caret position. Offsets in requests
//! and responses are UTF-16 code units, matching the editor.

use serde::{Deserialize, Serialize};

use crate::keywords::{is_reserved, FUNCTIONS};
use crate::lexer::{lex_partial, Kind, Token};
use crate::snippets::Snippet;
use crate::transform::{missing_terminators, upper_words};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Schema {
    pub database: String,
    pub default_schema: String,
    pub schemas: Vec<String>,
    pub databases: Vec<String>,
    pub objects: Vec<DbObject>,
    pub foreign_keys: Vec<ForeignKey>,
}

/// `schema.table (columns)` references `ref_schema.ref_table (ref_columns)`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ForeignKey {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    /// table, view, procedure, scalar_function, table_function or synonym.
    pub kind: String,
    pub columns: Vec<Column>,
    pub params: Vec<Param>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub nullable: bool,
    /// Identity, computed and rowversion columns cannot be inserted or updated.
    pub identity: bool,
    pub computed: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub output: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub label: String,
    pub insert: String,
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Completion {
    /// UTF-16 offset where the replaced text starts; it ends at the caret.
    pub start: usize,
    pub items: Vec<Item>,
    /// The item to select as the list opens: one that matches what was typed for sure (the whole
    /// word, its start, or the initials of its parts). None when the matches are only guesses,
    /// so Enter and Tab keep their usual meaning instead of inserting a guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<usize>,
}

/// How well a label matches what was typed: 0 the whole word, 1 its start, 2 the initials of
/// its parts (`cid` for CustomerId, `od` for OrderDetail), 3 somewhere inside; None for no match.
fn match_quality(label: &str, typed: &str) -> Option<u8> {
    let label = label.trim_start_matches('@');
    let lower = label.to_ascii_lowercase();
    let needle = typed.trim_start_matches('@').to_ascii_lowercase();
    if needle.is_empty() {
        return Some(1);
    }
    if lower == needle {
        return Some(0);
    }
    if lower.starts_with(&needle) {
        return Some(1);
    }
    if camel_match(&label_parts(label), &needle.chars().collect::<Vec<_>>()) {
        return Some(2);
    }
    if needle.len() >= 2 && lower.contains(&needle) {
        return Some(3);
    }
    None
}

/// The parts of a name, lowercased: words split at underscores, dots, spaces and case changes
/// (CustomerId: customer, id; HTTPStatus: http, status).
fn label_parts(label: &str) -> Vec<Vec<char>> {
    let chars: Vec<char> = label.chars().collect();
    let mut parts: Vec<Vec<char>> = Vec::new();
    let mut current = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous = if i > 0 { chars[i - 1] } else { ' ' };
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let new_part = c.is_uppercase() && (previous.is_lowercase() || previous.is_ascii_digit() || (previous.is_uppercase() && next_lower));
        if new_part && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Whether the typed letters start the first part and then, in order, the start of later parts:
/// each part used gives one or more of its first letters (`cid`: C-ustomer, I-d; `custid`).
fn camel_match(parts: &[Vec<char>], needle: &[char]) -> bool {
    fn rest(parts: &[Vec<char>], needle: &[char]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let Some((part, later)) = parts.split_first() else { return false };
        let most = part.len().min(needle.len());
        (1..=most).rev().any(|take| part[..take] == needle[..take] && (take == needle.len() || (0..later.len()).any(|skip| rest(&later[skip..], &needle[take..]))))
    }
    parts.len() > 1 && rest(parts, needle)
}
const MAX_ITEMS: usize = 300;

const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "WITH", "DECLARE", "SET", "EXEC", "IF",
    "BEGIN", "END", "WHILE", "RETURN", "CREATE", "ALTER", "DROP", "TRUNCATE", "PRINT", "USE",
];

/// Keywords inside conditions, select lists and the clauses that follow them.
const EXPRESSION_KEYWORDS: &[&str] = &[
    "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "BETWEEN", "EXISTS", "CASE", "WHEN", "THEN", "ELSE", "END", "AS",
    "ASC", "DESC", "DISTINCT", "TOP", "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "JOIN",
    "INNER JOIN", "LEFT JOIN", "OVER", "PARTITION BY", "COLLATE", "OFFSET", "FETCH",
];

/// What may follow a table and its alias in FROM.
const CLAUSE_KEYWORDS: &[&str] = &[
    "WHERE", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "CROSS APPLY", "OUTER APPLY",
    "GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "EXCEPT", "INTERSECT", "OPTION",
];

/// Whether the name before token `k` is an alias already written after a table: `t x` or `t AS x`.
fn alias_written(head: &[Token], k: usize) -> bool {
    let mut words = (0..k).rev().filter(|&j| !head[j].is_comment());
    let (Some(alias), Some(before)) = (words.next(), words.next()) else { return false };
    let named = |t: &Token| matches!(t.kind, Kind::Bracket | Kind::Quoted) || (t.kind == Kind::Word && !is_reserved(&t.text.to_ascii_uppercase()));
    named(&head[alias]) && (named(&head[before]) || head[before].kind == Kind::RParen || head[before].text.eq_ignore_ascii_case("AS"))
}

const GLOBAL_VARIABLES: &[&str] = &[
    "@@ERROR", "@@FETCH_STATUS", "@@IDENTITY", "@@ROWCOUNT", "@@SERVERNAME", "@@SPID",
    "@@TRANCOUNT", "@@VERSION",
];

pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

pub(crate) fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}

/// Identifier text without brackets or quotes.
pub(crate) fn bare(t: &Token) -> String {
    match t.kind {
        Kind::Bracket => t.text[1..t.text.len() - 1].replace("]]", "]"),
        Kind::Quoted => t.text[1..t.text.len() - 1].replace("\"\"", "\""),
        _ => t.text.clone(),
    }
}

pub(crate) fn quote(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '#' || c == '@')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '#' | '@' | '$'));
    if plain && !is_reserved(&name.to_ascii_uppercase()) {
        name.to_string()
    } else {
        format!("[{}]", name.replace(']', "]]"))
    }
}

pub(crate) fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub(crate) fn is_name(t: &Token) -> bool {
    matches!(t.kind, Kind::Word | Kind::Bracket | Kind::Quoted)
}

/// A table-like source visible in the statement around the caret.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    /// The name the query can refer to it by: its alias, or else its own name.
    pub reference: String,
    /// Multipart object name as written, or empty for a derived table.
    pub parts: Vec<String>,
    /// Columns inferred from a derived table or CTE.
    pub columns: Vec<String>,
    /// Token index of the source's first token.
    pub at: usize,
    /// Token index of the alias, if one is written.
    pub alias_at: Option<usize>,
}

pub(crate) struct Analysis {
    pub tokens: Vec<Token>,
    pub up: Vec<String>,
    /// Byte offset of each token's text.
    pub starts: Vec<usize>,
}

impl Analysis {
    pub fn new(text: &str) -> Self {
        let (tokens, _) = lex_partial(text);
        let up = upper_words(&tokens);
        let mut starts = Vec::with_capacity(tokens.len());
        let mut at = 0;
        for t in &tokens {
            at += t.ws.len();
            starts.push(at);
            at += t.text.len();
        }
        Analysis { tokens, up, starts }
    }

    pub fn matching(&self, open: usize, end: usize) -> usize {
        let mut depth = 0usize;
        for j in open..end {
            match self.tokens[j].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        end
    }

    /// Token range `[start, end)` of the statement containing byte offset `at`.
    pub fn statement_at(&self, at: usize) -> (usize, usize) {
        let explicit = |i: usize| matches!(self.tokens[i].kind, Kind::Semi | Kind::Go | Kind::Raw);
        let mut bounds: Vec<usize> = missing_terminators(&self.tokens);
        bounds.extend((0..self.tokens.len()).filter(|&i| explicit(i)));
        bounds.sort_unstable();
        bounds.dedup();
        let mut start = 0;
        for b in bounds {
            // An inferred end only lies before the caret once the next statement has begun;
            // until then the caret is still extending this one.
            let before = if explicit(b) {
                self.starts[b] + self.tokens[b].text.len() <= at
            } else {
                self.starts.get(b + 1).is_some_and(|&next| next < at)
            };
            if before {
                start = b + 1;
            } else {
                return (start, b + 1);
            }
        }
        (start, self.tokens.len())
    }

    /// The clause keyword governing token `k`, within its own parentheses.
    pub fn clause_before(&self, stmt_start: usize, k: usize) -> String {
        let mut depth = 0i32;
        for j in (stmt_start..k.min(self.tokens.len())).rev() {
            match self.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen => depth -= 1,
                Kind::Word if depth <= 0 && matches!(self.up[j].as_str(), "SELECT" | "FROM" | "WHERE" | "GROUP" | "ORDER" | "HAVING" | "SET" | "ON" | "VALUES" | "JOIN" | "INTO" | "INSERT") => {
                    return self.up[j].clone();
                }
                _ => {}
            }
            if depth < 0 {
                break;
            }
        }
        String::new()
    }

    /// Token ranges `[start, end)` of every statement, including their terminators.
    pub fn statements(&self) -> Vec<(usize, usize)> {
        let mut bounds: Vec<usize> = missing_terminators(&self.tokens);
        bounds.extend((0..self.tokens.len()).filter(|&i| matches!(self.tokens[i].kind, Kind::Semi | Kind::Go | Kind::Raw)));
        bounds.sort_unstable();
        bounds.dedup();
        let mut out = Vec::new();
        let mut start = 0;
        for b in bounds {
            if b >= start {
                out.push((start, b + 1));
                start = b + 1;
            }
        }
        if start < self.tokens.len() {
            out.push((start, self.tokens.len()));
        }
        out
    }

    /// Output column names of a SELECT list beginning at token `select`.
    pub fn select_columns(&self, select: usize, end: usize) -> Vec<String> {
        let mut columns = Vec::new();
        let mut depth = 0usize;
        let mut last_name: Option<String> = None;
        let mut j = select + 1;
        while j < end {
            let t = &self.tokens[j];
            match t.kind {
                Kind::LParen => depth += 1,
                Kind::RParen if depth == 0 => break,
                Kind::RParen => depth -= 1,
                Kind::Comma if depth == 0 => {
                    columns.extend(last_name.take());
                }
                Kind::Word if depth == 0 && matches!(self.up[j].as_str(), "FROM" | "INTO" | "WHERE" | "UNION" | "GROUP" | "ORDER") => break,
                _ if depth == 0 && is_name(t) && !(t.kind == Kind::Word && is_reserved(&self.up[j])) => {
                    last_name = Some(bare(t));
                }
                _ if depth == 0 && t.kind == Kind::Op && t.text == "=" => {
                    // `alias = expression` names the column before the `=`.
                    if let Some(name) = last_name.clone() {
                        columns.push(name);
                        last_name = None;
                        while j + 1 < end && !matches!(self.tokens[j + 1].kind, Kind::Comma) && !(self.tokens[j + 1].kind == Kind::Word && self.up[j + 1] == "FROM") {
                            j += 1;
                        }
                    }
                }
                _ => {}
            }
            j += 1;
        }
        columns.extend(last_name);
        columns.retain(|c| c != "*");
        columns
    }

    pub fn ctes(&self, start: usize, end: usize) -> Vec<Source> {
        let mut out = Vec::new();
        if start >= end || self.up[start] != "WITH" {
            return out;
        }
        let mut j = start + 1;
        while j < end {
            if !is_name(&self.tokens[j]) {
                break;
            }
            let name_at = j;
            let name = bare(&self.tokens[j]);
            j += 1;
            let mut columns = Vec::new();
            if j < end && self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                columns = (j + 1..close).filter(|&k| is_name(&self.tokens[k])).map(|k| bare(&self.tokens[k])).collect();
                j = close + 1;
            }
            if j < end && self.up[j] == "AS" {
                j += 1;
            }
            if j < end && self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                if columns.is_empty() && j + 1 < end && self.up[j + 1] == "SELECT" {
                    columns = self.select_columns(j + 1, close);
                }
                j = close + 1;
            }
            out.push(Source { reference: name.clone(), parts: vec![name], columns, at: name_at, alias_at: None });
            if j < end && self.tokens[j].kind == Kind::Comma {
                j += 1;
            } else {
                break;
            }
        }
        out
    }

    /// Tables, views, functions, CTEs and derived tables referenced in `[start, end)`.
    pub fn sources(&self, start: usize, end: usize) -> Vec<Source> {
        let mut out = self.ctes(start, end);
        let mut in_from = false;
        let mut j = start;
        while j < end {
            let w = self.up[j].as_str();
            let introduces = self.tokens[j].kind == Kind::Word
                && (matches!(w, "FROM" | "JOIN" | "APPLY" | "USING" | "INTO" | "UPDATE")
                    || (w == "MERGE" && self.up.get(j + 1).is_some_and(|n| n != "JOIN")));
            let comma_in_from = in_from && self.tokens[j].kind == Kind::Comma;
            if self.tokens[j].kind == Kind::Word && matches!(w, "WHERE" | "GROUP" | "ORDER" | "HAVING" | "ON" | "SET" | "SELECT" | "UNION") {
                in_from = false;
            }
            if !(introduces || comma_in_from) {
                j += 1;
                continue;
            }
            in_from = matches!(w, "FROM" | "JOIN" | "APPLY") || comma_in_from;
            j += 1;
            if j < end && self.up[j] == "INTO" {
                j += 1;
            }
            if j >= end {
                break;
            }
            let mut source = Source { reference: String::new(), parts: Vec::new(), columns: Vec::new(), at: j, alias_at: None };
            if self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                if j + 1 < end && self.up[j + 1] == "SELECT" {
                    source.columns = self.select_columns(j + 1, close);
                }
                j = close + 1;
            } else if is_name(&self.tokens[j]) && !(self.tokens[j].kind == Kind::Word && is_reserved(&self.up[j])) {
                source.parts.push(bare(&self.tokens[j]));
                j += 1;
                while j + 1 < end && self.tokens[j].kind == Kind::Dot {
                    // `db..table` leaves an empty schema part.
                    if self.tokens[j + 1].kind == Kind::Dot {
                        source.parts.push(String::new());
                        j += 1;
                        continue;
                    }
                    if !is_name(&self.tokens[j + 1]) {
                        break;
                    }
                    source.parts.push(bare(&self.tokens[j + 1]));
                    j += 2;
                }
                if j < end && self.tokens[j].kind == Kind::LParen {
                    j = self.matching(j, end) + 1;
                }
            } else {
                continue;
            }
            if j < end && self.up[j] == "AS" {
                j += 1;
            }
            if j < end && is_name(&self.tokens[j]) && !(self.tokens[j].kind == Kind::Word && (is_reserved(&self.up[j]) || matches!(self.up[j].as_str(), "WINDOW" | "APPLY"))) {
                source.reference = bare(&self.tokens[j]);
                source.alias_at = Some(j);
                j += 1;
            } else {
                source.reference = source.parts.last().cloned().unwrap_or_default();
            }
            if !source.reference.is_empty() {
                out.push(source);
            }
        }
        let ctes: Vec<Source> = out.iter().filter(|s| s.parts.len() == 1 && !s.columns.is_empty()).cloned().collect();
        for source in out.iter_mut().filter(|s| s.columns.is_empty() && s.parts.len() == 1) {
            if let Some(cte) = ctes.iter().find(|c| eq(&c.reference, &source.parts[0])) {
                source.columns = cte.columns.clone();
            }
        }
        out
    }

    /// True when token `k` sits directly inside the column list of `INSERT [INTO] name (`.
    fn in_insert_column_list(&self, k: usize) -> bool {
        let mut depth = 0usize;
        let mut open = None;
        for j in (0..k).rev() {
            match self.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen if depth == 0 => {
                    open = Some(j);
                    break;
                }
                Kind::LParen => depth -= 1,
                _ => {}
            }
        }
        let Some(mut j) = open else { return false };
        // Walk back over the multipart table name.
        let mut saw_name = false;
        while j > 0 {
            j -= 1;
            match self.tokens[j].kind {
                Kind::Word | Kind::Bracket | Kind::Quoted if !is_reserved(&self.up[j]) => saw_name = true,
                Kind::Dot => {}
                Kind::Word => return saw_name && matches!(self.up[j].as_str(), "INTO" | "INSERT"),
                _ => return false,
            }
        }
        false
    }

    pub fn declared_variables(&self, before: usize) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for j in 0..self.tokens.len() {
            if self.starts[j] >= before {
                break;
            }
            if self.tokens[j].kind == Kind::Variable && !self.tokens[j].text.starts_with("@@") {
                let ty = self.up.get(j + 1).filter(|_| self.tokens[j + 1].kind == Kind::Word).cloned().unwrap_or_default();
                let name = self.tokens[j].text.clone();
                if !out.iter().any(|(n, _)| eq(n, &name)) {
                    out.push((name, ty.to_lowercase()));
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Context {
    TableSource,
    Procedure,
    Expression,
    InsertColumns,
    /// After `INSERT INTO name `: offer the column list and VALUES.
    InsertTarget,
    /// After `EXEC name `: offer the parameters.
    ExecArgs,
    /// After `ALTER VIEW` or `DROP VIEW`.
    Views,
    /// After `ALTER FUNCTION` or `DROP FUNCTION`.
    Functions,
    /// After a table and its alias: the next clause, or a new statement.
    AfterSource,
    Nothing,
}

/// A literal of the right type to fill in a generated parameter or value list.
pub(crate) fn sample_value(data_type: &str) -> &'static str {
    let base = data_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "bigint" | "int" | "smallint" | "tinyint" | "bit" | "money" | "smallmoney" => "0",
        "decimal" | "numeric" | "float" | "real" => "0.0",
        "char" | "varchar" | "text" => "''",
        "nchar" | "nvarchar" | "ntext" | "sysname" => "N''",
        "date" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" | "time" => "'1900-01-01'",
        "uniqueidentifier" => "'00000000-0000-0000-0000-000000000000'",
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => "0x",
        _ => "NULL",
    }
}

/// A short alias from a name's initials (`OrderLines` -> `ol`), unique among `taken`.
pub(crate) fn alias_for(name: &str, taken: &[String]) -> String {
    let mut base = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let starts_word = match prev {
            None => true,
            Some(p) => (c.is_uppercase() && p.is_lowercase()) || (!p.is_alphanumeric() && c.is_alphanumeric()),
        };
        if starts_word && c.is_alphanumeric() {
            base.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    if base.is_empty() || !base.chars().next().is_some_and(char::is_alphabetic) {
        base.insert(0, 't');
    }
    let free = |a: &str| !is_reserved(&a.to_ascii_uppercase()) && !taken.iter().any(|t| eq(t, a));
    if free(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}{n}")).find(|a| free(a)).expect("unbounded")
}

/// Join conditions between a newly joined source and an earlier one, from foreign keys,
/// or else from identically named `...Id` columns. The flag is true for foreign keys.
fn join_conditions(new: (&str, &DbObject), old: (&str, &DbObject), schema: &Schema) -> Vec<(String, bool)> {
    let (nr, no) = new;
    let (or, oo) = old;
    let same = |s: &str, t: &str, o: &DbObject| eq(s, &o.schema) && eq(t, &o.name);
    let mut out = Vec::new();
    for fk in &schema.foreign_keys {
        let pairs: Vec<(String, String)> = if same(&fk.schema, &fk.table, no) && same(&fk.ref_schema, &fk.ref_table, oo) {
            fk.columns.iter().zip(&fk.ref_columns).map(|(a, b)| (a.clone(), b.clone())).collect()
        } else if same(&fk.schema, &fk.table, oo) && same(&fk.ref_schema, &fk.ref_table, no) {
            fk.ref_columns.iter().zip(&fk.columns).map(|(a, b)| (a.clone(), b.clone())).collect()
        } else {
            continue;
        };
        let cond = pairs.iter().map(|(a, b)| format!("{}.{} = {}.{}", quote(nr), quote(a), quote(or), quote(b))).collect::<Vec<_>>().join(" AND ");
        if !cond.is_empty() && !out.iter().any(|(c, _)| *c == cond) {
            out.push((cond, true));
        }
    }
    if out.is_empty() {
        for c in no.columns.iter().filter(|c| c.name.to_ascii_lowercase().ends_with("id")) {
            if oo.columns.iter().any(|o| eq(&o.name, &c.name)) {
                out.push((format!("{}.{} = {}.{}", quote(nr), quote(&c.name), quote(or), quote(&c.name)), false));
            }
        }
    }
    out
}

/// `@a = 0, -- int` lines for a procedure call, aligned under the first parameter.
fn parameter_list(params: &[Param], align: &str, nl: &str) -> String {
    let mut out = String::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(nl);
            out.push_str(align);
        }
        let value = if p.output { p.name.clone() } else { sample_value(&p.data_type).to_string() };
        out.push_str(&format!("{} = {}{}", p.name, value, if p.output { " OUTPUT" } else { "" }));
        out.push_str(if i + 1 < params.len() { ", " } else { " " });
        out.push_str(&format!("-- {}", p.data_type));
    }
    out
}

fn writable(o: &DbObject) -> Vec<&Column> {
    o.columns
        .iter()
        .filter(|c| !c.identity && !c.computed && !matches!(c.data_type.as_str(), "timestamp" | "rowversion"))
        .collect()
}

pub(crate) fn object_matches(o: &DbObject, parts: &[String], schema: &Schema) -> bool {
    match parts {
        [name] => eq(&o.name, name) && (eq(&o.schema, &schema.default_schema) || schema.default_schema.is_empty()),
        [.., s, name] if s.is_empty() => eq(&o.name, name),
        [.., s, name] => eq(&o.name, name) && eq(&o.schema, s),
        [] => false,
    }
}

pub(crate) fn resolve<'s>(source: &Source, schema: &'s Schema) -> Option<&'s DbObject> {
    let exact = schema.objects.iter().find(|o| object_matches(o, &source.parts, schema));
    // An unqualified name falls back to any schema, as SQL Server resolves it via the default schema first.
    exact.or_else(|| match source.parts.as_slice() {
        [name] => schema.objects.iter().find(|o| eq(&o.name, name)),
        _ => None,
    })
}

pub(crate) fn source_columns(source: &Source, schema: Option<&Schema>) -> Vec<(String, String)> {
    if !source.columns.is_empty() {
        return source.columns.iter().map(|c| (c.clone(), String::new())).collect();
    }
    schema
        .and_then(|s| resolve(source, s))
        .map(|o| o.columns.iter().map(|c| (c.name.clone(), c.data_type.clone())).collect())
        .unwrap_or_default()
}

fn object_kind(kind: &str) -> &'static str {
    match kind {
        "table" => "table",
        "view" => "view",
        "procedure" => "procedure",
        "scalar_function" => "function",
        "table_function" => "table_function",
        "synonym" => "synonym",
        _ => "object",
    }
}

fn object_item(o: &DbObject, schema: &Schema, qualified: bool) -> Item {
    let name = if qualified || eq(&o.schema, &schema.default_schema) || o.schema.is_empty() {
        quote(&o.name)
    } else {
        format!("{}.{}", quote(&o.schema), quote(&o.name))
    };
    let detail = match o.kind.as_str() {
        "procedure" | "scalar_function" | "table_function" if !o.params.is_empty() => format!(
            "{}.{} ({})",
            o.schema,
            o.name,
            o.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect::<Vec<_>>().join(", ")
        ),
        _ => format!("{}.{}", o.schema, o.name),
    };
    Item { label: o.name.clone(), insert: name, kind: object_kind(&o.kind), detail }
}

fn wants(context: Context, kind: &str) -> bool {
    match context {
        Context::TableSource => matches!(kind, "table" | "view" | "table_function" | "synonym"),
        Context::Procedure => matches!(kind, "procedure" | "synonym"),
        Context::Expression => matches!(kind, "scalar_function" | "table" | "view"),
        Context::Views => kind == "view",
        Context::Functions => matches!(kind, "scalar_function" | "table_function"),
        Context::InsertColumns | Context::InsertTarget | Context::ExecArgs | Context::AfterSource | Context::Nothing => false,
    }
}

/// Lower ranks sort first.
fn rank(kind: &str, context: Context) -> u8 {
    match (context, kind) {
        (_, "join" | "body") => 0,
        (Context::Expression, "column") => 1,
        (Context::Expression, "alias") => 2,
        (Context::Expression, "variable") => 3,
        (Context::Expression, "function") => 4,
        (_, "parameter") => 1,
        (_, "join_guess") => 3,
        (_, "snippet") => 5,
        (_, "global_variable") => 8,
        (_, "keyword") => 9,
        (_, "schema" | "database") => 6,
        _ => 4,
    }
}

fn object_at(tokens: &[Token], from: usize, to: usize, schema: &Schema) -> Option<DbObject> {
    let parts: Vec<String> = tokens[from..to].iter().filter(|t| is_name(t)).map(bare).collect();
    let source = Source { reference: String::new(), parts, columns: Vec::new(), at: from, alias_at: None };
    resolve(&source, schema).cloned()
}

/// Choices the user can change in the options.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompleteOptions {
    /// Give a table picked after FROM or JOIN an alias: `Customers` becomes `Customers AS c`.
    pub assign_aliases: bool,
    /// Write `AS` before generated aliases.
    pub alias_with_as: bool,
    /// Insert objects with their schema (`dbo.Customers`) even in the default schema.
    pub qualify_object_names: bool,
    /// List sys and INFORMATION_SCHEMA views without typing their schema first.
    pub show_system_objects: bool,
}

impl Default for CompleteOptions {
    fn default() -> Self {
        CompleteOptions { assign_aliases: true, alias_with_as: true, qualify_object_names: true, show_system_objects: false }
    }
}

fn is_system(o: &DbObject) -> bool {
    o.schema.eq_ignore_ascii_case("sys") || o.schema.eq_ignore_ascii_case("INFORMATION_SCHEMA")
}

/// Built-in functions called without parentheses.
const NILADIC: &[&str] = &["CURRENT_TIMESTAMP", "CURRENT_USER", "SESSION_USER", "SYSTEM_USER", "USER", "CURRENT_DATE"];

pub fn complete(text: &str, caret_utf16: usize, schema: Option<&Schema>) -> Completion {
    complete_with(text, caret_utf16, schema, &[])
}

pub fn complete_with(text: &str, caret_utf16: usize, schema: Option<&Schema>, snippets: &[Snippet]) -> Completion {
    complete_full(text, caret_utf16, schema, snippets, CompleteOptions::default())
}

pub fn complete_full(text: &str, caret_utf16: usize, schema: Option<&Schema>, snippets: &[Snippet], options: CompleteOptions) -> Completion {
    let caret = utf16_to_byte(text, caret_utf16);
    let none = |at: usize| Completion { start: byte_to_utf16(text, at), items: Vec::new(), selected: None };
    let before = &text[..caret];

    // Never suggest inside a string or comment.
    let (head, error) = lex_partial(before);
    let mut bracket_prefix: Option<(usize, String)> = None;
    if let Some((e, offset)) = &error {
        if e.what == "bracketed identifier" {
            bracket_prefix = Some((*offset, before[*offset + 1..].to_string()));
        } else {
            return none(caret);
        }
    }
    let trailing = before.len() - head.iter().map(|t| t.ws.len() + t.text.len()).sum::<usize>();
    let touching = trailing == 0 && bracket_prefix.is_none();
    if touching && head.last().is_some_and(|t| t.kind == Kind::LineComment) {
        return none(caret);
    }

    let a = Analysis::new(text);
    // Index of the last token before the caret in the full-text analysis.
    let mut k = head.len();
    let (prefix, prefix_start, variable) = match (&bracket_prefix, head.last()) {
        (Some((offset, p)), _) => (p.clone(), *offset, false),
        (None, Some(t)) if touching && matches!(t.kind, Kind::Word | Kind::Variable | Kind::Bracket) => {
            k -= 1;
            (bare(t), caret - t.text.len(), t.kind == Kind::Variable)
        }
        (None, Some(t)) if touching && t.kind == Kind::Number => return none(caret),
        _ => (String::new(), caret, false),
    };
    let variable = variable || (prefix.is_empty() && before.ends_with('@'));

    // `GO` alone on its line ends a batch (the lexer reads it as one): offering GOTO or GROUPING
    // right after it would let Enter replace it.
    if touching && head.last().is_some_and(|t| t.kind == Kind::Go) {
        return none(caret);
    }

    // A qualifier such as `t.` or `dbo.` right before the word.
    let mut qualifier: Vec<String> = Vec::new();
    while k >= 2 && head[k - 1].kind == Kind::Dot && is_name(&head[k - 2]) {
        qualifier.insert(0, bare(&head[k - 2]));
        k -= 2;
    }
    let prev_word = (0..k).rev().find(|&j| !head[j].is_comment()).map(|j| (head[j].kind, head[j].text.to_ascii_uppercase()));

    let (stmt_start, stmt_end) = a.statement_at(prefix_start);
    let sources = a.sources(stmt_start, stmt_end);

    let clause = a.clause_before(stmt_start, k);

    // `INSERT [INTO] name ` or `EXEC name ` with the caret after a space.
    let after_target = |intro: &[&str]| -> Option<usize> {
        if touching || k == 0 || !is_name(&head[k - 1]) {
            return None;
        }
        let mut j = k - 1;
        while j >= 2 && head[j - 1].kind == Kind::Dot && is_name(&head[j - 2]) {
            j -= 2;
        }
        let w = head.get(j.checked_sub(1)?)?.text.to_ascii_uppercase();
        intro.contains(&w.as_str()).then_some(j)
    };
    let insert_target = after_target(&["INTO", "INSERT"]).filter(|_| prefix.is_empty() && qualifier.is_empty());
    let exec_target = after_target(&["EXEC", "EXECUTE"]).filter(|_| prefix.is_empty() && qualifier.is_empty());

    let context = match &prev_word {
        _ if variable => Context::Expression,
        _ if insert_target.is_some() => Context::InsertTarget,
        _ if exec_target.is_some() => Context::ExecArgs,
        Some((Kind::Word, w)) if matches!(w.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO" | "TABLE" | "MERGE" | "USING" | "APPLY") => Context::TableSource,
        Some((Kind::Word, w)) if matches!(w.as_str(), "EXEC" | "EXECUTE" | "PROC" | "PROCEDURE") => Context::Procedure,
        Some((Kind::Word, w)) if w == "VIEW" => Context::Views,
        Some((Kind::Word, w)) if w == "FUNCTION" => Context::Functions,
        Some((Kind::Comma, _)) if clause == "FROM" || clause == "JOIN" => Context::TableSource,
        Some((Kind::LParen | Kind::Comma, _)) if a.in_insert_column_list(k) => Context::InsertColumns,
        Some((Kind::Word, w)) if matches!(w.as_str(), "AS") && clause == "FROM" => Context::Nothing,
        Some((Kind::Word | Kind::Bracket | Kind::Quoted, w)) if !qualifier.is_empty() || is_reserved(w) => Context::Expression,
        // After a table and its alias comes the next clause or statement.
        Some((Kind::Word | Kind::Bracket | Kind::Quoted, _)) if (clause == "FROM" || clause == "JOIN") && alias_written(&head, k) => Context::AfterSource,
        // A name directly after a table name is an alias being typed.
        Some((Kind::Word | Kind::Bracket | Kind::Quoted | Kind::RParen, _)) if clause == "FROM" || clause == "JOIN" => Context::Nothing,
        _ => Context::Expression,
    };

    let mut items: Vec<Item> = Vec::new();
    let default = Schema::default();
    let meta = schema.unwrap_or(&default);
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = before[..prefix_start].rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..prefix_start].chars().count();
    let indent: String = before[line_start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let prev_upper = prev_word.as_ref().filter(|(kind, _)| *kind == Kind::Word).map(|(_, w)| w.as_str()).unwrap_or("");
    let earlier: Vec<&Source> = sources.iter().filter(|s| s.at < k).collect();
    // An alias is added after FROM, JOIN or a comma in FROM, unless one is already written.
    let after = (0..a.tokens.len()).find(|&j| !a.tokens[j].is_comment() && a.starts[j] >= caret);
    let has_alias = after.is_some_and(|j| {
        let t = &a.tokens[j];
        matches!(t.kind, Kind::Bracket | Kind::Quoted | Kind::Dot | Kind::LParen) || (t.kind == Kind::Word && (!is_reserved(&a.up[j]) || a.up[j] == "AS"))
    });
    let aliasing = options.assign_aliases
        && context == Context::TableSource
        && !has_alias
        && (matches!(prev_upper, "FROM" | "JOIN") || (matches!(prev_word, Some((Kind::Comma, _))) && clause == "FROM"));
    let taken: Vec<String> = sources.iter().filter(|s| s.at < k || a.starts[s.at] > caret).map(|s| s.reference.clone()).collect();
    let with_alias = |item: &mut Item, name: &str| {
        let alias = alias_for(name, &taken);
        if !eq(&alias, name) {
            item.insert = format!("{}{}{alias}", item.insert, if options.alias_with_as { " AS " } else { " " });
        }
    };
    let qualify = options.qualify_object_names;

    if variable {
        for (name, ty) in a.declared_variables(prefix_start) {
            items.push(Item { label: name.clone(), insert: name, kind: "variable", detail: ty });
        }
        if stmt_start < a.tokens.len() && matches!(a.up[stmt_start].as_str(), "EXEC" | "EXECUTE") {
            let mut to = stmt_start + 1;
            while to < a.tokens.len() && (is_name(&a.tokens[to]) || a.tokens[to].kind == Kind::Dot) && a.starts[to] < prefix_start {
                to += 1;
            }
            if let Some(proc) = object_at(&a.tokens, stmt_start + 1, to, meta) {
                for p in &proc.params {
                    items.push(Item { label: p.name.clone(), insert: format!("{} = ", p.name), kind: "parameter", detail: p.data_type.clone() });
                }
            }
        }
        for g in GLOBAL_VARIABLES {
            items.push(Item { label: g.to_string(), insert: g.to_string(), kind: "global_variable", detail: "global".into() });
        }
    } else if !qualifier.is_empty() {
        let q = qualifier.last().expect("non-empty");
        // The qualifier being typed is itself parsed as a source, so only a source with
        // known columns counts as a match.
        let aliased = sources
            .iter()
            .filter(|s| qualifier.len() == 1 && eq(&s.reference, q))
            .map(|s| source_columns(s, schema))
            .find(|c| !c.is_empty());
        if let Some(columns) = aliased {
            for (name, ty) in columns {
                items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
            }
        } else if meta.schemas.iter().any(|s| eq(s, q)) || meta.objects.iter().any(|o| eq(&o.schema, q)) {
            let wanted = |kind: &str| match context {
                Context::Expression => true,
                c => wants(c, kind),
            };
            for o in meta.objects.iter().filter(|o| eq(&o.schema, q) && wanted(&o.kind)) {
                let mut item = object_item(o, meta, true);
                if aliasing && matches!(o.kind.as_str(), "table" | "view" | "synonym") {
                    with_alias(&mut item, &o.name);
                }
                items.push(item);
            }
        } else if let Some(src) = sources.iter().find(|s| s.parts.last().is_some_and(|p| eq(p, q))) {
            for (name, ty) in source_columns(src, schema) {
                items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
            }
        }
    } else {
        match context {
            Context::TableSource | Context::Procedure | Context::Views | Context::Functions => {
                for o in meta.objects.iter().filter(|o| wants(context, &o.kind) && (options.show_system_objects || !is_system(o))) {
                    let mut item = object_item(o, meta, false);
                    if qualify && !o.schema.is_empty() {
                        item.insert = format!("{}.{}", quote(&o.schema), quote(&o.name));
                    }
                    if aliasing && matches!(o.kind.as_str(), "table" | "view" | "synonym") {
                        with_alias(&mut item, &o.name);
                    }
                    if matches!(prev_upper, "EXEC" | "EXECUTE") && o.kind == "procedure" && !o.params.is_empty() {
                        let align = " ".repeat(column + item.insert.chars().count() + 1);
                        item.insert = format!("{} {}", item.insert, parameter_list(&o.params, &align, nl));
                    }
                    items.push(item);
                }
                if prev_upper == "JOIN" {
                    let taken: Vec<String> = sources.iter().map(|s| s.reference.clone()).collect();
                    for o in meta.objects.iter().filter(|o| matches!(o.kind.as_str(), "table" | "view")) {
                        let alias = alias_for(&o.name, &taken);
                        for e in &earlier {
                            let Some(eo) = resolve(e, meta) else { continue };
                            if let Some((cond, fk)) = join_conditions((&alias, o), (&e.reference, eo), meta).into_iter().next() {
                                let name = if qualify && !o.schema.is_empty() { format!("{}.{}", quote(&o.schema), quote(&o.name)) } else { object_item(o, meta, false).insert };
                                let kind = if fk { "join" } else { "join_guess" };
                                let as_ = if options.alias_with_as { " AS " } else { " " };
                                items.push(Item { label: o.name.clone(), insert: format!("{name}{as_}{alias} ON {cond}"), kind, detail: format!("ON {cond}") });
                            }
                        }
                    }
                }
                if context == Context::TableSource {
                    for cte in a.ctes(stmt_start, stmt_end) {
                        let mut item = Item { label: cte.reference.clone(), insert: quote(&cte.reference), kind: "cte", detail: "common table expression".into() };
                        if aliasing {
                            with_alias(&mut item, &cte.reference);
                        }
                        items.push(item);
                    }
                }
                for s in &meta.schemas {
                    items.push(Item { label: s.clone(), insert: quote(s), kind: "schema", detail: "schema".into() });
                }
            }
            Context::InsertTarget => {
                let from = insert_target.expect("context");
                if let Some(o) = object_at(&head, from, k, meta) {
                    let cols = writable(&o);
                    if !cols.is_empty() {
                        let names = cols.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ");
                        let mut body = format!("({names}){nl}{indent}VALUES{nl}{indent}({nl}");
                        for (i, c) in cols.iter().enumerate() {
                            let comma = if i + 1 < cols.len() { "," } else { "" };
                            body.push_str(&format!("{indent}    {}{comma} -- {} {}{nl}", sample_value(&c.data_type), c.name, c.data_type));
                        }
                        body.push_str(&format!("{indent})"));
                        items.push(Item { label: "(columns) VALUES (...)".into(), insert: body, kind: "body", detail: format!("{} columns of {}.{}", cols.len(), o.schema, o.name) });
                    }
                }
            }
            Context::ExecArgs => {
                let from = exec_target.expect("context");
                if let Some(o) = object_at(&head, from, k, meta) {
                    if !o.params.is_empty() {
                        let align = " ".repeat(column);
                        items.push(Item { label: "(all parameters)".into(), insert: parameter_list(&o.params, &align, nl), kind: "body", detail: format!("{}.{}", o.schema, o.name) });
                    }
                    for p in &o.params {
                        items.push(Item { label: p.name.clone(), insert: format!("{} = ", p.name), kind: "parameter", detail: p.data_type.clone() });
                    }
                }
            }
            Context::InsertColumns => {
                if let Some(target) = sources.iter().find(|s| !s.parts.is_empty()) {
                    for (name, ty) in source_columns(target, schema) {
                        items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
                    }
                }
            }
            Context::Expression => {
                if prev_upper == "ON" && clause == "ON"
                    && let Some((new, older)) = earlier.split_last()
                        && let Some(no) = resolve(new, meta) {
                            for e in older {
                                let Some(eo) = resolve(e, meta) else { continue };
                                for (cond, fk) in join_conditions((&new.reference, no), (&e.reference, eo), meta) {
                                    let kind = if fk { "join" } else { "join_guess" };
                                    items.push(Item { label: cond.clone(), insert: cond, kind, detail: "join condition".into() });
                                }
                            }
                        }
                if prev_upper == "SET" && a.up.get(stmt_start).is_some_and(|w| w == "UPDATE")
                    && let Some(o) = sources.iter().find(|s| !s.parts.is_empty()).and_then(|s| resolve(s, meta)) {
                        let cols = writable(o);
                        let align = " ".repeat(column);
                        let body = cols
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{} = {}{} -- {}", quote(&c.name), sample_value(&c.data_type), if i + 1 < cols.len() { "," } else { "" }, c.data_type))
                            .collect::<Vec<_>>()
                            .join(&format!("{nl}{align}"));
                        if !body.is_empty() {
                            items.push(Item { label: "(all columns)".into(), insert: body, kind: "body", detail: format!("{}.{}", o.schema, o.name) });
                        }
                    }
                // Columns of every source; a name in more than one source is qualified.
                let all: Vec<(String, String, String)> = sources
                    .iter()
                    .flat_map(|s| source_columns(s, schema).into_iter().map(move |(c, t)| (s.reference.clone(), c, t)))
                    .collect();
                for (reference, name, ty) in &all {
                    let ambiguous = all.iter().filter(|(_, n, _)| eq(n, name)).count() > 1;
                    let insert = if ambiguous { format!("{}.{}", quote(reference), quote(name)) } else { quote(name) };
                    let detail = if ty.is_empty() { reference.clone() } else { format!("{reference} · {ty}") };
                    items.push(Item { label: name.clone(), insert, kind: "column", detail });
                }
                for s in &sources {
                    items.push(Item { label: s.reference.clone(), insert: quote(&s.reference), kind: "alias", detail: s.parts.join(".") });
                }
                for o in meta.objects.iter().filter(|o| o.kind == "scalar_function") {
                    let mut item = object_item(o, meta, false);
                    // Scalar functions must be called with their schema.
                    item.insert = format!("{}.{}()", quote(&o.schema), quote(&o.name));
                    items.push(item);
                }
                for f in FUNCTIONS {
                    // The editor puts the caret between the parentheses.
                    let insert = if NILADIC.contains(f) { f.to_string() } else { format!("{f}()") };
                    items.push(Item { label: f.to_string(), insert, kind: "function", detail: "built-in function".into() });
                }
                for kw in STATEMENT_KEYWORDS.iter().chain(EXPRESSION_KEYWORDS) {
                    items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                }
            }
            Context::AfterSource => {
                for kw in CLAUSE_KEYWORDS.iter().chain(STATEMENT_KEYWORDS) {
                    items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                }
            }
            Context::Nothing => {}
        }
    }

    if prefix.chars().count() >= 2 && qualifier.is_empty() && !variable && !matches!(context, Context::InsertColumns | Context::InsertTarget | Context::ExecArgs) {
        for sn in snippets {
            items.push(Item { label: sn.prefix.clone(), insert: sn.prefix.clone(), kind: "snippet", detail: sn.description.clone() });
        }
    }

    // Rank: prefix matches, then substring matches; then by kind and name.
    let mut scored: Vec<(u8, u8, bool, Item)> = items
        .into_iter()
        .filter_map(|it| {
            let exact_or_prefix = match_quality(&it.label, &prefix)?;
            // The whole word ranks with its start; both come before initials and inner matches.
            let quality = if exact_or_prefix == 0 { 1 } else { exact_or_prefix };
            let label = it.label.to_ascii_lowercase();
            let label = label.trim_start_matches('@');
            let needle = prefix.to_ascii_lowercase();
            let needle = needle.trim_start_matches('@');
            // System catalog views sort after the database's own objects.
            let system = it.detail.starts_with("sys.") || it.detail.starts_with("INFORMATION_SCHEMA.");
            // A whole word typed in another case (`join`, `customerid`) comes first, so Tab or Enter
            // gives it its proper case instead of a longer name that starts with it.
            let exact = !needle.is_empty() && label == needle;
            let rank = if exact { 0 } else { rank(it.kind, context) };
            Some((quality, rank, system, it))
        })
        .collect();
    scored.sort_by_key(|x| (x.0, x.1, x.2, x.3.label.to_ascii_lowercase()));
    scored.dedup_by(|x, y| x.3.insert == y.3.insert && x.3.kind == y.3.kind);
    let items: Vec<Item> = scored.into_iter().take(MAX_ITEMS).map(|(_, _, _, it)| it).collect();
    // Select the first item only when something was typed and it matches for sure; a list of guesses,
    // or one opened by a space before anything is typed, starts with nothing selected.
    let selected = items.first().filter(|it| !prefix.is_empty() && match_quality(&it.label, &prefix).is_some_and(|q| q <= 2)).map(|_| 0);
    Completion { start: byte_to_utf16(text, prefix_start), items, selected }
}
