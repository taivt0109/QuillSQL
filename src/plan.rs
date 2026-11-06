//! Execution plans for the AI commands: splitting a script into batches, deciding whether it is
//! safe to run for an actual plan, and turning showplan XML into a short text an AI can read.

use roxmltree::{Document, Node};

use crate::complete::Analysis;
use crate::lexer::{Kind, lex_partial};

/// The script's batches, split at `GO`, without the separators.
pub fn batches(text: &str) -> Vec<String> {
    let (tokens, _) = lex_partial(text);
    let mut out = Vec::new();
    let mut current = String::new();
    for t in &tokens {
        if t.kind == Kind::Go {
            out.push(std::mem::take(&mut current));
            continue;
        }
        current.push_str(&t.ws);
        current.push_str(&t.text);
    }
    out.push(current);
    out.into_iter().filter(|b| !b.trim().is_empty()).collect()
}

const WRITES: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "TRUNCATE", "DROP", "CREATE", "ALTER", "EXEC", "EXECUTE", "GRANT", "REVOKE",
    "DENY", "BACKUP", "RESTORE", "DBCC", "KILL", "SHUTDOWN", "RECONFIGURE", "BULK", "OPENROWSET", "OPENQUERY",
    "OPENDATASOURCE", "WAITFOR", "INTO", "WHILE", "GOTO", "COMMIT", "ROLLBACK", "SAVE", "BEGIN", "USE", "ENABLE",
    "DISABLE", "SETUSER", "CHECKPOINT", "RAISERROR", "THROW", "SEND", "RECEIVE",
];

const SAFE_SET_OPTIONS: &[&str] = &[
    "NOCOUNT", "ANSI_NULLS", "ANSI_PADDING", "ANSI_WARNINGS", "ARITHABORT", "CONCAT_NULL_YIELDS_NULL", "QUOTED_IDENTIFIER",
    "NUMERIC_ROUNDABORT", "DATEFORMAT", "DATEFIRST", "LANGUAGE", "TRANSACTION", "DEADLOCK_PRIORITY", "LOCK_TIMEOUT",
];

/// Why running the script to measure it could change something, or None if it only reads.
/// Strict on purpose: anything that is not a query, a variable or a session option is refused.
pub fn write_reason(text: &str) -> Option<String> {
    let a = Analysis::new(text);
    for (i, t) in a.tokens.iter().enumerate() {
        if t.kind != Kind::Word {
            continue;
        }
        let w = a.up[i].as_str();
        let prev = (0..i).rev().find(|&j| !a.tokens[j].is_comment()).map(|j| a.up[j].as_str()).unwrap_or("");
        // `FOR UPDATE` in a cursor and `END` of CASE are not writes; neither is `BEGIN`/`END` of a TRY block's read.
        if w == "UPDATE" && prev == "FOR" {
            continue;
        }
        if WRITES.contains(&w) {
            return Some(format!("it contains {w}"));
        }
        if w == "SET" {
            let next = a.tokens.get(i + 1).map(|n| (n.kind, a.up[i + 1].as_str()));
            let ok = matches!(next, Some((Kind::Variable, _))) || next.is_some_and(|(_, n)| SAFE_SET_OPTIONS.contains(&n) || n.starts_with("ANSI_"));
            if !ok {
                return Some("it changes a session or table setting with SET".into());
            }
        }
        if w == "NEXT" && a.up.get(i + 2).is_some_and(|f| f == "FOR") {
            return Some("it draws from a sequence".into());
        }
    }
    None
}

fn local<'a>(n: &Node<'a, 'a>) -> &'a str {
    n.tag_name().name()
}

fn f(n: &Node, name: &str) -> f64 {
    n.attribute(name).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

/// Element children of `n` that are not inside a nested RelOp.
fn own<'a>(n: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    let mut out = Vec::new();
    let mut stack: Vec<Node> = n.children().filter(|c| c.is_element()).collect();
    while let Some(c) = stack.pop() {
        if local(&c) == "RelOp" {
            continue;
        }
        out.push(c);
        stack.extend(c.children().filter(|x| x.is_element()));
    }
    out
}

