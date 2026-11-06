//! Editing commands that rewrite or inspect a script as text: semicolons, unformat, brackets,
//! casing, comments, `sp_executesql`, statement ranges, an outline and execution warnings.

use serde::Serialize;

use crate::analysis::analyze;
use crate::complete::{Analysis, byte_to_utf16, utf16_to_byte};
use crate::lexer::{Kind, Token, lex};
use crate::options::{Enclose, WordCase};
use crate::refactor::end_of;
use crate::transform::{Casing, apply_casing, enclose_identifiers, insert_semicolons, next_sig};

fn render(tokens: &[Token], trailing: &str) -> String {
    let mut out = String::new();
    for t in tokens {
        out.push_str(&t.ws);
        out.push_str(&t.text);
    }
    out.push_str(trailing);
    out
}

fn rewrite(text: &str, change: impl FnOnce(&mut Vec<Token>)) -> Result<String, String> {
    let lexed = lex(text).map_err(|e| e.to_string())?;
    let mut tokens = lexed.tokens;
    change(&mut tokens);
    Ok(render(&tokens, &lexed.trailing_ws))
}

/// Adds the statement terminators the script leaves out.
pub fn semicolons(text: &str) -> Result<String, String> {
    rewrite(text, insert_semicolons)
}

/// Puts the script on as few lines as it can: single spaces between tokens, with line breaks
/// kept only where T-SQL needs them (after a line comment, around GO and sqlcmd lines).
pub fn unformat(text: &str) -> Result<String, String> {
    rewrite(text, |tokens| {
        for i in 0..tokens.len() {
            let after_line = i > 0 && matches!(tokens[i - 1].kind, Kind::LineComment | Kind::Go | Kind::Raw);
            let own_line = matches!(tokens[i].kind, Kind::Go | Kind::Raw);
            tokens[i].ws = if i == 0 {
                String::new()
            } else if after_line || own_line {
                "\n".into()
            } else if tokens[i].ws.is_empty() {
                String::new()
            } else {
                " ".into()
            };
        }
    })
}

/// Adds brackets to object and column names, or removes them where they are not needed.
pub fn brackets(text: &str, add: bool) -> Result<String, String> {
    rewrite(text, |tokens| enclose_identifiers(tokens, if add { Enclose::WithBrackets } else { Enclose::WithoutBrackets }))
}

/// Upper- or lower-cases keywords, built-in functions and data types.
pub fn casing(text: &str, upper: bool) -> Result<String, String> {
    let case = if upper { WordCase::Upper } else { WordCase::Lower };
    rewrite(text, |tokens| apply_casing(tokens, &Casing { keywords: Some(case), functions: Some(case), data_types: Some(case) }))
}

/// Removes every comment, keeping the line breaks around line comments.
pub fn remove_comments(text: &str) -> Result<String, String> {
    rewrite(text, |tokens| {
        let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
        let mut carry = String::new();
        for t in tokens.drain(..) {
            if t.is_comment() {
                // The comment's own leading space is dropped; a line break before it is kept.
                if t.ws.contains('\n') && !carry.contains('\n') {
                    carry = t.ws.clone();
                }
                continue;
            }
            let mut t = t;
            if !carry.is_empty() {
                if !t.ws.contains('\n') {
                    t.ws = if carry.contains('\n') { carry.clone() } else { t.ws };
                }
                carry.clear();
            }
            out.push(t);
        }
        *tokens = out;
    })
}

/// Unescapes a T-SQL string literal (`N'it''s'` -> `it's`).
fn string_value(literal: &str) -> Option<String> {
    let body = literal.strip_prefix(['N', 'n']).unwrap_or(literal);
    let inner = body.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'"))
}

