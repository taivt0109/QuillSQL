//! Long-running mode for the SSMS extension. Requests and replies are single-line JSON with
//! an `id` and an `op`; offsets are UTF-16. `key` names the connection whose metadata applies.
//!
//! - `load` {key, schema} stores metadata; `loaded` {key} says whether it is stored.
//! - `complete` {key, text, caret} returns suggestions.
//! - `tab` {key, text, caret, env, start?, snippet?} expands the snippet or wildcard before the
//!   caret (or the named snippet over `[start, caret)`), or returns null.
//! - `format` {text} formats with the options the server started with.
//! - `refactor` {action, key, text, caret, start, end, name, definition} rewrites the script.
//! - `analyze` {key, text} returns code issues; `fix` {key, text, codes?} applies their fixes.
//! - `hover` {key, text, caret} and `occurrences` {text, caret} describe the name at the caret.
//! - `schema_context` {key, text, budget?} describes the database for an AI prompt.
//! - `plan_target` {key, text} gives the batches, the objects used and why running it could write;
//!   `plan_summary` {plans, maxLines?} turns showplan XML into text.
//! - `exec_target` {text, caret} names the procedure an EXEC at the caret calls.
//! - `snippets` and `rules` list what is available.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use quillsql::complete::{Schema, complete_with, utf16_to_byte};
use quillsql::refactor::{self, Renamed};
use quillsql::snippets::{self, Env, Snippet};
use quillsql::{Options, TableAlias, analysis, format, info, plan};

struct State {
    schemas: HashMap<String, Schema>,
    snippets: Vec<Snippet>,
}

/// Built-in snippets, then the user's; the user's win.
fn load_snippets() -> Vec<Snippet> {
    let mut lists = vec![snippets::builtins()];
    let mut warnings = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        lists.push(snippets::load_dir(&local.join("QuillSQL").join("Snippets"), &mut warnings));
    }
    for w in warnings {
        eprintln!("snippet {w}");
    }
    snippets::merge(lists)
}

fn str_field<'a>(request: &'a Value, name: &str) -> Result<&'a str, String> {
    request[name].as_str().ok_or_else(|| format!("missing {name}"))
}

fn usize_field(request: &Value, name: &str) -> Result<usize, String> {
    request[name].as_u64().map(|v| v as usize).ok_or_else(|| format!("missing {name}"))
}

fn line_indent(text: &str, at: usize) -> String {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// Replacement for `[start, caret)` and the selection to leave, or null.
fn tab(request: &Value, state: &State) -> Result<Value, String> {
    let text = str_field(request, "text")?;
    let caret = usize_field(request, "caret")?;
    let env: Env = serde_json::from_value(request["env"].clone()).unwrap_or_default();
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let caret_byte = utf16_to_byte(text, caret);
    let edit = |start: usize, snippet: &Snippet| {
        let indent = line_indent(text, utf16_to_byte(text, start));
        let e = snippets::expand(snippet, &env, &indent, nl);
        json!({ "start": start, "end": caret, "text": e.text, "selectStart": start + e.select_start, "selectEnd": start + e.select_end })
    };
    if let Some(name) = request["snippet"].as_str() {
        let snippet = snippets::find(&state.snippets, name).ok_or_else(|| format!("no snippet named {name}"))?;
        let start = request["start"].as_u64().map_or(caret, |v| v as usize);
        return Ok(edit(start, snippet));
    }
    let before = &text[..caret_byte];
    let word: String = before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
    if !word.is_empty() {
        let start_byte = caret_byte - word.len();
        let starts_word = before[..start_byte].chars().last().is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '.' | '$' | '[')));
        if let Some(snippet) = snippets::find(&state.snippets, &word).filter(|_| starts_word) {
            return Ok(edit(text[..start_byte].encode_utf16().count(), snippet));
        }
        return Ok(Value::Null);
    }
    if before.trim_end().ends_with('*') {
        let schema = state.schemas.get(request["key"].as_str().unwrap_or_default());
        let star = before.trim_end().len() - 1;
        let star16 = text[..star].encode_utf16().count();
        if let Some((start, end, replacement)) = refactor::wildcard_edit_at(text, schema, star16 + 1) {
            let after = start + replacement.encode_utf16().count();
            return Ok(json!({ "start": start, "end": end, "text": replacement, "selectStart": after, "selectEnd": after }));
        }
    }
    Ok(Value::Null)
}

