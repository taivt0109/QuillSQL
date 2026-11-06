//! Optional token rewrites that intentionally change the token stream.
//! When a position is ambiguous they leave the code alone: a missed semicolon
//! is harmless, a misplaced one breaks the script.

use crate::keywords::{is_contextual, is_data_type, is_function, is_reserved};
use crate::lexer::{Kind, Token};
use crate::options::{Enclose, TableAlias, WordCase};

pub fn upper_words(tokens: &[Token]) -> Vec<String> {
    tokens
        .iter()
        .map(|t| if t.kind == Kind::Word { t.text.to_ascii_uppercase() } else { t.text.clone() })
        .collect()
}

pub fn prev_sig(tokens: &[Token], i: usize) -> Option<usize> {
    (0..i).rev().find(|&j| !tokens[j].is_comment())
}

pub fn next_sig(tokens: &[Token], i: usize) -> Option<usize> {
    (i + 1..tokens.len()).find(|&j| !tokens[j].is_comment())
}

fn word_at(up: &[String], at: Option<usize>) -> &str {
    at.map_or("", |j| up[j].as_str())
}

pub struct Casing {
    pub keywords: Option<WordCase>,
    pub functions: Option<WordCase>,
    pub data_types: Option<WordCase>,
}

/// Index of the `(` enclosing token `i`, if any.
fn enclosing_paren(tokens: &[Token], i: usize) -> Option<usize> {
    let mut depth = 0usize;
    for j in (0..i).rev() {
        match tokens[j].kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => return Some(j),
            Kind::LParen => depth -= 1,
            _ => {}
        }
    }
    None
}

fn enclosing_call<'a>(tokens: &[Token], up: &'a [String], i: usize) -> &'a str {
    enclosing_paren(tokens, i).and_then(|j| prev_sig(tokens, j)).map_or("", |p| up[p].as_str())
}

/// True inside `CREATE TABLE name (`, `@t TABLE (` or `RETURNS @t TABLE (`.
fn in_table_definition(tokens: &[Token], up: &[String], i: usize) -> bool {
    let Some(open) = enclosing_paren(tokens, i) else { return false };
    let mut at = prev_sig(tokens, open);
    // Walk back over a possibly multipart table name.
    for _ in 0..6 {
        match at {
            Some(j) if up[j] == "TABLE" => return true,
            Some(j) if matches!(tokens[j].kind, Kind::Word | Kind::Bracket | Kind::Quoted | Kind::Dot) => {
                at = prev_sig(tokens, j);
            }
            _ => return false,
        }
    }
    false
}

/// Data type names are also legal column names, so they are only recased where
/// a type is expected: after a variable, in CAST/CONVERT, or after a column name
/// in a table definition or ALTER TABLE.
fn in_type_position(tokens: &[Token], up: &[String], i: usize) -> bool {
    let Some(p) = prev_sig(tokens, i) else { return false };
    let pp = prev_sig(tokens, p).map_or("", |j| up[j].as_str());
    match (tokens[p].kind, up[p].as_str()) {
        (Kind::Variable, _) | (Kind::Word, "RETURNS") => true,
        (Kind::Word, "AS") => {
            matches!(enclosing_call(tokens, up, i), "CAST" | "TRY_CAST")
                || prev_sig(tokens, p).is_some_and(|j| tokens[j].kind == Kind::Variable)
        }
        (Kind::LParen, _) => matches!(enclosing_call(tokens, up, i), "CONVERT" | "TRY_CONVERT"),
        (Kind::Word | Kind::Bracket | Kind::Quoted, _) => {
            matches!(pp, "ADD" | "COLUMN") || (matches!(pp, "(" | ",") && in_table_definition(tokens, up, i))
        }
        _ => false,
    }
}

