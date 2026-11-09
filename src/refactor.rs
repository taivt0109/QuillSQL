//! Refactorings for the SSMS extension. Each one takes the script and returns the
//! rewritten script, or an error message that the editor shows instead of changing anything.

use crate::complete::{Analysis, Schema, Source, bare, eq, is_name, quote, resolve, source_columns, utf16_to_byte};
use crate::keywords::is_reserved;
use crate::lexer::{Kind, lex};
use crate::options::TableAlias;
use crate::transform::{next_sig, prev_sig, table_alias};

/// Replacements `(start, end, text)` in byte offsets; they must not overlap.
fn apply(text: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (start, end, replacement) in edits {
        out.replace_range(start..end, &replacement);
    }
    out
}

pub(crate) fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Leading whitespace of the line containing byte offset `at`.
fn line_indent(text: &str, at: usize) -> String {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

pub(crate) fn depths(a: &Analysis) -> Vec<usize> {
    let mut depth = 0usize;
    a.tokens
        .iter()
        .map(|t| match t.kind {
            Kind::LParen => {
                depth += 1;
                depth - 1
            }
            Kind::RParen => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        })
        .collect()
}

/// Token range of the query that token `i` belongs to: the innermost `(SELECT ...)`
/// around it, or the whole statement.
pub(crate) fn query_range(a: &Analysis, stmt: (usize, usize), i: usize) -> (usize, usize) {
    let mut depth = 0usize;
    for j in (stmt.0..i).rev() {
        match a.tokens[j].kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth > 0 => depth -= 1,
            Kind::LParen
                if a.up.get(j + 1).is_some_and(|w| w == "SELECT") => {
                    return (j + 1, a.matching(j, stmt.1));
                }
            _ => {}
        }
    }
    stmt
}

/// The sources a column at token `i` can refer to: those in its own query, not nested ones.
pub(crate) fn scope_sources(a: &Analysis, d: &[usize], stmt: (usize, usize), i: usize) -> Vec<Source> {
    let (q, q_end) = query_range(a, stmt, i);
    let base = d[q];
    let cte_names: Vec<usize> = a.ctes(stmt.0, stmt.1).iter().map(|c| c.at).collect();
    a.sources(stmt.0, stmt.1)
        .into_iter()
        .filter(|s| s.at >= q && s.at < q_end && d[s.at] == base && !cte_names.contains(&s.at))
        .collect()
}

/// The byte after token `i`; past the last token, the end of the last one (a statement still
/// being typed can point there).
pub(crate) fn end_of(a: &Analysis, i: usize) -> usize {
    let i = i.min(a.tokens.len().saturating_sub(1));
    a.tokens.get(i).map_or(0, |t| a.starts[i] + t.text.len())
}

/// The `SELECT` whose select list contains token `i`, if any.
pub(crate) fn owning_select(a: &Analysis, stmt_start: usize, i: usize) -> Option<usize> {
    let mut depth = 0usize;
    for j in (stmt_start..i).rev() {
        match a.tokens[j].kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => return None,
            Kind::LParen => depth -= 1,
            Kind::Word if depth == 0 => match a.up[j].as_str() {
                "SELECT" => return Some(j),
                "FROM" | "WHERE" | "GROUP" | "ORDER" | "HAVING" | "SET" | "VALUES" | "ON" | "INTO" | "UNION" | "EXCEPT" | "INTERSECT" => return None,
                _ => {}
            },
            _ => {}
        }
    }
    None
}

/// Replaces `*` and `alias.*` in select lists with the column names. With `caret`
/// (UTF-16), only the wildcard at the caret is expanded.
pub fn expand_wildcards(text: &str, schema: Option<&Schema>, caret: Option<usize>) -> Result<(String, usize), String> {
    let a = Analysis::new(text);
    let (edits, unknown) = wildcard_edits(&a, text, schema, caret.map(|c| utf16_to_byte(text, c)));
    if edits.is_empty() {
        return Err(if unknown.is_empty() {
            "no wildcard to expand".into()
        } else {
            format!("columns of {} are not known", unknown.join(", "))
        });
    }
    let n = edits.len();
    Ok((apply(text, edits), n))
}

