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
//! - `set_options`, `reload_snippets`, `tool`, `outline`, `execution_warnings`, `object_at`,
//!   `table_script` and `to_alter` serve the editor commands of the same names.
//! - `snippets` and `rules` list what is available.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use quillsql::complete::{CompleteOptions, Schema, complete_across, utf16_to_byte};
use quillsql::refactor::{self, Renamed};
use quillsql::snippets::{self, Env, Snippet};
use quillsql::{Options, TableAlias, analysis, format, info, plan, script, style, tools};

struct State {
    schemas: HashMap<String, Schema>,
    snippets: Vec<Snippet>,
    /// Formatting options; `set_options` replaces them while the engine runs.
    options: Options,
}

/// Object kinds whose description is shown next to the suggestion list.
const DESCRIBED: &[&str] = &["table", "view", "procedure", "function", "table_function", "synonym"];

/// Adds an `info` description to the first suggestions that are database objects.
fn with_descriptions(completion: Value, schema: Option<&Schema>) -> Value {
    let Some(schema) = schema else { return completion };
    let mut completion = completion;
    if let Some(items) = completion["items"].as_array_mut() {
        for item in items.iter_mut().take(80) {
            if !DESCRIBED.contains(&item["kind"].as_str().unwrap_or("")) {
                continue;
            }
            let detail = item["detail"].as_str().unwrap_or("");
            let full = detail.split(" (").next().unwrap_or(detail);
            let Some((owner, name)) = full.split_once('.') else { continue };
            if let Some(o) = schema.objects.iter().find(|o| o.schema.eq_ignore_ascii_case(owner) && o.name.eq_ignore_ascii_case(name)) {
                item["info"] = json!(info::describe_object(o));
            }
        }
    }
    completion
}

fn edit_result(result: Result<String, String>) -> Result<Value, String> {
    result.map(|t| json!({ "text": t }))
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

fn tool(request: &Value) -> Result<Value, String> {
    let text = str_field(request, "text")?;
    let caret = request["caret"].as_u64().map_or(0, |v| v as usize);
    let range = |r: Option<(usize, usize)>| Ok(r.map_or(Value::Null, |(start, end)| json!({ "start": start, "end": end })));
    match str_field(request, "action")? {
        "semicolons" => edit_result(tools::semicolons(text)),
        "unformat" => edit_result(tools::unformat(text)),
        "add_brackets" => edit_result(tools::brackets(text, true)),
        "remove_brackets" => edit_result(tools::brackets(text, false)),
        "uppercase" => edit_result(tools::casing(text, true)),
        "lowercase" => edit_result(tools::casing(text, false)),
        "remove_comments" => edit_result(tools::remove_comments(text)),
        "sp_executesql" => edit_result(tools::convert_sp_executesql(text, caret)),
        "statement_range" => range(tools::statement_range(text, caret)),
        "batch_range" => range(tools::batch_range(text, caret)),
        other => Err(format!("unknown tool {other:?}")),
    }
}

fn handle(request: &Value, state: &mut State) -> Result<Value, String> {
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
            let options: CompleteOptions = serde_json::from_value(request["options"].clone()).unwrap_or_default();
            let schema = state.schemas.get(key);
            // Other databases the script names, already loaded under these keys.
            let others: Vec<&Schema> = request["others"].as_array().into_iter().flatten().filter_map(|k| state.schemas.get(k.as_str()?)).collect();
            let completion = serde_json::to_value(complete_across(text, caret, schema, &others, &state.snippets, options)).map_err(|e| e.to_string())?;
            Ok(with_descriptions(completion, schema))
        }
        "tab" => tab(request, state),
        "format" => {
            // `options` previews other settings without changing the engine's own.
            let preview = match request.get("options").filter(|o| o.is_object()) {
                Some(o) => Some(style::options_from_value(o)?.0),
                None => None,
            };
            let mut text = str_field(request, "text")?.to_string();
            let schema = state.schemas.get(key);
            if request["expandWildcards"].as_bool() == Some(true)
                && let Ok((t, _)) = refactor::expand_wildcards(&text, schema, None) {
                    text = t;
                }
            if request["qualifyObjectNames"].as_bool() == Some(true)
                && let Some(Ok((t, _))) = schema.map(|s| refactor::qualify_object_names(&text, s)) {
                    text = t;
                }
            format(&text, preview.as_ref().unwrap_or(&state.options)).map(|t| json!({ "text": t })).map_err(|e| e.to_string())
        }
        "set_options" => {
            let (options, warnings) = style::options_from_value(&request["options"])?;
            state.options = options;
            Ok(json!({ "warnings": warnings }))
        }
        "reload_snippets" => {
            state.snippets = load_snippets();
            Ok(json!({ "count": state.snippets.len() }))
        }
        "tool" => tool(request),
        "outline" => Ok(json!({ "entries": tools::outline(str_field(request, "text")?) })),
        "execution_warnings" => {
            let truncate = request["truncate"].as_bool().unwrap_or(false);
            let drop = request["drop"].as_bool().unwrap_or(false);
            let warnings: Vec<String> = tools::execution_warnings(str_field(request, "text")?)
                .into_iter()
                .filter(|w| (truncate || !w.contains("TRUNCATE")) && (drop || !w.contains("DROP")))
                .collect();
            Ok(json!({ "warnings": warnings }))
        }
        "object_at" => {
            let found = state.schemas.get(key).and_then(|s| info::object_at(str_field(request, "text").ok()?, usize_field(request, "caret").ok()?, s));
            Ok(found.map_or(Value::Null, |o| json!({ "schema": o.schema, "name": o.name, "kind": o.kind })))
        }
        "table_script" => {
            let details: script::TableDetails = serde_json::from_value(request["details"].clone()).map_err(|e| e.to_string())?;
            let nl = request["newline"].as_str().unwrap_or("\r\n");
            Ok(json!({ "text": script::table_script(&details, nl) }))
        }
        "to_alter" => edit_result(script::to_alter(str_field(request, "text")?)),
        "refactor" => refactor_op(request, state),
        "analyze" => {
            let disabled: Vec<String> = request["disabled"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
            let issues: Vec<_> = analysis::analyze(str_field(request, "text")?, state.schemas.get(key)).into_iter().filter(|i| !disabled.iter().any(|d| d == i.code)).collect();
            Ok(json!({ "issues": issues }))
        }
        "fix" => {
            let codes: Option<Vec<String>> = request["codes"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect());
            let (text, count) = analysis::apply_fixes(str_field(request, "text")?, state.schemas.get(key), codes.as_deref());
            Ok(json!({ "text": text, "count": count }))
        }
        "keyword_case" => {
            let found = tools::keyword_before(str_field(request, "text")?, usize_field(request, "caret")?);
            Ok(found.map_or(Value::Null, |(start, end, text)| json!({ "start": start, "end": end, "text": text })))
        }
        "signature" => {
            let text = str_field(request, "text")?;
            let caret = usize_field(request, "caret")?;
            serde_json::to_value(quillsql::signature::signature_at(text, caret, state.schemas.get(key))).map_err(|e| e.to_string())
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
    let mut state = State { schemas: HashMap::new(), snippets: load_snippets(), options: opts.clone() };
    for line in stdin.lock().lines() {
        let line = line?;
        // A .NET client may start the stream with a UTF-8 byte order mark.
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(request) => {
                let id = request["id"].clone();
                match handle(&request, &mut state) {
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