/// RelOps directly under this one.
fn children_ops<'a>(n: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    let mut out = Vec::new();
    let mut stack: Vec<Node> = n.children().filter(|c| c.is_element()).collect();
    stack.reverse();
    while let Some(c) = stack.pop() {
        if local(&c) == "RelOp" {
            out.push(c);
            continue;
        }
        let mut kids: Vec<Node> = c.children().filter(|x| x.is_element()).collect();
        kids.reverse();
        stack.extend(kids);
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

fn number(v: f64) -> String {
    if v >= 100.0 || v == v.trunc() { format!("{v:.0}") } else { format!("{v:.1}") }
}

fn warnings(n: Node) -> Vec<String> {
    let mut out = Vec::new();
    for w in n.children().filter(|c| c.is_element()) {
        let detail: Vec<String> = w.attributes().map(|a| format!("{}={}", a.name(), clip(a.value(), 80))).collect();
        let columns: Vec<String> = w.descendants().filter(|d| local(d) == "ColumnReference").filter_map(|d| d.attribute("Column").map(String::from)).collect();
        let mut text = local(&w).to_string();
        if !detail.is_empty() {
            text.push_str(&format!(" ({})", detail.join(", ")));
        }
        if !columns.is_empty() {
            text.push_str(&format!(" columns {}", columns.join(", ")));
        }
        out.push(text);
    }
    out
}

struct Writer {
    lines: Vec<String>,
    max_lines: usize,
}

impl Writer {
    fn push(&mut self, line: String) -> bool {
        if self.lines.len() >= self.max_lines {
            return false;
        }
        self.lines.push(line);
        true
    }
}

fn operator(op: Node, statement_cost: f64, depth: usize, out: &mut Writer) {
    let kids = children_ops(op);
    let subtree = f(&op, "EstimatedTotalSubtreeCost");
    let own_cost = (subtree - kids.iter().map(|k| f(k, "EstimatedTotalSubtreeCost")).sum::<f64>()).max(0.0);
    let percent = if statement_cost > 0.0 { own_cost / statement_cost * 100.0 } else { 0.0 };
    let physical = op.attribute("PhysicalOp").unwrap_or("?");
    let logical = op.attribute("LogicalOp").unwrap_or("");
    let mut line = format!("{}{physical}", "  ".repeat(depth));
    if !logical.is_empty() && logical != physical {
        line.push_str(&format!(" ({logical})"));
    }
    let parts = own(op);
    if let Some(obj) = parts.iter().find(|p| local(p) == "Object") {
        let name: Vec<&str> = ["Schema", "Table", "Index"].iter().filter_map(|a| obj.attribute(*a)).collect();
        line.push_str(&format!(" {}", name.join(".").replace(['[', ']'], "")));
        if let Some(alias) = obj.attribute("Alias") {
            line.push_str(&format!(" AS {}", alias.replace(['[', ']'], "")));
        }
    }
    line.push_str(&format!(" | est rows {}", number(f(&op, "EstimateRows"))));
    let executions = f(&op, "EstimateRebinds") + f(&op, "EstimateRewinds") + 1.0;
    if executions > 1.0 {
        line.push_str(&format!(" x{} executions", number(executions)));
    }
    let counters: Vec<Node> = parts.iter().filter(|p| local(p) == "RunTimeCountersPerThread").copied().collect();
    if !counters.is_empty() {
        let rows: f64 = counters.iter().map(|c| f(c, "ActualRows")).sum();
        let runs: f64 = counters.iter().map(|c| f(c, "ActualExecutions")).sum();
        let reads: f64 = counters.iter().map(|c| f(c, "ActualLogicalReads")).sum();
        let ms = counters.iter().map(|c| f(c, "ActualElapsedms")).fold(0.0, f64::max);
        line.push_str(&format!(" | actual rows {} in {} executions", number(rows), number(runs)));
        if reads > 0.0 {
            line.push_str(&format!(", {} logical reads", number(reads)));
        }
        if ms > 0.0 {
            line.push_str(&format!(", {} ms", number(ms)));
        }
    }
    line.push_str(&format!(" | cost {percent:.0}%"));
    for kind in ["SeekPredicates", "Predicate"] {
        if let Some(p) = parts.iter().find(|p| local(p) == kind)
            && let Some(s) = p.descendants().find_map(|d| d.attribute("ScalarString")) {
                let label = if kind == "SeekPredicates" { "seek" } else { "where" };
                line.push_str(&format!(" | {label} {}", clip(s, 120)));
            }
    }
    if let Some(w) = op.children().find(|c| local(c) == "Warnings") {
        line.push_str(&format!(" | WARNING {}", warnings(w).join("; ")));
    }
    if !out.push(line) {
        return;
    }
    for k in kids {
        operator(k, statement_cost, depth + 1, out);
    }
}

/// A readable summary of showplan XML documents (estimated or actual), at most `max_lines` lines.
pub fn summarize(plans: &[String], max_lines: usize) -> String {
    let mut out = Writer { lines: Vec::new(), max_lines };
    let mut number_of = 0;
    for xml in plans {
        let doc = match Document::parse(xml) {
            Ok(d) => d,
            Err(e) => {
                out.push(format!("(unreadable plan: {e})"));
                continue;
            }
        };
        for stmt in doc.descendants().filter(|n| local(n) == "StmtSimple" || local(n) == "StmtCond") {
            let Some(query) = stmt.children().find(|c| local(c) == "QueryPlan") else { continue };
            number_of += 1;
            let cost = f(&stmt, "StatementSubTreeCost");
            out.push(format!(
                "Statement {number_of}: {} | estimated cost {cost:.4}, est rows {}",
                clip(stmt.attribute("StatementText").unwrap_or(""), 200),
                number(f(&stmt, "StatementEstRows"))
            ));
            let mut facts = Vec::new();
            if let Some(dop) = query.attribute("DegreeOfParallelism")
                && dop != "0" && dop != "1" {
                    facts.push(format!("parallel DOP {dop}"));
                }
            if let Some(grant) = query.children().find(|c| local(c) == "MemoryGrantInfo") {
                if f(&grant, "GrantedMemory") > 0.0 {
                    facts.push(format!("memory grant {} KB, used {} KB", number(f(&grant, "GrantedMemory")), number(f(&grant, "MaxUsedMemory"))));
                } else if f(&grant, "SerialDesiredMemory") > 0.0 {
                    facts.push(format!("wants a memory grant of {} KB", number(f(&grant, "SerialDesiredMemory"))));
                }
            }
            if let Some(reason) = stmt.attribute("StatementOptmEarlyAbortReason") {
                facts.push(format!("optimizer stopped early: {reason}"));
            }
            if let Some(ce) = stmt.attribute("CardinalityEstimationModelVersion") {
                facts.push(format!("CE model {ce}"));
            }
            if !facts.is_empty() {
                out.push(format!("  ({})", facts.join(", ")));
            }
            if let Some(w) = query.children().find(|c| local(c) == "Warnings") {
                out.push(format!("  PLAN WARNING {}", warnings(w).join("; ")));
            }
            if let Some(params) = query.children().find(|c| local(c) == "ParameterList") {
                let values: Vec<String> = params
                    .children()
                    .filter(|c| local(c) == "ColumnReference")
                    .map(|c| {
                        let mut v = format!("{} compiled {}", c.attribute("Column").unwrap_or("?"), c.attribute("ParameterCompiledValue").unwrap_or("?"));
                        if let Some(r) = c.attribute("ParameterRuntimeValue") {
                            v.push_str(&format!(" runtime {r}"));
                        }
                        v
                    })
                    .collect();
                if !values.is_empty() {
                    out.push(format!("  parameters: {}", values.join(", ")));
                }
            }
            for group in query.descendants().filter(|n| local(n) == "MissingIndexGroup") {
                for index in group.children().filter(|c| local(c) == "MissingIndex") {
                    let table = ["Schema", "Table"].iter().filter_map(|a| index.attribute(*a)).collect::<Vec<_>>().join(".").replace(['[', ']'], "");
                    let mut cols = Vec::new();
                    for usage in index.children().filter(|c| local(c) == "ColumnGroup") {
                        let names: Vec<String> = usage.children().filter_map(|c| c.attribute("Name")).map(|n| n.replace(['[', ']'], "")).collect();
                        cols.push(format!("{} ({})", usage.attribute("Usage").unwrap_or("?").to_lowercase(), names.join(", ")));
                    }
                    out.push(format!("  MISSING INDEX suggested by SQL Server, impact {}%: {table} {}", group.attribute("Impact").unwrap_or("?"), cols.join(", ")));
                }
            }
            if let Some(top) = query.children().find(|c| local(c) == "RelOp") {
                operator(top, cost, 1, &mut out);
            }
        }
    }
    if out.lines.len() >= max_lines {
        out.lines.push("(plan truncated)".into());
    }
    if out.lines.is_empty() {
        return "(no plan)".into();
    }
    out.lines.join("\n")
}