/// Replaces `EXEC sp_executesql N'...', N'@p int', @p = 1` at the caret (UTF-16) with
/// `DECLARE @p int = 1;` followed by the SQL it runs.
pub fn convert_sp_executesql(text: &str, caret: usize) -> Result<String, String> {
    let a = Analysis::new(text);
    let (s, e) = a.statement_at(utf16_to_byte(text, caret));
    let s = (s..e).find(|&j| !a.tokens[j].is_comment()).ok_or("put the caret on an EXEC sp_executesql statement")?;
    let mut j = s;
    if !matches!(a.up[j].as_str(), "EXEC" | "EXECUTE") {
        return Err("put the caret on an EXEC sp_executesql statement".into());
    }
    j += 1;
    // [sys.]sp_executesql
    while j < e && (a.tokens[j].kind == Kind::Dot || (a.tokens[j].kind == Kind::Word && a.up[j] != "SP_EXECUTESQL")) {
        j += 1;
    }
    if j >= e || a.up[j] != "SP_EXECUTESQL" {
        return Err("the statement does not call sp_executesql".into());
    }
    j += 1;
    let mut end = e;
    while end > j && matches!(a.tokens[end - 1].kind, Kind::Semi | Kind::Go) {
        end -= 1;
    }
    // Top-level arguments.
    let mut args: Vec<(usize, usize)> = Vec::new();
    let mut from = j;
    let mut depth = 0usize;
    for k in j..=end {
        if k < end {
            match a.tokens[k].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => depth = depth.saturating_sub(1),
                Kind::Comma if depth == 0 => {}
                _ => continue,
            }
            if !(a.tokens[k].kind == Kind::Comma && depth == 0) {
                continue;
            }
        }
        if from < k {
            args.push((from, k));
        }
        from = k + 1;
    }
    let arg_text = |(f, t): (usize, usize)| text[a.starts[f]..end_of(&a, t - 1)].to_string();
    let named = |(f, t): (usize, usize)| -> Option<(String, String)> {
        (t - f >= 3 && a.tokens[f].kind == Kind::Variable && a.tokens[f + 1].text == "=").then(|| (a.tokens[f].text.clone(), arg_text((f + 2, t))))
    };
    let first = *args.first().ok_or("sp_executesql has no statement")?;
    let statement_arg = named(first).map(|(_, v)| v).unwrap_or_else(|| arg_text(first));
    let sql = string_value(statement_arg.trim()).ok_or("the statement is not a string literal, so it cannot be converted")?;
    let mut declarations = Vec::new();
    if let Some(&second) = args.get(1) {
        let definition_arg = named(second).map(|(_, v)| v).unwrap_or_else(|| arg_text(second));
        let definition = string_value(definition_arg.trim()).ok_or("the parameter list is not a string literal")?;
        let values: Vec<(Option<String>, String)> = args[2..]
            .iter()
            .map(|&r| named(r).map(|(n, v)| (Some(n), v)).unwrap_or((None, arg_text(r))))
            .collect();
        // Split the definition at top-level commas.
        let mut parts = Vec::new();
        let mut level = 0usize;
        let mut current = String::new();
        for c in definition.chars() {
            match c {
                '(' => level += 1,
                ')' => level = level.saturating_sub(1),
                ',' if level == 0 => {
                    parts.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            }
            current.push(c);
        }
        parts.push(current);
        for (index, part) in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).enumerate() {
            let mut words = part.splitn(2, char::is_whitespace);
            let name = words.next().unwrap_or("").to_string();
            let mut ty = words.next().unwrap_or("").trim().to_string();
            for suffix in [" OUTPUT", " OUT", " output", " out"] {
                if let Some(t) = ty.strip_suffix(suffix) {
                    ty = t.trim().to_string();
                }
            }
            let value = values
                .iter()
                .find(|(n, _)| n.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&name)))
                .or_else(|| values.get(index).filter(|(n, _)| n.is_none()))
                .map(|(_, v)| v.trim().trim_end_matches(" OUTPUT").trim_end_matches(" OUT").to_string());
            declarations.push(match value {
                Some(v) => format!("DECLARE {name} {ty} = {v};"),
                None => format!("DECLARE {name} {ty};"),
            });
        }
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut replacement = declarations.join(nl);
    if !replacement.is_empty() {
        replacement.push_str(nl);
    }
    replacement.push_str(sql.trim());
    let stop = end_of(&a, (s..e).rev().find(|&k| !matches!(a.tokens[k].kind, Kind::Go)).unwrap_or(s));
    let mut out = text.to_string();
    out.replace_range(a.starts[s]..stop, &replacement);
    Ok(out)
}

/// UTF-16 range of the statement at the caret, from its first token to its terminator.
pub fn statement_range(text: &str, caret: usize) -> Option<(usize, usize)> {
    let a = Analysis::new(text);
    let at = utf16_to_byte(text, caret);
    let (s, e) = a.statement_at(at);
    let first = (s..e).find(|&j| !a.tokens[j].is_comment() && a.tokens[j].kind != Kind::Go)?;
    let last = (first..e).rev().find(|&j| a.tokens[j].kind != Kind::Go)?;
    Some((byte_to_utf16(text, a.starts[first]), byte_to_utf16(text, end_of(&a, last))))
}

