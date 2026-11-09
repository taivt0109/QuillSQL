//! Parameter hints: the signature of the function or procedure whose arguments the caret is in,
//! and which argument it is at.

use serde::Serialize;

use crate::complete::{Schema, bare, eq, is_name, object_matches, utf16_to_byte};
use crate::keywords::is_reserved;
use crate::lexer::{Kind, Token, lex_partial};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Signature {
    /// The name as it is shown: `DATEADD`, `dbo.usp_GetOrders`.
    pub name: String,
    /// Each parameter as it is shown: `datepart`, `@CustomerId int`.
    pub params: Vec<String>,
    /// The parameter the caret is at; past the last one for extra arguments.
    pub active: usize,
    /// What the function returns, if known.
    pub returns: String,
}

/// The signature at the caret, or None when it is not inside a call's arguments.
pub fn signature_at(text: &str, caret_utf16: usize, schema: Option<&Schema>) -> Option<Signature> {
    let caret = utf16_to_byte(text, caret_utf16);
    let (head, error) = lex_partial(&text[..caret]);
    // Not inside a string or comment.
    if error.is_some() {
        return None;
    }
    if head.last().is_some_and(|t| t.kind == Kind::LineComment) {
        return None;
    }
    let code: Vec<&Token> = head.iter().filter(|t| !t.is_comment()).collect();

    // The innermost `(` still open at the caret, with the commas after it at its own depth.
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut open = None;
    for (i, t) in code.iter().enumerate().rev() {
        match t.kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => {
                open = Some(i);
                break;
            }
            Kind::LParen => depth -= 1,
            Kind::Comma if depth == 0 => commas += 1,
            Kind::Semi | Kind::Go => break,
            _ => {}
        }
    }
    if let Some(o) = open {
        return if o > 0 { call(&code, o, commas, schema) } else { None };
    }
    let touching = text[..caret].chars().last().is_some_and(|c| !c.is_whitespace());
    exec(&code, touching, schema)
}

/// The function called by `name (` with the `(` at `o`.
fn call(code: &[&Token], o: usize, commas: usize, schema: Option<&Schema>) -> Option<Signature> {
    let mut j = o - 1;
    if !matches!(code[j].kind, Kind::Word | Kind::Bracket | Kind::Quoted) {
        return None;
    }
    let mut parts = vec![bare(code[j])];
    while j >= 2 && code[j - 1].kind == Kind::Dot && is_name(code[j - 2]) {
        parts.insert(0, bare(code[j - 2]));
        j -= 2;
    }
    if parts.len() == 1 {
        let upper = parts[0].to_ascii_uppercase();
        if let Some(f) = crate::functions::find(&upper) {
            return Some(Signature { name: f.name.clone(), params: f.params.clone(), active: commas, returns: f.returns.clone() });
        }
        if is_reserved(&upper) {
            return None;
        }
    }
    let schema = schema?;
    let o = schema.objects.iter().find(|o| matches!(o.kind.as_str(), "scalar_function" | "table_function") && object_matches(o, &parts, schema))?;
    Some(Signature {
        name: format!("{}.{}", o.schema, o.name),
        params: o.params.iter().map(|p| format!("{} {}", p.name, p.data_type)).collect(),
        active: commas,
        returns: if o.kind == "table_function" { "table".into() } else { String::new() },
    })
}

/// `EXEC name arguments`: the procedure's parameters, and the one the caret is at — by name when
/// the argument names it (`@Id = `), else by position.
fn exec(code: &[&Token], touching: bool, schema: Option<&Schema>) -> Option<Signature> {
    let schema = schema?;
    let start = code.iter().rposition(|t| t.kind == Kind::Word && matches!(t.text.to_ascii_uppercase().as_str(), "EXEC" | "EXECUTE"))?;
    // Only within that statement: no statement keyword after EXEC.
    let mut j = start + 1;
    // `EXEC @result = name`
    if j + 1 < code.len() && code[j].kind == Kind::Variable && code[j + 1].text == "=" {
        j += 2;
    }
    if j >= code.len() || !is_name(code[j]) {
        return None;
    }
    let mut parts = vec![bare(code[j])];
    j += 1;
    while j + 1 < code.len() && code[j].kind == Kind::Dot && is_name(code[j + 1]) {
        parts.push(bare(code[j + 1]));
        j += 2;
    }
    // The caret must be after the name and a space, not still in it.
    if j > code.len() || (j == code.len() && touching) {
        return None;
    }
    let args = &code[j..];
    if args.iter().any(|t| t.kind == Kind::Word && matches!(t.text.to_ascii_uppercase().as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "SET" | "DECLARE" | "IF" | "BEGIN" | "END")) {
        return None;
    }
    let proc = schema.objects.iter().find(|o| o.kind == "procedure" && object_matches(o, &parts, schema))?;
    let commas = args.iter().filter(|t| t.kind == Kind::Comma).count();
    // The argument being written starts after the last comma.
    let current = args.iter().rposition(|t| t.kind == Kind::Comma).map_or(0, |c| c + 1);
    let named = args.get(current).filter(|t| t.kind == Kind::Variable).and_then(|t| proc.params.iter().position(|p| eq(&p.name, &t.text)));
    Some(Signature {
        name: format!("{}.{}", proc.schema, proc.name),
        params: proc.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect(),
        active: named.unwrap_or(commas),
        returns: String::new(),
    })
}