/// The UTF-16 range and replacement that expand the wildcard around the caret (UTF-16).
pub fn wildcard_edit_at(text: &str, schema: Option<&Schema>, caret: usize) -> Option<(usize, usize, String)> {
    let a = Analysis::new(text);
    let (edits, _) = wildcard_edits(&a, text, schema, Some(utf16_to_byte(text, caret)));
    edits.into_iter().next().map(|(s, e, t)| (text[..s].encode_utf16().count(), text[..e].encode_utf16().count(), t))
}

/// Byte-offset edits that expand each select-list wildcard (only the one around byte
/// offset `caret`, if given), and the sources whose columns are unknown.
pub(crate) fn wildcard_edits(a: &Analysis, text: &str, schema: Option<&Schema>, caret: Option<usize>) -> (Vec<(usize, usize, String)>, Vec<String>) {
    let d = depths(a);
    let nl = newline(text);
    let mut edits = Vec::new();
    let mut unknown = Vec::new();
    for stmt in a.statements() {
        for i in stmt.0..stmt.1 {
            if !(a.tokens[i].kind == Kind::Op && a.tokens[i].text == "*") {
                continue;
            }
            let prev = prev_sig(&a.tokens, i);
            let qualifier = prev
                .filter(|&p| a.tokens[p].kind == Kind::Dot && p > 0 && is_name(&a.tokens[p - 1]))
                .map(|p| p - 1);
            let start_token = qualifier.unwrap_or(i);
            let in_list = match prev {
                _ if qualifier.is_some() => true,
                Some(p) => match a.tokens[p].kind {
                    Kind::Word => matches!(a.up[p].as_str(), "SELECT" | "DISTINCT" | "ALL"),
                    Kind::Comma => true,
                    Kind::Number | Kind::RParen => true,
                    _ => false,
                },
                None => false,
            };
            let Some(select) = owning_select(a, stmt.0, start_token).filter(|_| in_list) else { continue };
            // `TOP 5 *` and `TOP (5) *`, not `x * 5`.
            if let Some(p) = prev.filter(|&p| matches!(a.tokens[p].kind, Kind::Number | Kind::RParen)) {
                let before = if a.tokens[p].kind == Kind::RParen {
                    (stmt.0..p).rev().find(|&j| a.tokens[j].kind == Kind::LParen && a.matching(j, stmt.1) == p).and_then(|j| prev_sig(&a.tokens, j))
                } else {
                    prev_sig(&a.tokens, p)
                };
                if !before.is_some_and(|b| a.up[b] == "TOP") {
                    continue;
                }
            }
            let (from, to) = (a.starts[start_token], end_of(a, i));
            if caret.is_some_and(|c| c < from || c > to) {
                continue;
            }
            let sources = scope_sources(a, &d, stmt, select);
            let chosen: Vec<&Source> = match qualifier {
                Some(q) => sources.iter().filter(|s| eq(&s.reference, &bare(&a.tokens[q]))).collect(),
                None => sources.iter().collect(),
            };
            if chosen.is_empty() {
                continue;
            }
            let mut names = Vec::new();
            for s in &chosen {
                let columns = source_columns(s, schema);
                if columns.is_empty() {
                    unknown.push(s.reference.clone());
                    names.clear();
                    break;
                }
                let qualify = qualifier.is_some() || sources.len() > 1;
                for (c, _) in columns {
                    names.push(if qualify { format!("{}.{}", quote(&s.reference), quote(&c)) } else { quote(&c) });
                }
            }
            if names.is_empty() {
                continue;
            }
            let separator = if names.len() <= 4 { ", ".to_string() } else { format!(",{nl}{}    ", line_indent(text, from)) };
            edits.push((from, to, names.join(&separator)));
        }
    }
    (edits, unknown)
}