pub fn apply_casing(tokens: &mut [Token], casing: &Casing) {
    let up = upper_words(tokens);
    for i in 0..tokens.len() {
        let after_dot = prev_sig(tokens, i).is_some_and(|p| tokens[p].kind == Kind::Dot);
        let called = next_sig(tokens, i).is_some_and(|n| tokens[n].kind == Kind::LParen);
        // PARTITION is unreserved, but before BY it can only be the keyword.
        let partition_by = up[i] == "PARTITION" && next_sig(tokens, i).is_some_and(|n| up[n] == "BY");
        let case = match tokens[i].kind {
            Kind::Go => casing.keywords,
            Kind::Word if after_dot => None,
            Kind::Word if called && is_function(&up[i]) => casing.functions,
            Kind::Word if is_reserved(&up[i]) || partition_by => casing.keywords,
            Kind::Word if is_data_type(&up[i]) && in_type_position(tokens, &up, i) => casing.data_types,
            _ => None,
        };
        if let Some(case) = case {
            tokens[i].text = case.apply(&tokens[i].text);
        }
    }
}

/// Adds `N` to non-ASCII string literals assigned to a variable.
pub fn prefix_unicode(tokens: &mut [Token]) {
    for i in 0..tokens.len() {
        let t = &tokens[i];
        if t.kind != Kind::Str || !t.text.starts_with('\'') || t.text.is_ascii() {
            continue;
        }
        let Some(eq) = prev_sig(tokens, i).filter(|&p| tokens[p].text == "=") else {
            continue;
        };
        // Walk back over an optional type, as in `DECLARE @a NVARCHAR(10) = 'x'`.
        let mut at = prev_sig(tokens, eq);
        let mut assigns = false;
        for _ in 0..8 {
            let Some(j) = at else { break };
            match tokens[j].kind {
                Kind::Variable => {
                    assigns = true;
                    break;
                }
                Kind::Word | Kind::Number | Kind::LParen | Kind::RParen | Kind::Comma => {
                    at = prev_sig(tokens, j);
                }
                _ => break,
            }
        }
        if assigns {
            tokens[i].text.insert(0, 'N');
        }
    }
}

fn is_plain_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn enclose_identifiers(tokens: &mut [Token], style: Enclose) {
    let up = upper_words(tokens);
    for i in 0..tokens.len() {
        let prev = prev_sig(tokens, i);
        let next = next_sig(tokens, i);
        match style {
            Enclose::WithoutBrackets => {
                if tokens[i].kind != Kind::Bracket {
                    continue;
                }
                let inner = &tokens[i].text[1..tokens[i].text.len() - 1];
                let upper = inner.to_ascii_uppercase();
                if is_plain_identifier(inner) && !is_reserved(&upper) && !is_contextual(&upper) {
                    tokens[i].text = inner.to_string();
                    tokens[i].kind = Kind::Word;
                }
            }
            Enclose::WithBrackets => {
                if tokens[i].kind != Kind::Word
                    || !is_plain_identifier(&tokens[i].text)
                    || is_reserved(&up[i])
                    || is_contextual(&up[i])
                    || next.is_some_and(|n| tokens[n].kind == Kind::LParen)
                {
                    continue;
                }
                let after_dot = prev.is_some_and(|p| tokens[p].kind == Kind::Dot);
                let before_dot = next.is_some_and(|n| tokens[n].kind == Kind::Dot);
                let object_position = matches!(word_at(&up, prev), "FROM" | "JOIN" | "INTO");
                if (after_dot && chain_starts_with_name(tokens, i)) || before_dot || object_position {
                    tokens[i].text = format!("[{}]", tokens[i].text);
                    tokens[i].kind = Kind::Bracket;
                }
            }
        }
    }
}

/// False for `@xml.value` or `f(x).method`: only real multipart names get brackets.
fn chain_starts_with_name(tokens: &[Token], i: usize) -> bool {
    let mut at = i;
    while let Some(dot) = prev_sig(tokens, at).filter(|&p| tokens[p].kind == Kind::Dot) {
        match prev_sig(tokens, dot) {
            Some(p) if matches!(tokens[p].kind, Kind::Word | Kind::Bracket | Kind::Quoted | Kind::Dot) => {
                at = p;
            }
            _ => return false,
        }
    }
    true
}

