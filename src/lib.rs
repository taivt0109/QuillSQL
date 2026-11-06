//! T-SQL formatter engine.

pub mod analysis;
pub mod args;
pub mod catalog;
pub mod commands;
pub mod compare_serve;
pub mod complete;
pub mod data;
pub mod db;
pub mod info;
pub mod keywords;
pub mod layout;
pub mod lexer;
pub mod options;
pub mod plan;
pub mod project;
pub mod refactor;
pub mod schema;
pub mod snippets;
pub mod script;
pub mod style;
pub mod tools;
pub mod transform;

use lexer::{Token, lex};
pub use options::{Enclose, Options, TableAlias};

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    Lex(lexer::LexError),
    /// The rendered text no longer lexes to the same tokens. Refusing protects the user's script.
    Verification(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Lex(e) => write!(f, "{e}"),
            FormatError::Verification(d) => {
                write!(f, "formatter safety check failed, input left unchanged: {d}")
            }
        }
    }
}

impl std::error::Error for FormatError {}

fn verify(expected: &[Token], rendered: &str) -> Result<(), FormatError> {
    let actual = lex(rendered).map_err(|e| FormatError::Verification(e.to_string()))?.tokens;
    for (i, (a, b)) in expected.iter().zip(&actual).enumerate() {
        if a.kind != b.kind || a.text != b.text {
            return Err(FormatError::Verification(format!(
                "token {i}: expected {:?}, got {:?}",
                a.text, b.text
            )));
        }
    }
    if expected.len() != actual.len() {
        return Err(FormatError::Verification(format!(
            "expected {} tokens, got {}",
            expected.len(),
            actual.len()
        )));
    }
    Ok(())
}

/// Whether a line turns formatting off (`Some(false)`) or back on (`Some(true)`).
fn format_marker(line: &str) -> Option<bool> {
    let t = line.trim().to_ascii_lowercase();
    let body = t.strip_prefix("--")?.trim();
    match body {
        "quill-format-off" => Some(false),
        "quill-format-on" => Some(true),
        _ => None,
    }
}

/// Formats the script. Lines between a format-off and a format-on marker are kept as they are.
pub fn format(src: &str, opts: &Options) -> Result<String, FormatError> {
    if !src.lines().any(|l| format_marker(l).is_some()) {
        return format_region(src, opts);
    }
    let mut out = String::with_capacity(src.len());
    let mut region = String::new();
    let mut on = true;
    for line in src.split_inclusive('\n') {
        match format_marker(line) {
            Some(false) if on => {
                out.push_str(&format_region(&region, opts)?);
                region.clear();
                on = false;
                out.push_str(line);
            }
            Some(true) if !on => {
                on = true;
                out.push_str(line);
            }
            _ if on => region.push_str(line),
            _ => out.push_str(line),
        }
    }
    if !region.is_empty() {
        out.push_str(&format_region(&region, opts)?);
    }
    Ok(out)
}

fn format_region(src: &str, opts: &Options) -> Result<String, FormatError> {
    if src.trim().is_empty() {
        return Ok(src.to_string());
    }
    let lexed = lex(src).map_err(FormatError::Lex)?;
    let mut tokens = lexed.tokens;
    let casing = transform::Casing {
        keywords: opts.effective_keyword_case(),
        functions: opts.function_case,
        data_types: opts.data_type_case,
    };
    if casing.keywords.is_some() || casing.functions.is_some() || casing.data_types.is_some() {
        transform::apply_casing(&mut tokens, &casing);
    }
    if opts.prefix_unicode {
        transform::prefix_unicode(&mut tokens);
    }
    if let Some(style) = opts.enclose_identifiers {
        transform::enclose_identifiers(&mut tokens, style);
    }
    if let Some(style) = opts.table_alias_style {
        transform::table_alias(&mut tokens, style);
    }
    if opts.insert_semicolons {
        transform::insert_semicolons(&mut tokens);
    }
    let output = if opts.layout {
        let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
        layout::render(&tokens, &lexed.trailing_ws, &opts.effective_layout(), nl)
    } else {
        let mut out = String::with_capacity(src.len());
        for t in &tokens {
            out.push_str(&t.ws);
            out.push_str(&t.text);
        }
        out.push_str(&lexed.trailing_ws);
        out
    };
    verify(&tokens, &output)?;
    Ok(output)
}