/// Adds the schema to unqualified tables, views and procedures, and the alias to column
/// names in queries that read from more than one source.
pub fn qualify_object_names(text: &str, schema: &Schema) -> Result<(String, usize), String> {
    let a = Analysis::new(text);
    let edits: Vec<(usize, usize, String)> = qualify_edits(&a, schema).into_iter().map(|(at, prefix, _)| (at, at, prefix)).collect();
    if edits.is_empty() {
        return Err("every name is already qualified".into());
    }
    let n = edits.len();
    Ok((apply(text, edits), n))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Qualified {
    Procedure,
    Object,
    Column,
}

/// Prefixes to insert at byte offsets to qualify names, sorted by offset.
pub(crate) fn qualify_edits(a: &Analysis, schema: &Schema) -> Vec<(usize, String, Qualified)> {
    let d = depths(a);
    let mut edits: Vec<(usize, String, Qualified)> = Vec::new();
    for stmt in a.statements() {
        let ctes: Vec<String> = a.ctes(stmt.0, stmt.1).into_iter().map(|c| c.reference).collect();
        let sources = a.sources(stmt.0, stmt.1);
        let mut source_tokens: Vec<usize> = Vec::new();
        for s in &sources {
            source_tokens.extend(s.alias_at);
            if a.tokens[s.at].kind == Kind::LParen {
                continue;
            }
            let mut j = s.at;
            while j < stmt.1 && (is_name(&a.tokens[j]) || a.tokens[j].kind == Kind::Dot) {
                source_tokens.push(j);
                j += 1;
            }
            if s.parts.len() == 1 && !s.parts[0].starts_with('#') && !ctes.iter().any(|c| eq(c, &s.parts[0]))
                && let Some(o) = resolve(s, schema).filter(|o| !o.schema.is_empty()) {
                    edits.push((a.starts[s.at], format!("{}.", quote(&o.schema)), Qualified::Object));
                }
        }
        // EXEC proc
        let first = stmt.0;
        if first < stmt.1 && matches!(a.up[first].as_str(), "EXEC" | "EXECUTE") {
            let name = next_sig(&a.tokens, first).filter(|&n| is_name(&a.tokens[n]) && a.tokens.get(n + 1).is_none_or(|t| t.kind != Kind::Dot));
            if let Some(n) = name {
                let s = Source { reference: String::new(), parts: vec![bare(&a.tokens[n])], columns: Vec::new(), at: n, alias_at: None };
                if let Some(o) = resolve(&s, schema).filter(|o| o.kind == "procedure" && !o.schema.is_empty()) {
                    edits.push((a.starts[n], format!("{}.", quote(&o.schema)), Qualified::Procedure));
                }
            }
        }
        for i in stmt.0..stmt.1 {
            let t = &a.tokens[i];
            let candidate = match t.kind {
                Kind::Word => !is_reserved(&a.up[i]) && !t.text.starts_with('#'),
                Kind::Bracket | Kind::Quoted => true,
                _ => false,
            };
            if !candidate || source_tokens.contains(&i) {
                continue;
            }
            let prev = prev_sig(&a.tokens, i);
            let next = next_sig(&a.tokens, i);
            if next.is_some_and(|n| matches!(a.tokens[n].kind, Kind::Dot | Kind::LParen)) || prev.is_some_and(|p| a.tokens[p].kind == Kind::Dot) {
                continue;
            }
            // An alias after an expression or after AS, not a column.
            let after_expression = prev.is_some_and(|p| match a.tokens[p].kind {
                Kind::Word => a.up[p] == "AS" || !is_reserved(&a.up[p]),
                Kind::Bracket | Kind::Quoted | Kind::RParen | Kind::Str | Kind::Number | Kind::Variable => true,
                _ => false,
            });
            if after_expression {
                continue;
            }
            let clause = a.clause_before(stmt.0, i);
            if !matches!(clause.as_str(), "SELECT" | "WHERE" | "ON" | "GROUP" | "ORDER" | "HAVING") {
                continue;
            }
            if clause == "SELECT" && next.is_some_and(|n| a.tokens[n].kind == Kind::Op && a.tokens[n].text == "=") {
                continue;
            }
            let in_scope = scope_sources(a, &d, stmt, i);
            if in_scope.len() < 2 {
                continue;
            }
            let name = bare(t);
            let owners: Vec<&Source> = in_scope.iter().filter(|s| source_columns(s, Some(schema)).iter().any(|(c, _)| eq(c, &name))).collect();
            if let [owner] = owners.as_slice() {
                edits.push((a.starts[i], format!("{}.", quote(&owner.reference)), Qualified::Column));
            }
        }
    }
    edits.sort_by_key(|e| e.0);
    edits.dedup_by_key(|e| e.0);
    edits
}

/// Adds or removes `AS` before table aliases.
pub fn table_aliases(text: &str, style: TableAlias) -> Result<String, String> {
    let lexed = lex(text).map_err(|e| e.to_string())?;
    let mut tokens = lexed.tokens;
    table_alias(&mut tokens, style);
    let mut out = String::with_capacity(text.len());
    for t in &tokens {
        out.push_str(&t.ws);
        out.push_str(&t.text);
    }
    out.push_str(&lexed.trailing_ws);
    Ok(out)
}

/// `DECLARE @name type` declarations in the script, with the type as written.
pub(crate) fn declared_types(a: &Analysis) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (s, e) in a.statements() {
        if s >= e || a.up[s] != "DECLARE" {
            continue;
        }
        let mut j = s + 1;
        while j < e {
            if a.tokens[j].kind == Kind::Variable {
                let name = a.tokens[j].text.clone();
                let mut k = j + 1;
                if k < e && a.up[k] == "AS" {
                    k += 1;
                }
                let mut ty = String::new();
                if k < e && a.tokens[k].kind == Kind::Word && a.up[k] != "TABLE" && a.up[k] != "CURSOR" {
                    ty.push_str(&a.tokens[k].text);
                    if k + 1 < e && a.tokens[k + 1].kind == Kind::LParen {
                        let close = a.matching(k + 1, e);
                        for t in &a.tokens[k + 1..=close.min(e - 1)] {
                            ty.push_str(&t.text);
                        }
                    }
                }
                if !ty.is_empty() && !out.iter().any(|(n, _)| eq(n, &name)) {
                    out.push((name, ty));
                }
            }
            // Skip to the next declaration in a list.
            let mut depth = 0usize;
            j += 1;
            while j < e {
                match a.tokens[j].kind {
                    Kind::LParen => depth += 1,
                    Kind::RParen => depth = depth.saturating_sub(1),
                    Kind::Comma if depth == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            j += 1;
        }
    }
    out
}

fn reindent(body: &str, indent: &str, nl: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let common = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.trim().is_empty() { String::new() } else { format!("{indent}{}", &l[common..]) })
        .collect::<Vec<_>>()
        .join(nl)
}

/// A `CREATE PROCEDURE` script whose body is `[start, end)` (UTF-16) of the script.
/// Variables the selection uses but does not declare become parameters.
pub fn encapsulate_as_procedure(text: &str, start: usize, end: usize, name: &str) -> Result<String, String> {
    let (bs, be) = (utf16_to_byte(text, start), utf16_to_byte(text, end));
    let selection = text[bs..be].trim_matches(['\r', '\n']);
    if selection.trim().is_empty() {
        return Err("select the statements to put in the procedure".into());
    }
    let nl = newline(text);
    let whole = Analysis::new(text);
    let types = declared_types(&whole);
    let inner = Analysis::new(selection);
    let declared_inside: Vec<String> = declared_types(&inner).into_iter().map(|(n, _)| n).collect();
    let mut params: Vec<String> = Vec::new();
    for t in &inner.tokens {
        let n = &t.text;
        if t.kind == Kind::Variable && !n.starts_with("@@") && !declared_inside.iter().any(|d| eq(d, n)) && !params.iter().any(|p| eq(p, n)) {
            params.push(n.clone());
        }
    }
    let mut out = format!("CREATE PROCEDURE {name}");
    for (i, p) in params.iter().enumerate() {
        let ty = types.iter().find(|(n, _)| eq(n, p)).map(|(_, t)| t.clone()).unwrap_or_else(|| "sql_variant /* type not found in the script */".into());
        out.push_str(&format!("{nl}    {p} {ty}{}", if i + 1 < params.len() { "," } else { "" }));
    }
    out.push_str(&format!("{nl}AS{nl}BEGIN{nl}    SET NOCOUNT ON;{nl}{nl}{}{nl}END;{nl}GO{nl}", reindent(selection, "    ", nl)));
    Ok(out)
}

pub enum Renamed {
    /// The script with every occurrence renamed, and how many there were.
    Text(String, usize),
    /// A script that renames a database object.
    Script(String),
}

fn token_at(a: &Analysis, at: usize) -> Option<usize> {
    (0..a.tokens.len()).find(|&i| a.starts[i] <= at && at <= end_of(a, i) && !matches!(a.tokens[i].kind, Kind::Dot | Kind::Comma | Kind::Semi | Kind::LParen | Kind::RParen | Kind::Op) && !a.tokens[i].is_comment())
}

fn batch_of(a: &Analysis, i: usize) -> (usize, usize) {
    let start = (0..i).rev().find(|&j| a.tokens[j].kind == Kind::Go).map_or(0, |j| j + 1);
    let end = (i..a.tokens.len()).find(|&j| a.tokens[j].kind == Kind::Go).unwrap_or(a.tokens.len());
    (start, end)
}

fn statement_of(a: &Analysis, i: usize) -> (usize, usize) {
    a.statements().into_iter().find(|&(s, e)| s <= i && i < e).unwrap_or((0, a.tokens.len()))
}

fn bracket_if_needed(name: &str) -> String {
    quote(name.trim_start_matches('[').trim_end_matches(']'))
}

/// Renames the variable, alias or CTE at the caret (UTF-16) throughout its scope. For a
/// table, view, procedure or column, returns an `sp_rename` script instead.
pub fn rename(text: &str, caret: usize, new_name: &str, schema: Option<&Schema>) -> Result<Renamed, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("enter a new name".into());
    }
    let a = Analysis::new(text);
    let at = utf16_to_byte(text, caret);
    let i = token_at(&a, at).ok_or("put the caret on a name to rename")?;
    let t = &a.tokens[i];
    if t.kind == Kind::Variable {
        if t.text.starts_with("@@") {
            return Err("global variables cannot be renamed".into());
        }
        let replacement = if new_name.starts_with('@') { new_name.to_string() } else { format!("@{new_name}") };
        let (s, e) = batch_of(&a, i);
        let edits: Vec<_> = (s..e).filter(|&j| a.tokens[j].kind == Kind::Variable && eq(&a.tokens[j].text, &t.text)).map(|j| (a.starts[j], end_of(&a, j), replacement.clone())).collect();
        let n = edits.len();
        return Ok(Renamed::Text(apply(text, edits), n));
    }
    if !is_name(t) {
        return Err("put the caret on a name to rename".into());
    }
    let name = bare(t);
    let replacement = bracket_if_needed(new_name);
    let stmt = statement_of(&a, i);
    let sources = a.sources(stmt.0, stmt.1);
    let qualifier_of = |j: usize| is_name(&a.tokens[j]) && a.tokens.get(j + 1).is_some_and(|n| n.kind == Kind::Dot) && (j == 0 || a.tokens[j - 1].kind != Kind::Dot);
    let alias = sources.iter().find(|s| s.alias_at.is_some() && eq(&s.reference, &name) && (s.alias_at == Some(i) || qualifier_of(i)));
    if let Some(src) = alias {
        let mut edits = vec![(a.starts[src.alias_at.expect("alias")], end_of(&a, src.alias_at.expect("alias")), replacement.clone())];
        for j in stmt.0..stmt.1 {
            if qualifier_of(j) && eq(&bare(&a.tokens[j]), &name) {
                edits.push((a.starts[j], end_of(&a, j), replacement.clone()));
            }
        }
        let n = edits.len();
        return Ok(Renamed::Text(apply(text, edits), n));
    }
    if a.ctes(stmt.0, stmt.1).iter().any(|c| eq(&c.reference, &name)) {
        let edits: Vec<_> = (stmt.0..stmt.1)
            .filter(|&j| is_name(&a.tokens[j]) && eq(&bare(&a.tokens[j]), &name) && (j == 0 || a.tokens[j - 1].kind != Kind::Dot))
            .map(|j| (a.starts[j], end_of(&a, j), replacement.clone()))
            .collect();
        let n = edits.len();
        return Ok(Renamed::Text(apply(text, edits), n));
    }
    let schema = schema.ok_or("connect to the database to rename tables, views, procedures and columns")?;
    let nl = newline(text);
    // A multipart name: walk to its first part.
    let mut first = i;
    while first >= 2 && a.tokens[first - 1].kind == Kind::Dot && is_name(&a.tokens[first - 2]) {
        first -= 2;
    }
    let mut parts = Vec::new();
    let mut j = first;
    while j <= i {
        parts.push(bare(&a.tokens[j]));
        j += 2;
    }
    let lookup = Source { reference: String::new(), parts: parts.clone(), columns: Vec::new(), at: first, alias_at: None };
    if let Some(o) = resolve(&lookup, schema) {
        let full = format!("{}.{}", o.schema, o.name);
        return Ok(Renamed::Script(format!(
            "-- Objects that reference {full} must be changed to use the new name:{nl}SELECT OBJECT_SCHEMA_NAME(d.referencing_id) AS SchemaName, OBJECT_NAME(d.referencing_id) AS ObjectName{nl}FROM sys.sql_expression_dependencies AS d{nl}WHERE d.referenced_id = OBJECT_ID(N'{}');{nl}{nl}EXEC sp_rename N'{}', N'{}';{nl}",
            full.replace('\'', "''"),
            full.replace('\'', "''"),
            new_name.replace('\'', "''")
        )));
    }
    // A column: `alias.column` or a column of the statement's only source.
    let owner = if parts.len() >= 2 {
        let q = &parts[parts.len() - 2];
        sources.iter().find(|s| eq(&s.reference, q))
    } else {
        let with_column: Vec<&Source> = sources.iter().filter(|s| source_columns(s, Some(schema)).iter().any(|(c, _)| eq(c, &name))).collect();
        if with_column.len() == 1 { Some(with_column[0]) } else { None }
    };
    if let Some(o) = owner.and_then(|s| resolve(s, schema)).filter(|o| o.columns.iter().any(|c| eq(&c.name, &name))) {
        let full = format!("{}.{}.{}", o.schema, o.name, name);
        return Ok(Renamed::Script(format!(
            "-- Code that uses {full} must be changed to use the new name.{nl}EXEC sp_rename N'{}', N'{}', N'COLUMN';{nl}",
            full.replace('\'', "''"),
            new_name.replace('\'', "''")
        )));
    }
    Err(format!("{name} is not a variable, alias, CTE or known object"))
}