/// UTF-16 range of the batch (between GO lines) at the caret.
pub fn batch_range(text: &str, caret: usize) -> Option<(usize, usize)> {
    let a = Analysis::new(text);
    let at = utf16_to_byte(text, caret);
    let mut start = 0;
    let mut end = text.len();
    for (j, t) in a.tokens.iter().enumerate() {
        if t.kind != Kind::Go {
            continue;
        }
        if a.starts[j] < at {
            start = end_of(&a, j);
        } else {
            end = a.starts[j];
            break;
        }
    }
    let slice = &text[start..end];
    let lead = slice.len() - slice.trim_start().len();
    let trail = slice.len() - slice.trim_end().len();
    (lead + trail < slice.len()).then(|| (byte_to_utf16(text, start + lead), byte_to_utf16(text, end - trail)))
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct OutlineEntry {
    /// UTF-16 offset of the statement.
    pub start: usize,
    pub line: usize,
    pub kind: String,
    pub summary: String,
}

/// One entry per statement: its first keywords and the start of its text.
pub fn outline(text: &str) -> Vec<OutlineEntry> {
    let a = Analysis::new(text);
    let mut out = Vec::new();
    for (s, e) in a.statements() {
        let Some(first) = (s..e).find(|&j| !a.tokens[j].is_comment() && !matches!(a.tokens[j].kind, Kind::Go | Kind::Semi)) else { continue };
        let mut kind = a.up[first].clone();
        if let Some(n) = next_sig(&a.tokens, first).filter(|&n| n < e && a.tokens[n].kind == Kind::Word)
            && matches!(kind.as_str(), "CREATE" | "ALTER" | "DROP" | "BEGIN" | "INSERT") {
                kind = format!("{kind} {}", a.up[n]);
            }
        let last = (first..e).rev().find(|&j| a.tokens[j].kind != Kind::Go).unwrap_or(first);
        let body = text[a.starts[first]..end_of(&a, last)].split_whitespace().collect::<Vec<_>>().join(" ");
        let summary = if body.chars().count() > 100 { format!("{}...", body.chars().take(100).collect::<String>()) } else { body };
        out.push(OutlineEntry {
            start: byte_to_utf16(text, a.starts[first]),
            line: text[..a.starts[first]].matches('\n').count() + 1,
            kind,
            summary,
        });
    }
    out
}

/// Statements that can lose data, for a confirmation before the script runs.
pub fn execution_warnings(text: &str) -> Vec<String> {
    let line_of = |utf16: usize| {
        let byte = utf16_to_byte(text, utf16);
        text[..byte].matches('\n').count() + 1
    };
    let mut out: Vec<(usize, String)> = analyze(text, None)
        .into_iter()
        .filter(|i| matches!(i.code, "BP017" | "BP018"))
        .map(|i| {
            let verb = if i.code == "BP017" { "DELETE" } else { "UPDATE" };
            (i.start, format!("line {}: {verb} without WHERE changes every row", line_of(i.start)))
        })
        .collect();
    let a = Analysis::new(text);
    for (s, e) in a.statements() {
        let Some(first) = (s..e).find(|&j| !a.tokens[j].is_comment()) else { continue };
        let next = next_sig(&a.tokens, first).filter(|&n| n < e).map(|n| a.up[n].as_str()).unwrap_or("");
        let what = match (a.up[first].as_str(), next) {
            ("TRUNCATE", "TABLE") => "TRUNCATE TABLE removes every row",
            ("DROP", "TABLE") => "DROP TABLE deletes a table and its data",
            ("DROP", "DATABASE") => "DROP DATABASE deletes a database",
            _ => continue,
        };
        // Skip `DROP TABLE #temp`.
        let target = next_sig(&a.tokens, first).and_then(|n| next_sig(&a.tokens, n));
        let target = target.filter(|&t| t < e && !(a.up[t] == "IF")).or_else(|| target.and_then(|t| next_sig(&a.tokens, t)).and_then(|t| next_sig(&a.tokens, t)));
        if target.is_some_and(|t| a.tokens[t].text.starts_with('#') || a.tokens[t].text.starts_with('@')) {
            continue;
        }
        let at = byte_to_utf16(text, a.starts[first]);
        out.push((at, format!("line {}: {what}", line_of(at))));
    }
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, m)| m).collect()
}