fn is_alias_candidate(t: &Token, upper: &str) -> bool {
    match t.kind {
        Kind::Bracket | Kind::Quoted => true,
        Kind::Word => !is_reserved(upper) && !is_contextual(upper),
        _ => false,
    }
}

fn ends_table_source(t: &Token, upper: &str) -> bool {
    matches!(t.kind, Kind::RParen | Kind::Variable | Kind::Bracket | Kind::Quoted | Kind::Str)
        || (t.kind == Kind::Word && !is_reserved(upper) && upper != "SYSTEM_TIME")
}

pub fn table_alias(tokens: &mut Vec<Token>, style: TableAlias) {
    let up = upper_words(tokens);
    let mut in_from = vec![false];
    let mut remove = Vec::new();
    let mut add = Vec::new();
    for i in 0..tokens.len() {
        let t = &tokens[i];
        match t.kind {
            Kind::LParen => in_from.push(false),
            Kind::RParen if in_from.len() > 1 => {
                in_from.pop();
            }
            Kind::Go | Kind::Semi => *in_from.last_mut().unwrap() = false,
            _ => {}
        }
        if t.is_comment() || matches!(t.kind, Kind::LParen | Kind::RParen) {
            continue;
        }
        let from = *in_from.last().unwrap();
        let prev = prev_sig(tokens, i);
        let next = next_sig(tokens, i);
        if t.kind == Kind::Word {
            match up[i].as_str() {
                "FROM" | "JOIN" | "APPLY" => *in_from.last_mut().unwrap() = true,
                "FOR" if word_at(&up, next) == "SYSTEM_TIME" => {}
                "SELECT" | "WHERE" | "GROUP" | "ORDER" | "HAVING" | "ON" | "UNION" | "EXCEPT"
                | "INTERSECT" | "SET" | "OPTION" | "WHEN" | "FOR" | "INTO" => {
                    *in_from.last_mut().unwrap() = false;
                }
                _ => {}
            }
        }
        if !from {
            continue;
        }
        let prev_ok = prev.is_some_and(|p| ends_table_source(&tokens[p], &up[p]));
        match style {
            TableAlias::RemoveAs => {
                let next_ok =
                    next.is_some_and(|n| is_alias_candidate(&tokens[n], &up[n]) && up[n] != "OF");
                if up[i] == "AS" && prev_ok && next_ok {
                    remove.push(i);
                }
            }
            TableAlias::AddAs => {
                let next_dot = next.is_some_and(|n| tokens[n].kind == Kind::Dot);
                if is_alias_candidate(t, &up[i]) && prev_ok && !next_dot {
                    add.push(i);
                }
            }
        }
    }
    for &i in remove.iter().rev() {
        tokens.remove(i);
    }
    for &i in add.iter().rev() {
        tokens.insert(i, Token::synth(Kind::Word, "AS", " "));
    }
}

const STATEMENT_STARTS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "DECLARE", "SET", "PRINT", "EXEC", "EXECUTE",
    "RETURN", "DROP", "TRUNCATE", "COMMIT", "ROLLBACK", "USE", "RAISERROR", "WAITFOR", "OPEN",
    "CLOSE", "FETCH", "DEALLOCATE", "BREAK", "CONTINUE", "GOTO", "BACKUP", "RESTORE", "CREATE",
    "ALTER",
];

/// True when a statement-starting word is really inside the current statement.
fn continues_statement(w: &str, p: &str, n: &str, kind: &str) -> bool {
    matches!(
        p,
        "," | "UNION" | "ALL" | "EXCEPT" | "INTERSECT" | "THEN" | "FOR" | "AFTER" | "OF" | "WITH"
            | "GRANT" | "DENY" | "REVOKE" | "RETURN" | "AS" | "OR" | "BULK" | "ROW" | "ROWS"
    ) || (p == "ON" && matches!(w, "DELETE" | "UPDATE" | "INSERT") && kind != "SET")
        || (matches!(p, "DELETE" | "UPDATE") && w == "SET")
        || (w == "SET" && matches!(kind, "UPDATE" | "MERGE" | "ALTER" | "CREATE"))
        || (matches!(w, "SELECT" | "EXEC" | "EXECUTE") && kind == "INSERT")
        || (matches!(w, "DROP" | "ALTER") && kind == "ALTER")
        || (w == "UPDATE" && n == "(")
        || (w == "MERGE" && n == "JOIN")
}