struct ProcParam {
    name: String,
    data_type: String,
    default: Option<String>,
}

/// Parameters and body of a `CREATE`/`ALTER PROCEDURE` definition.
fn parse_procedure(definition: &str) -> Result<(Vec<ProcParam>, String), String> {
    let a = Analysis::new(definition);
    let n = a.tokens.len();
    let mut j = (0..n).find(|&j| matches!(a.up[j].as_str(), "PROC" | "PROCEDURE")).ok_or("not a procedure definition")? + 1;
    while j < n && (is_name(&a.tokens[j]) || a.tokens[j].kind == Kind::Dot) {
        j += 1;
    }
    let mut params = Vec::new();
    let mut depth = 0usize;
    let text_of = |from: usize, to: usize| definition[a.starts[from]..end_of(&a, to - 1)].to_string();
    while j < n {
        match a.tokens[j].kind {
            Kind::LParen => {
                depth += 1;
                j += 1;
                continue;
            }
            Kind::RParen => {
                depth = depth.saturating_sub(1);
                j += 1;
                continue;
            }
            Kind::Comma => {
                j += 1;
                continue;
            }
            Kind::Word if depth == 0 && matches!(a.up[j].as_str(), "AS" | "WITH" | "FOR") => break,
            Kind::Variable => {}
            _ => {
                j += 1;
                continue;
            }
        }
        let name = a.tokens[j].text.clone();
        j += 1;
        if j < n && a.up[j] == "AS" {
            j += 1;
        }
        let type_start = j;
        let mut inner = 0usize;
        let stop = |j: usize, inner: usize| {
            inner == 0
                && (matches!(a.tokens[j].kind, Kind::Comma | Kind::RParen)
                    || (a.tokens[j].kind == Kind::Op && a.tokens[j].text == "=")
                    || matches!(a.up[j].as_str(), "OUTPUT" | "OUT" | "READONLY" | "AS" | "WITH" | "NULL" | "VARYING"))
        };
        while j < n && !stop(j, inner) {
            match a.tokens[j].kind {
                Kind::LParen => inner += 1,
                Kind::RParen => inner -= 1,
                _ => {}
            }
            j += 1;
        }
        let data_type = text_of(type_start, j);
        let mut default = None;
        if j < n && a.tokens[j].text == "=" {
            let from = j + 1;
            j += 1;
            while j < n && !(matches!(a.tokens[j].kind, Kind::Comma | Kind::RParen) || matches!(a.up[j].as_str(), "OUTPUT" | "OUT" | "READONLY" | "AS" | "WITH")) {
                j += 1;
            }
            default = Some(text_of(from, j));
        }
        while j < n && matches!(a.up[j].as_str(), "OUTPUT" | "OUT" | "READONLY" | "VARYING") {
            j += 1;
        }
        params.push(ProcParam { name, data_type, default });
    }
    let as_at = (j..n).find(|&k| a.up[k] == "AS").ok_or("the definition has no AS")?;
    let mut last = n;
    while last > as_at + 1 && matches!(a.tokens[last - 1].kind, Kind::Go | Kind::Semi) {
        last -= 1;
    }
    let mut first = as_at + 1;
    if first >= last {
        return Err("the procedure body is empty".into());
    }
    // Drop a BEGIN ... END that wraps the whole body.
    if a.up[first] == "BEGIN" {
        let mut level = 0i32;
        let mut close = None;
        for k in first..last {
            match a.up[k].as_str() {
                "BEGIN" if !a.up.get(k + 1).is_some_and(|w| matches!(w.as_str(), "TRAN" | "TRANSACTION" | "DISTRIBUTED" | "DIALOG" | "CONVERSATION")) => level += 1,
                "CASE" => level += 1,
                "END" if a.tokens[k].kind == Kind::Word => {
                    level -= 1;
                    if level == 0 {
                        close = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        if close == Some(last - 1) {
            first += 1;
            last -= 1;
        }
    }
    if a.up[first..last].iter().zip(&a.tokens[first..last]).any(|(w, t)| t.kind == Kind::Word && w == "RETURN") {
        return Err("the procedure uses RETURN, which would end the calling batch; inline it by hand".into());
    }
    // Start at the beginning of the first line so that every line keeps its relative indent.
    let mut from = if first < last { a.starts[first] } else { 0 };
    while from > 0 && matches!(definition.as_bytes()[from - 1], b' ' | b'\t') {
        from -= 1;
    }
    let body = if first < last { definition[from..end_of(&a, last - 1)].to_string() } else { String::new() };
    Ok((params, body))
}

/// The procedure name of the `EXEC` statement at the caret (UTF-16), as written.
pub fn exec_target(text: &str, caret: usize) -> Option<String> {
    let a = Analysis::new(text);
    let (s, e) = a.statement_at(utf16_to_byte(text, caret));
    let s = (s..e).find(|&j| !a.tokens[j].is_comment())?;
    if !matches!(a.up[s].as_str(), "EXEC" | "EXECUTE") {
        return None;
    }
    let mut j = s + 1;
    if a.tokens.get(j).is_some_and(|t| t.kind == Kind::Variable) && a.tokens.get(j + 1).is_some_and(|t| t.text == "=") {
        j += 2;
    }
    let first = j;
    while j < e && (is_name(&a.tokens[j]) || a.tokens[j].kind == Kind::Dot) {
        j += 1;
    }
    (j > first).then(|| text[a.starts[first]..end_of(&a, j - 1)].to_string())
}

/// Replaces the `EXEC` statement at the caret with the procedure's body.
pub fn inline_exec(text: &str, caret: usize, definition: &str) -> Result<String, String> {
    let a = Analysis::new(text);
    let at = utf16_to_byte(text, caret);
    let (s, e) = a.statement_at(at);
    let s = (s..e).find(|&j| !a.tokens[j].is_comment()).ok_or("put the caret in an EXEC statement")?;
    if !matches!(a.up[s].as_str(), "EXEC" | "EXECUTE") {
        return Err("put the caret in an EXEC statement".into());
    }
    let mut j = s + 1;
    if a.tokens.get(j).is_some_and(|t| t.kind == Kind::Variable) && a.tokens.get(j + 1).is_some_and(|t| t.text == "=") {
        return Err("the return value is used; inline it by hand".into());
    }
    let name_start = j;
    while j < e && (is_name(&a.tokens[j]) || a.tokens[j].kind == Kind::Dot) {
        j += 1;
    }
    let name = text[a.starts[name_start]..end_of(&a, j - 1)].to_string();
    let mut end = e;
    while end > j && matches!(a.tokens[end - 1].kind, Kind::Semi | Kind::Go) {
        end -= 1;
    }
    // Arguments, split at top-level commas.
    let mut args: Vec<(Option<String>, String, bool)> = Vec::new();
    let mut from = j;
    let mut depth = 0usize;
    for k in j..=end {
        let split = k == end || (a.tokens[k].kind == Kind::Comma && depth == 0);
        if k < end {
            match a.tokens[k].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if !split {
            continue;
        }
        if from < k {
            let mut f = from;
            let mut named = None;
            if a.tokens[f].kind == Kind::Variable && f + 1 < k && a.tokens[f + 1].text == "=" {
                named = Some(a.tokens[f].text.clone());
                f += 2;
            }
            let mut l = k;
            let output = matches!(a.up[l - 1].as_str(), "OUTPUT" | "OUT");
            if output {
                l -= 1;
            }
            if f < l {
                args.push((named, text[a.starts[f]..end_of(&a, l - 1)].to_string(), output));
            }
        }
        from = k + 1;
    }
    let (params, body) = parse_procedure(definition)?;
    let nl = newline(text);
    let indent = line_indent(text, a.starts[s]);
    let inner = format!("{indent}    ");
    let mut out = format!("BEGIN -- inlined {name}{nl}");
    let mut copy_back = Vec::new();
    for (index, p) in params.iter().enumerate() {
        let arg = args.iter().find(|(n, _, _)| n.as_deref().is_some_and(|n| eq(n, &p.name))).or_else(|| args.get(index).filter(|(n, _, _)| n.is_none()));
        let value = match (arg, &p.default) {
            (Some((_, v, _)), _) if !eq(v, "DEFAULT") => v.clone(),
            (_, Some(d)) => d.clone(),
            _ => return Err(format!("no value for {}", p.name)),
        };
        if let Some((_, v, true)) = arg {
            copy_back.push(format!("{inner}SET {v} = {};{nl}", p.name));
        }
        out.push_str(&format!("{inner}DECLARE {} {} = {value};{nl}", p.name, p.data_type));
    }
    out.push_str(&reindent(&body, &inner, nl));
    out.push_str(nl);
    for c in copy_back {
        out.push_str(&c);
    }
    out.push_str(&format!("{indent}END;"));
    let stop = if e > end { end_of(&a, e - 1) } else { end_of(&a, end - 1) };
    let stop = if a.tokens[e - 1].kind == Kind::Go { end_of(&a, end - 1) } else { stop };
    Ok(apply(text, vec![(a.starts[s], stop, out)]))
}