fn refactor_op(request: &Value, state: &State) -> Result<Value, String> {
    let text = str_field(request, "text")?;
    let schema = state.schemas.get(request["key"].as_str().unwrap_or_default());
    let caret = request["caret"].as_u64().map(|v| v as usize);
    let need_schema = || schema.ok_or_else(|| "connect to a database first: this needs its metadata".to_string());
    match str_field(request, "action")? {
        "expand_wildcards" => refactor::expand_wildcards(text, schema, None).map(|(t, n)| json!({ "text": t, "count": n })),
        "qualify" => refactor::qualify_object_names(text, need_schema()?).map(|(t, n)| json!({ "text": t, "count": n })),
        "add_as" => refactor::table_aliases(text, TableAlias::AddAs).map(|t| json!({ "text": t })),
        "remove_as" => refactor::table_aliases(text, TableAlias::RemoveAs).map(|t| json!({ "text": t })),
        "encapsulate" => {
            let name = request["name"].as_str().unwrap_or("dbo.NewProcedure");
            refactor::encapsulate_as_procedure(text, usize_field(request, "start")?, usize_field(request, "end")?, name).map(|s| json!({ "script": s }))
        }
        "rename" => match refactor::rename(text, caret.ok_or("missing caret")?, str_field(request, "name")?, schema)? {
            Renamed::Text(t, n) => Ok(json!({ "text": t, "count": n })),
            Renamed::Script(s) => Ok(json!({ "script": s })),
        },
        "inline_exec" => refactor::inline_exec(text, caret.ok_or("missing caret")?, str_field(request, "definition")?).map(|t| json!({ "text": t })),
        other => Err(format!("unknown refactoring {other:?}")),
    }
}

fn handle(request: &Value, state: &mut State, opts: &Options) -> Result<Value, String> {
    let key = request["key"].as_str().unwrap_or_default();
    match request["op"].as_str().unwrap_or_default() {
        "load" => {
            let schema: Schema = serde_json::from_value(request["schema"].clone()).map_err(|e| e.to_string())?;
            let objects = schema.objects.len();
            state.schemas.insert(key.to_string(), schema);
            Ok(json!({ "objects": objects }))
        }
        "loaded" => Ok(json!({ "loaded": state.schemas.contains_key(key) })),
        "complete" => {
            let text = str_field(request, "text")?;
            let caret = usize_field(request, "caret")?;
            serde_json::to_value(complete_with(text, caret, state.schemas.get(key), &state.snippets)).map_err(|e| e.to_string())
        }
        "tab" => tab(request, state),
        "format" => format(str_field(request, "text")?, opts).map(|t| json!({ "text": t })).map_err(|e| e.to_string()),
        "refactor" => refactor_op(request, state),
        "analyze" => {
            let issues = analysis::analyze(str_field(request, "text")?, state.schemas.get(key));
            Ok(json!({ "issues": issues }))
        }
        "fix" => {
            let codes: Option<Vec<String>> = request["codes"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect());
            let (text, count) = analysis::apply_fixes(str_field(request, "text")?, state.schemas.get(key), codes.as_deref());
            Ok(json!({ "text": text, "count": count }))
        }
        "hover" => Ok(json!({ "text": info::hover(str_field(request, "text")?, usize_field(request, "caret")?, state.schemas.get(key)) })),
        "occurrences" => Ok(json!({ "ranges": info::occurrences(str_field(request, "text")?, usize_field(request, "caret")?) })),
        "schema_context" => {
            let text = str_field(request, "text")?;
            let budget = request["budget"].as_u64().map_or(40_000, |v| v as usize);
            Ok(json!({ "text": state.schemas.get(key).map(|s| info::schema_context(text, s, budget)) }))
        }
        "plan_target" => {
            let text = str_field(request, "text")?;
            let objects = state.schemas.get(key).map(|s| info::referenced_objects(text, s)).unwrap_or_default();
            Ok(json!({ "batches": plan::batches(text), "objects": objects, "writeReason": plan::write_reason(text) }))
        }
        "plan_summary" => {
            let plans: Vec<String> = request["plans"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
            let lines = request["maxLines"].as_u64().map_or(120, |v| v as usize);
            Ok(json!({ "text": plan::summarize(&plans, lines) }))
        }
        "exec_target" => Ok(json!({ "name": refactor::exec_target(str_field(request, "text")?, usize_field(request, "caret")?) })),
        "snippets" => Ok(json!({ "snippets": state.snippets })),
        "rules" => Ok(json!({ "rules": analysis::RULES.iter().map(|(c, s, t)| json!({ "code": c, "severity": s, "title": t })).collect::<Vec<_>>() })),
        other => Err(format!("unknown op {other:?}")),
    }
}

pub fn run(opts: &Options) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut state = State { schemas: HashMap::new(), snippets: load_snippets() };
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let id = request["id"].clone();
                match handle(&request, &mut state, opts) {
                    Ok(result) => json!({ "id": id, "result": result }),
                    Err(error) => json!({ "id": id, "error": error }),
                }
            }
            Err(e) => json!({ "id": null, "error": format!("invalid request: {e}") }),
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}