pub fn insert_semicolons(tokens: &mut Vec<Token>) {
    for &i in missing_terminators(tokens).iter().rev() {
        tokens.insert(i + 1, Token::synth(Kind::Semi, ";", ""));
    }
}

/// Indices of the last token of each statement that has no `;`, in ascending order.
pub fn missing_terminators(tokens: &[Token]) -> Vec<usize> {
    let up = upper_words(tokens);
    let mut depth = 0usize;
    let mut cases = 0usize;
    let mut open = false; // a statement is waiting for its terminator
    let mut header = false; // the next statement belongs to IF/ELSE/WHILE/CTE/module header
    let mut kind = String::new();
    let mut last: Option<usize> = None;
    let mut inserts = Vec::new();

    for i in 0..tokens.len() {
        let t = &tokens[i];
        if t.is_comment() {
            continue;
        }
        match t.kind {
            Kind::LParen => depth += 1,
            Kind::RParen => depth = depth.saturating_sub(1),
            Kind::Semi if depth == 0 => {
                open = false;
                header = false;
                kind.clear();
            }
            Kind::Go | Kind::Raw => {
                if open && !header {
                    inserts.extend(last);
                }
                open = false;
                header = false;
                cases = 0;
                depth = 0;
                kind.clear();
                last = None;
                continue;
            }
            _ => {}
        }
        if depth == 0 && t.kind == Kind::Word {
            let w = up[i].as_str();
            let p = word_at(&up, prev_sig(tokens, i));
            let n = word_at(&up, next_sig(tokens, i));
            match w {
                "CASE" => cases += 1,
                "END" if cases > 0 => cases -= 1,
                "ELSE" if cases > 0 => {}
                "END" | "ELSE" => {
                    if open {
                        inserts.extend(last);
                    }
                    open = false;
                    header = w == "ELSE";
                    kind.clear();
                }
                "BEGIN" if matches!(n, "TRAN" | "TRANSACTION" | "DISTRIBUTED") => {
                    if open && !header {
                        inserts.extend(last);
                    }
                    open = true;
                    header = false;
                    kind = w.to_string();
                }
                "BEGIN" => {
                    if open {
                        inserts.extend(last);
                    }
                    open = false;
                    header = false;
                    kind.clear();
                }
                "IF" if n == "EXISTS" && matches!(kind.as_str(), "DROP" | "ALTER") => {}
                "IF" | "WHILE" => {
                    if open {
                        inserts.extend(last);
                    }
                    open = false;
                    header = true;
                    kind = w.to_string();
                }
                "WITH" if n != "(" && !open => {
                    header = true;
                    kind = w.to_string();
                }
                _ if header && p == "AS" && STATEMENT_STARTS.contains(&w) => {
                    header = false;
                    open = true;
                    kind = w.to_string();
                }
                _ if STATEMENT_STARTS.contains(&w) && !continues_statement(w, p, n, &kind) => {
                    if open && !header {
                        inserts.extend(last);
                    }
                    let module = matches!(w, "CREATE" | "ALTER")
                        && matches!(n, "PROC" | "PROCEDURE" | "FUNCTION" | "VIEW" | "TRIGGER" | "OR");
                    open = !module;
                    header = module;
                    kind = w.to_string();
                }
                _ => {}
            }
        }
        last = Some(i);
    }
    if open && !header {
        inserts.extend(last);
    }
    inserts.sort_unstable();
    inserts.dedup();
    inserts
}
