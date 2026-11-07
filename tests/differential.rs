//! Differential tests: `quill schema` / `quill data` against the real SQLCompare.exe and
//! SQLDataCompare.exe on the same databases. Scenarios are folders under
//! `tests/fixtures/differential/` (see the README there); every observed difference in outcome is
//! either listed in `known.toml` (an expected failure) or fails the test, and a known difference
//! that no longer shows up fails it too, so fixed entries get removed.
//!
//! Runs only when QUILL_TEST_SQLCOMPARE and QUILL_TEST_SQLDATACOMPARE (paths to the executables)
//! and QUILL_TEST_SERVER (with QUILL_TEST_USER / QUILL_TEST_PASSWORD) are set. Databases are named
//! `QD_diff_<pid>_*` and dropped afterwards.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use quillsql::db::{self, Auth, ConnectionSpec};

// ---------------------------------------------------------------------------------------------
// A small TOML subset: `key = value` with strings ("..." or '...'), integers, booleans and
// arrays of strings (may span lines), plus `[[table]]` array headers.

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    List(Vec<String>),
    Bool(bool),
}

type Table = BTreeMap<String, Value>;

#[derive(Default)]
struct Toml {
    top: Table,
    arrays: BTreeMap<String, Vec<Table>>,
}

fn parse_toml(text: &str) -> Result<Toml, String> {
    let mut doc = Toml::default();
    let mut current: Option<String> = None;
    let mut chars = text.chars().peekable();
    let mut line_no = 1;
    let skip_ws = |c: &mut std::iter::Peekable<std::str::Chars>, nl: bool, line_no: &mut usize| {
        loop {
            match c.peek() {
                Some(' ' | '\t' | '\r') => {
                    c.next();
                }
                Some('\n') if nl => {
                    *line_no += 1;
                    c.next();
                }
                Some('#') => {
                    while c.peek().is_some_and(|&x| x != '\n') {
                        c.next();
                    }
                }
                _ => break,
            }
        }
    };
    fn string(c: &mut std::iter::Peekable<std::str::Chars>, line: usize) -> Result<String, String> {
        let quote = c.next().unwrap();
        let mut s = String::new();
        loop {
            match c.next() {
                None | Some('\n') => return Err(format!("line {line}: unterminated string")),
                Some(x) if x == quote => return Ok(s),
                Some('\\') if quote == '"' => match c.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some(x @ ('"' | '\\')) => s.push(x),
                    other => return Err(format!("line {line}: bad escape {other:?}")),
                },
                Some(x) => s.push(x),
            }
        }
    }
    loop {
        skip_ws(&mut chars, true, &mut line_no);
        let Some(&c) = chars.peek() else { break };
        if c == '[' {
            let header: String = chars.by_ref().take_while(|&x| x != '\n').collect();
            line_no += 1;
            let name = header.trim().trim_start_matches("[[").trim_end_matches("]]").trim().to_string();
            if !header.trim().starts_with("[[") || name.is_empty() {
                return Err(format!("line {}: only [[array]] headers are supported", line_no - 1));
            }
            doc.arrays.entry(name.clone()).or_default().push(Table::new());
            current = Some(name);
            continue;
        }
        let key: String = std::iter::from_fn(|| chars.next_if(|&x| x.is_alphanumeric() || x == '_' || x == '-')).collect();
        if key.is_empty() {
            return Err(format!("line {line_no}: expected a key"));
        }
        skip_ws(&mut chars, false, &mut line_no);
        if chars.next() != Some('=') {
            return Err(format!("line {line_no}: expected = after {key}"));
        }
        skip_ws(&mut chars, false, &mut line_no);
        let value = match chars.peek() {
            Some('"' | '\'') => Value::Str(string(&mut chars, line_no)?),
            Some('[') => {
                chars.next();
                let mut items = Vec::new();
                loop {
                    skip_ws(&mut chars, true, &mut line_no);
                    match chars.peek() {
                        Some(']') => {
                            chars.next();
                            break;
                        }
                        Some(',') => {
                            chars.next();
                        }
                        Some('"' | '\'') => items.push(string(&mut chars, line_no)?),
                        other => return Err(format!("line {line_no}: unexpected {other:?} in array")),
                    }
                }
                Value::List(items)
            }
            _ => {
                let word: String = std::iter::from_fn(|| chars.next_if(|&x| !x.is_whitespace() && x != '#')).collect();
                match word.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    w if w.parse::<i64>().is_ok() => Value::Str(w.to_string()),
                    w => return Err(format!("line {line_no}: bad value {w:?}")),
                }
            }
        };
        let table = match &current {
            Some(name) => doc.arrays.get_mut(name).unwrap().last_mut().unwrap(),
            None => &mut doc.top,
        };
        table.insert(key, value);
    }
    Ok(doc)
}

fn get_str(t: &Table, key: &str) -> Option<String> {
    match t.get(key) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_list(t: &Table, key: &str) -> Vec<String> {
    match t.get(key) {
        Some(Value::List(l)) => l.clone(),
        Some(Value::Str(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// Scenarios and known differences.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Schema,
    Data,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Live,
    Scripts,
    Snapshot,
    Backup,
}

struct Scenario {
    name: String,
    dir: PathBuf,
    kind: Kind,
    about: String,
    finding: String,
    source: Source,
    args: Vec<String>,
    judge_args: Option<Vec<String>>,
    project: Option<String>,
    oracles: Option<Vec<String>>,
}

const ORACLES: [&str; 9] = ["converge-quill", "converge-vendor", "outcome", "check", "prints", "diffset", "warnings", "exit", "names"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("differential")
}

fn load_scenarios() -> Result<Vec<Scenario>, String> {
    let mut out = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(fixtures()).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(dir.join("scenario.toml")).map_err(|e| format!("{name}: {e}"))?;
        let t = parse_toml(&text).map_err(|e| format!("{name}/scenario.toml: {e}"))?.top;
        let kind = match get_str(&t, "kind").as_deref() {
            Some("schema") => Kind::Schema,
            Some("data") => Kind::Data,
            k => return Err(format!("{name}: kind must be schema or data, not {k:?}")),
        };
        let source = match get_str(&t, "source").as_deref() {
            None | Some("live") => Source::Live,
            Some("scripts") => Source::Scripts,
            Some("snapshot") => Source::Snapshot,
            Some("backup") => Source::Backup,
            Some(s) => return Err(format!("{name}: unknown source {s}")),
        };
        let oracles = t.contains_key("oracles").then(|| get_list(&t, "oracles"));
        for o in oracles.iter().flatten() {
            if !ORACLES.contains(&o.as_str()) {
                return Err(format!("{name}: unknown oracle {o}"));
            }
        }
        out.push(Scenario {
            kind,
            about: get_str(&t, "about").unwrap_or_default(),
            finding: get_str(&t, "finding").unwrap_or_default(),
            source,
            args: get_list(&t, "args"),
            judge_args: t.contains_key("judge_args").then(|| get_list(&t, "judge_args")),
            project: get_str(&t, "project"),
            oracles,
            name,
            dir,
        });
    }
    Ok(out)
}

struct Known {
    scenario: String,
    oracle: String,
    reason: String,
    finding: String,
}

fn load_known() -> Result<Vec<Known>, String> {
    let text = std::fs::read_to_string(fixtures().join("known.toml")).map_err(|e| e.to_string())?;
    let doc = parse_toml(&text).map_err(|e| format!("known.toml: {e}"))?;
    let mut out = Vec::new();
    for t in doc.arrays.get("known").into_iter().flatten() {
        let (Some(scenario), Some(oracle)) = (get_str(t, "scenario"), get_str(t, "oracle")) else {
            return Err("known.toml: every entry needs scenario and oracle".into());
        };
        if !ORACLES.contains(&oracle.as_str()) && oracle != "setup" {
            return Err(format!("known.toml: unknown oracle {oracle}"));
        }
        out.push(Known { scenario, oracle, reason: get_str(t, "reason").unwrap_or_default(), finding: get_str(t, "finding").unwrap_or_default() });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The environment: server and tools.

struct Env {
    host: String,
    user: Option<String>,
    password: String,
    sqlcompare: PathBuf,
    sqldatacompare: PathBuf,
    container: String,
    work: PathBuf,
}

fn env() -> Option<Env> {
    let host = std::env::var("QUILL_TEST_SERVER").ok()?;
    let sqlcompare = PathBuf::from(std::env::var("QUILL_TEST_SQLCOMPARE").ok()?);
    let sqldatacompare = PathBuf::from(std::env::var("QUILL_TEST_SQLDATACOMPARE").ok()?);
    Some(Env {
        host,
        user: std::env::var("QUILL_TEST_USER").ok(),
        password: std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default(),
        sqlcompare,
        sqldatacompare,
        container: std::env::var("QUILL_TEST_CONTAINER").unwrap_or_else(|_| "quill-mssql-2022".into()),
        work: std::env::temp_dir().join(format!("quill-diff-{}", std::process::id())),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Quill,
    Vendor,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Tool::Quill => "quill",
            Tool::Vendor => "vendor",
        }
    }
}

#[derive(Clone, Debug)]
enum Side {
    Live(String),
    Scripts(PathBuf),
    Snapshot(PathBuf),
    Backup(PathBuf),
}

struct Run {
    code: i32,
    out: String,
}

impl Env {
    fn spec(&self, database: &str) -> ConnectionSpec {
        let auth = match &self.user {
            Some(user) => Auth::Sql { user: user.clone(), password: self.password.clone() },
            None => Auth::Windows,
        };
        let mut spec = ConnectionSpec::new(&self.host, database, auth);
        spec.trust_server_certificate = true;
        spec
    }

    /// Connects, retrying when the machine has briefly run out of sockets.
    async fn connect(&self, database: &str) -> Result<db::Connection, String> {
        let mut tries = 0;
        loop {
            match db::connect_async(&self.spec(database)).await {
                Err(e) if tries < 5 && transient(&e) => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                r => return r,
            }
        }
    }

    fn sql(&self, database: &str, script: &str) -> Result<(), String> {
        let rt = db::runtime()?;
        rt.block_on(async {
            let mut c = self.connect(database).await?;
            for batch in quillsql::schema::deploy::split_batches(script) {
                db::execute(&mut c, &batch).await.map_err(|e| format!("{e}\n--- in batch:\n{batch}"))?;
            }
            Ok(())
        })
    }

    /// Creates the database from a script; `{db}` in the script is its name.
    fn create(&self, database: &str, script: &str) -> Result<(), String> {
        self.drop_db(database);
        self.sql("master", &format!("CREATE DATABASE {}", db::quote(database)))?;
        self.sql(database, &script.replace("{db}", database))
    }

    /// Every result set of a script, one line per row.
    fn rows(&self, database: &str, script: &str) -> Result<Vec<String>, String> {
        let rt = db::runtime()?;
        rt.block_on(async {
            let mut c = self.connect(database).await?;
            let mut out = Vec::new();
            for batch in quillsql::schema::deploy::split_batches(script) {
                for set in db::query_sets(&mut c, &batch).await? {
                    for row in set {
                        out.push(row.cells().map(|(col, v)| format!("{}={v:?}", col.name())).collect::<Vec<_>>().join(", "));
                    }
                    out.push("--".into());
                }
            }
            Ok(out)
        })
    }

    fn drop_db(&self, database: &str) {
        let q = db::quote(database);
        for _ in 0..3 {
            let sql = format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END", db::literal(database));
            if self.sql("master", &sql).is_ok() {
                break;
            }
        }
    }

    /// Drops the logins scenarios created (their names end with the process id).
    fn drop_logins(&self) {
        let sql = format!(
            "DECLARE @s nvarchar(max) = N''; SELECT @s += N'DROP LOGIN ' + QUOTENAME(name) + N'; ' FROM sys.server_principals WHERE name LIKE N'QD\\_diff\\_%\\_{}' ESCAPE N'\\'; EXEC (@s);",
            std::process::id()
        );
        let _ = self.sql("master", &sql);
    }

    /// The same switches for both tools: no encryption switch, as SQL Compare takes none.
    fn side_args(&self, n: u8, side: &Side, _tool: Tool) -> Vec<String> {
        match side {
            Side::Live(database) => {
                let mut a = vec![format!("/server{n}:{}", self.host), format!("/database{n}:{database}")];
                if let Some(user) = &self.user {
                    a.push(format!("/username{n}:{user}"));
                    a.push(format!("/password{n}:{}", self.password));
                }
                a
            }
            Side::Scripts(p) => vec![format!("/scripts{n}:{}", p.display())],
            Side::Snapshot(p) => vec![format!("/snapshot{n}:{}", p.display())],
            Side::Backup(p) => vec![format!("/backup{n}:{}", p.display())],
        }
    }

    fn exe(&self, tool: Tool, kind: Kind) -> (PathBuf, Option<&'static str>) {
        match (tool, kind) {
            (Tool::Quill, Kind::Schema) => (PathBuf::from(env!("CARGO_BIN_EXE_quill")), Some("schema")),
            (Tool::Quill, Kind::Data) => (PathBuf::from(env!("CARGO_BIN_EXE_quill")), Some("data")),
            (Tool::Vendor, Kind::Schema) => (self.sqlcompare.clone(), None),
            (Tool::Vendor, Kind::Data) => (self.sqldatacompare.clone(), None),
        }
    }

    /// Runs a tool, again when it failed for a reason that has nothing to do with the scenario
    /// (no free sockets, a licence check that crashed under load).
    fn run(&self, tool: Tool, kind: Kind, args: &[String], dir: &Path, step: &str) -> Run {
        let mut tries = 0;
        loop {
            let r = self.run_once(tool, kind, args, dir, step);
            if r.code == 0 || r.code == 63 || tries == 3 || !transient(&r.out) {
                return r;
            }
            tries += 1;
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Runs a tool with a time limit; stdout and stderr are kept together, the command and its
    /// output are appended to `log` with the password masked.
    fn run_once(&self, tool: Tool, kind: Kind, args: &[String], dir: &Path, step: &str) -> Run {
        let (exe, sub) = self.exe(tool, kind);
        let out_path = dir.join(format!("{step}.out"));
        let file = std::fs::File::create(&out_path).unwrap();
        let mut cmd = Command::new(&exe);
        cmd.args(sub).args(args).current_dir(dir).stdin(Stdio::null()).stdout(file.try_clone().unwrap()).stderr(file);
        let code = match cmd.spawn() {
            Err(e) => {
                std::fs::write(&out_path, format!("could not start {}: {e}", exe.display())).unwrap();
                -1
            }
            Ok(mut child) => {
                let start = Instant::now();
                loop {
                    if let Some(s) = child.try_wait().unwrap() {
                        break s.code().unwrap_or(-1);
                    }
                    if start.elapsed() > Duration::from_secs(600) {
                        let _ = child.kill();
                        let _ = child.wait();
                        break -2;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        let out = String::from_utf8_lossy(&std::fs::read(&out_path).unwrap_or_default()).replace("\r\n", "\n");
        let mask = |s: &str| if self.password.is_empty() { s.to_string() } else { s.replace(&self.password, "***") };
        let line = format!("## {step}: {} {} {}\n# exit {code}\n{}\n", tool.label(), sub.unwrap_or(""), mask(&args.join(" ")), mask(&out));
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")).unwrap();
        let _ = log.write_all(line.as_bytes());
        // The masked copy is all that stays on disk.
        std::fs::write(&out_path, mask(&out)).unwrap();
        Run { code, out }
    }

    /// Backs the database up inside the container and copies the file out.
    fn backup(&self, database: &str, to: &Path) -> Result<(), String> {
        let file = format!("/var/opt/mssql/data/{database}.bak");
        self.sql("master", &format!("BACKUP DATABASE {} TO DISK = N'{file}' WITH INIT, FORMAT, COPY_ONLY", db::quote(database)))?;
        let ok = Command::new("docker").args(["cp", &format!("{}:{file}", self.container)]).arg(to).status().is_ok_and(|s| s.success());
        let _ = Command::new("docker").args(["exec", &self.container, "rm", "-f", &file]).status();
        if ok { Ok(()) } else { Err(format!("docker cp of {file} failed")) }
    }
}

/// Failures of the machine rather than of a tool: socket exhaustion, the Vendor licence check.
fn transient(text: &str) -> bool {
    ["os error 10048", "os error 10055", "Vendor.Licensing", "could not reach"].iter().any(|m| text.contains(m))
}

// ---------------------------------------------------------------------------------------------
// Reading the tools' output.

/// A schema XML report: (object type, name without brackets, status) and the raw fqn values.
fn read_report(path: &Path) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let bytes = std::fs::read(path).ok()?;
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_string()
    };
    let text = text.replacen("encoding=\"utf-16\"", "encoding=\"utf-8\"", 1);
    let doc = roxmltree::Document::parse(&text).ok()?;
    let mut set = BTreeSet::new();
    let mut fqns = BTreeSet::new();
    for d in doc.descendants().filter(|n| n.has_tag_name("difference")) {
        let ty = d.attribute("objecttype").unwrap_or_default();
        let status = d.attribute("status").unwrap_or_default();
        if status == "equal" || status == "identical" {
            continue;
        }
        let fqn = d.attribute("fqn").unwrap_or_default();
        let name = fqn.split_once('-').map(|(_, n)| n).unwrap_or(fqn);
        set.insert(format!("{ty} {} {status}", unbracket(name)));
        fqns.insert(format!("{fqn} {status}"));
    }
    Some((set, fqns))
}

fn unbracket(name: &str) -> String {
    let mut out = String::new();
    let mut chars = name.chars().peekable();
    let mut inside = false;
    while let Some(c) = chars.next() {
        match c {
            '[' if !inside => inside = true,
            ']' if inside && chars.peek() == Some(&']') => {
                chars.next();
                out.push(']');
            }
            ']' if inside => inside = false,
            _ => out.push(c),
        }
    }
    out
}

/// Data console summary: "table status" -> records, e.g. "dbo.T different" -> 3.
fn read_data_summary(out: &str) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    let mut in_table = false;
    let mut dashes = 0;
    for line in out.lines() {
        if line.starts_with("Object type") && line.contains("Records") {
            in_table = true;
            dashes = 0;
            continue;
        }
        if !in_table {
            continue;
        }
        if line.starts_with("-----") {
            dashes += 1;
            if dashes == 2 {
                in_table = false;
            }
            continue;
        }
        let t = line.trim_end();
        let (status, rest) = if let Some(r) = t.strip_suffix("<>  <>") {
            ("different", r)
        } else if let Some(r) = t.strip_suffix("==  ==") {
            ("identical", r)
        } else if let Some(r) = t.strip_suffix(">>") {
            ("only1", r)
        } else if let Some(r) = t.strip_suffix("<<") {
            ("only2", r)
        } else {
            continue;
        };
        let rest = rest.trim_end();
        let Some((head, count)) = rest.rsplit_once(' ') else { continue };
        let Ok(count) = count.parse::<u64>() else { continue };
        let head = head.trim();
        let name = head.split_once(char::is_whitespace).map(|(_, n)| n.trim()).unwrap_or(head);
        *map.entry(format!("{} {status}", unbracket(name))).or_default() += count;
    }
    map
}

/// The summary lines (objects with differences) of a compare run, for failure details.
fn summary_lines(out: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut dashes = 0;
    let mut on = false;
    for line in out.lines() {
        if line.starts_with("Object type") {
            on = true;
            dashes = 0;
            continue;
        }
        if on {
            if line.starts_with("-----") {
                dashes += 1;
                if dashes == 2 {
                    on = false;
                }
                continue;
            }
            if !line.trim().is_empty() {
                v.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
    }
    v
}

/// Errors printed by a run (lines from "Error:" to the next blank line), for failure details.
fn error_text(out: &str) -> String {
    let mut v = Vec::new();
    let mut on = false;
    for line in out.lines() {
        if line.starts_with("Error") || line.contains("error occurred") || line.starts_with("Msg ") {
            on = true;
        }
        if on {
            if line.trim().is_empty() {
                on = false;
                continue;
            }
            v.push(line.trim());
        }
    }
    let s = v.join(" ");
    if s.len() > 600 { format!("{}...", &s[..s.char_indices().nth(600).map(|(i, _)| i).unwrap_or(s.len())]) } else { s }
}

/// Warnings as kinds: severity and text with identifiers masked and product names unified.
fn read_warnings(out: &str) -> BTreeSet<String> {
    let mut items: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    for line in out.lines() {
        let t = line.trim();
        if t.starts_with("Warning:") || t.starts_with("Warning (") {
            if let Some(c) = cur.take() {
                items.push(c);
            }
            cur = Some(t.to_string());
        } else if let Some(c) = cur.as_mut() {
            if t.is_empty() || t.starts_with("=====") || t.starts_with("-----") || t.starts_with("Summary Information") || t.starts_with("Object type") {
                items.push(cur.take().unwrap());
            } else {
                c.push(' ');
                c.push_str(t);
            }
        }
    }
    items.extend(cur);
    let ident = regex::Regex::new(r"(\[[^\]]*\](\.\[[^\]]*\])*)|('[^']*')").unwrap();
    let prefix = regex::Regex::new(r"^Warning:\s*(\((High|Medium|Low)\)\s*)?((\[[^\]]*\]|[^\s:\[]+)(\.(\[[^\]]*\]|[^\s:\[]+))*:\s+)?").unwrap();
    let product = regex::Regex::new(r"QuillSQL Data Compare|QuillSQL Compare|SQL Data Compare|SQL Compare|QuillSQL").unwrap();
    items
        .into_iter()
        .map(|w| {
            let w = w.split_whitespace().collect::<Vec<_>>().join(" ");
            let sev = prefix.captures(&w).and_then(|c| c.get(2)).map(|m| m.as_str().to_string()).unwrap_or_else(|| "-".into());
            let body = prefix.replace(&w, "");
            let body = product.replace_all(&body, "<product>");
            format!("({sev}) {}", ident.replace_all(&body, "<id>"))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Running one scenario.

type Outcome = BTreeMap<String, Result<String, String>>;

fn set_diff<T: Ord + std::fmt::Display>(quill: &BTreeSet<T>, vendor: &BTreeSet<T>) -> Option<String> {
    let only_q: Vec<String> = quill.difference(vendor).map(|x| x.to_string()).collect();
    let only_r: Vec<String> = vendor.difference(quill).map(|x| x.to_string()).collect();
    if only_q.is_empty() && only_r.is_empty() {
        return None;
    }
    let mut s = String::new();
    if !only_q.is_empty() {
        s += &format!("only quill: {}", only_q.join("; "));
    }
    if !only_r.is_empty() {
        if !s.is_empty() {
            s += " | ";
        }
        s += &format!("only vendor: {}", only_r.join("; "));
    }
    Some(s)
}

/// Switches a judging run must not repeat: identical rows/objects would make every run succeed.
fn judge_filter(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| !a.eq_ignore_ascii_case("/include:Identical")).cloned().collect()
}

/// Only the options switches, for comparing the two deployed copies with each other.
fn options_only(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| a.to_ascii_lowercase().starts_with("/options:") || a.to_ascii_lowercase().starts_with("/o:")).cloned().collect()
}

fn ok_sync(code: i32) -> bool {
    code == 0 || code == 63
}

fn db_name(scenario: &str, suffix: &str) -> String {
    let short: String = scenario.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).take(60).collect();
    format!("QD_diff_{}_{short}_{suffix}", std::process::id())
}

fn substitute(text: &str, vars: &[(&str, String)]) -> String {
    let mut t = text.to_string();
    for (k, v) in vars {
        t = t.replace(&format!("{{{k}}}"), v);
    }
    t
}

fn run_scenario(env: &Env, sc: &Scenario) -> Outcome {
    let mut outcome = Outcome::new();
    let dir = env.work.join(&sc.name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pid = std::process::id().to_string();
    let dbs: BTreeMap<&str, String> = ["1", "2", "q", "r"].into_iter().map(|s| (s, db_name(&sc.name, s))).collect();
    let mut vars: Vec<(&str, String)> = vec![("pid", pid.clone()), ("dir", dir.display().to_string()), ("fixture", sc.dir.display().to_string())];
    let read = |file: &str| std::fs::read_to_string(sc.dir.join(file)).ok().map(|t| substitute(&t, &vars));
    let (server_sql, db1_sql, db2_sql) = (read("server.sql"), read("db1.sql").unwrap_or_default(), read("db2.sql").unwrap_or_default());
    let setup = (|| -> Result<Side, String> {
        if let Some(s) = &server_sql {
            env.sql("master", s).map_err(|e| format!("server.sql: {e}"))?;
        }
        env.create(&dbs["1"], &db1_sql).map_err(|e| format!("db1.sql: {e}"))?;
        for s in ["2", "q", "r"] {
            env.create(&dbs[s], &db2_sql).map_err(|e| format!("db2.sql: {e}"))?;
        }
        let live1 = Side::Live(dbs["1"].clone());
        Ok(match sc.source {
            Source::Live => live1,
            Source::Scripts => {
                let folder = dir.join("scripts1");
                let mut a = env.side_args(1, &live1, Tool::Vendor);
                a.push(format!("/makescripts:{}", folder.display()));
                let r = env.run(Tool::Vendor, Kind::Schema, &a, &dir, "makescripts");
                if r.code != 0 {
                    return Err(format!("SQLCompare /makescripts exit {}: {}", r.code, error_text(&r.out)));
                }
                if sc.kind == Kind::Data || sc.args.iter().any(|a| a.eq_ignore_ascii_case("/include:StaticData")) {
                    let mut a = env.side_args(1, &live1, Tool::Vendor);
                    a.push(format!("/scripts2:{}", folder.display()));
                    a.push("/synchronize".into());
                    let r = env.run(Tool::Vendor, Kind::Data, &a, &dir, "makedata");
                    if !ok_sync(r.code) {
                        return Err(format!("SQLDataCompare data into scripts folder exit {}: {}", r.code, error_text(&r.out)));
                    }
                }
                Side::Scripts(folder)
            }
            Source::Snapshot => {
                let file = dir.join("db1.snp");
                let mut a = env.side_args(1, &live1, Tool::Vendor);
                a.push(format!("/makesnapshot:{}", file.display()));
                let r = env.run(Tool::Vendor, Kind::Schema, &a, &dir, "makesnapshot");
                if r.code != 0 {
                    return Err(format!("SQLCompare /makesnapshot exit {}: {}", r.code, error_text(&r.out)));
                }
                Side::Snapshot(file)
            }
            Source::Backup => {
                let file = dir.join("db1.bak");
                env.backup(&dbs["1"], &file)?;
                Side::Backup(file)
            }
        })
    })();
    let side1 = match setup {
        Ok(s) => s,
        Err(e) => {
            outcome.insert("setup".into(), Err(e));
            return outcome;
        }
    };
    let source1 = match &side1 {
        Side::Live(d) => d.clone(),
        Side::Scripts(p) | Side::Snapshot(p) | Side::Backup(p) => p.display().to_string(),
    };
    vars.push(("server", env.host.clone()));
    vars.push(("user", env.user.clone().unwrap_or_default()));
    vars.push(("db1", dbs["1"].clone()));
    vars.push(("source1", source1));

    // Command lines: the sides come from a project (one copy per target) or from switches.
    let project_for = |target: &str| -> Option<PathBuf> {
        let file = sc.project.as_ref()?;
        let mut v = vars.clone();
        v.push(("db2", target.to_string()));
        let text = substitute(&std::fs::read_to_string(sc.dir.join(file)).unwrap(), &v);
        let ext = Path::new(file).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let path = dir.join(format!("{target}.{ext}"));
        std::fs::write(&path, text).unwrap();
        Some(path)
    };
    let sides = |tool: Tool, s1: &Side, target: &str| -> Vec<String> {
        match project_for(target) {
            Some(p) => {
                let mut a = vec![format!("/project:{}", p.display())];
                if env.user.is_some() {
                    if matches!(s1, Side::Live(_)) {
                        a.push(format!("/password1:{}", env.password));
                    }
                    a.push(format!("/password2:{}", env.password));
                }
                a
            }
            None => {
                let mut a = env.side_args(1, s1, tool);
                a.extend(env.side_args(2, &Side::Live(target.to_string()), tool));
                a
            }
        }
    };
    let args: Vec<String> = sc.args.iter().map(|a| substitute(a, &vars)).collect();
    let judge_args: Vec<String> = sc.judge_args.as_ref().map(|j| j.iter().map(|a| substitute(a, &vars)).collect()).unwrap_or_else(|| judge_filter(&args));
    let width = "/outputwidth:250".to_string();
    let tools = [Tool::Quill, Tool::Vendor];

    // 1. The scenario's own command line, with a report (schema) and warnings.
    let mut main = BTreeMap::new();
    let mut reports = BTreeMap::new();
    let mut ident = BTreeMap::new();
    for tool in tools {
        let l = tool.label();
        let mut a = sides(tool, &side1, &dbs["2"]);
        a.extend(args.iter().cloned());
        a.push(width.clone());
        a.push("/showWarnings".into());
        a.push(format!("/scriptfile:{}", dir.join(format!("{l}-compare.sql")).display()));
        if sc.kind == Kind::Schema {
            a.push(format!("/report:{}", dir.join(format!("{l}.xml")).display()));
            a.push("/reporttype:Xml".into());
        }
        main.insert(l, env.run(tool, sc.kind, &a, &dir, &format!("{l}-compare")));
        if sc.kind == Kind::Schema {
            reports.insert(l, read_report(&dir.join(format!("{l}.xml"))));
        } else {
            let mut a = sides(tool, &side1, &dbs["2"]);
            a.extend(args.iter().filter(|x| !x.eq_ignore_ascii_case("/include:Identical")).cloned());
            a.push("/include:Identical".into());
            a.push(width.clone());
            ident.insert(l, env.run(tool, sc.kind, &a, &dir, &format!("{l}-identical")));
        }
    }

    // 2. Each tool deploys to its own copy of database 2.
    let mut sync = BTreeMap::new();
    for (tool, target) in [(Tool::Quill, &dbs["q"]), (Tool::Vendor, &dbs["r"])] {
        let l = tool.label();
        let mut a = sides(tool, &side1, target);
        a.extend(args.iter().cloned());
        a.push(width.clone());
        a.push("/synchronize".into());
        a.push(format!("/scriptfile:{}", dir.join(format!("{l}-sync.sql")).display()));
        sync.insert(l, env.run(tool, sc.kind, &a, &dir, &format!("{l}-sync")));
    }

    // 3. Judging: each result checked by the other tool, then the two results with each other.
    let identical = |r: &Run| if sc.kind == Kind::Schema { r.code == 0 } else { r.code == 63 };
    let judge = |tool: Tool, s1: &Side, target: &str, extra: &[String], step: &str| -> Run {
        let mut a = sides(tool, s1, target);
        a.extend(extra.iter().cloned());
        a.push(width.clone());
        if sc.kind == Kind::Schema {
            a.push("/assertidentical".into());
        }
        env.run(tool, sc.kind, &a, &dir, step)
    };
    let wants = |o: &str| sc.oracles.as_ref().is_none_or(|l| l.iter().any(|x| x == o));
    let (sq, sr) = (&sync["quill"], &sync["vendor"]);
    let both_failed = !ok_sync(sq.code) && !ok_sync(sr.code) && sq.code == sr.code;
    for (name, tool, target, judge_tool) in [("converge-quill", Tool::Quill, &dbs["q"], Tool::Vendor), ("converge-vendor", Tool::Vendor, &dbs["r"], Tool::Quill)] {
        if !wants(name) {
            continue;
        }
        let s = &sync[tool.label()];
        let res = if both_failed {
            Ok(format!("both deployments failed with exit {}", s.code))
        } else if !ok_sync(s.code) {
            Err(format!("{} deployment failed with exit {}: {}", tool.label(), s.code, error_text(&s.out)))
        } else {
            let j = judge(judge_tool, &side1, target, &judge_args, &format!("judge-{}-by-{}", tool.label(), judge_tool.label()));
            if identical(&j) {
                Ok(String::new())
            } else {
                let lines = summary_lines(&j.out);
                Err(format!("{} judges the result not identical (exit {}): {}", judge_tool.label(), j.code, if lines.is_empty() { error_text(&j.out) } else { lines.join("; ") }))
            }
        };
        outcome.insert(name.into(), res);
    }
    if wants("outcome") {
        let res = if both_failed {
            Ok(format!("both deployments failed with exit {}", sq.code))
        } else if ok_sync(sq.code) != ok_sync(sr.code) {
            Err(format!("deployment exit codes differ: quill {}, vendor {}", sq.code, sr.code))
        } else {
            // The two copies themselves, never through the scenario's project (it names database 1).
            let mut a = env.side_args(1, &Side::Live(dbs["q"].clone()), Tool::Vendor);
            a.extend(env.side_args(2, &Side::Live(dbs["r"].clone()), Tool::Vendor));
            a.extend(options_only(&args));
            a.push(width.clone());
            if sc.kind == Kind::Schema {
                a.push("/assertidentical".into());
            }
            let j = env.run(Tool::Vendor, sc.kind, &a, &dir, "judge-q-vs-r");
            if identical(&j) {
                Ok(String::new())
            } else {
                let lines = summary_lines(&j.out);
                Err(format!("the two deployed copies differ (DB1 = quill's, DB2 = vendor's): {}", if lines.is_empty() { error_text(&j.out) } else { lines.join("; ") }))
            }
        };
        outcome.insert("outcome".into(), res);
    }

    // A query run on both deployed copies must give the same rows.
    if let Some(check) = std::fs::read_to_string(sc.dir.join("check.sql")).ok().filter(|_| wants("check")) {
        let res = match (env.rows(&dbs["q"], &check), env.rows(&dbs["r"], &check)) {
            (Ok(q), Ok(r)) if q == r => Ok(String::new()),
            (Ok(q), Ok(r)) => Err(format!("check.sql gives quill's copy [{}] and vendor's copy [{}]", q.join("; "), r.join("; "))),
            (q, r) => Err(format!("check.sql failed: quill's copy {:?}, vendor's copy {:?}", q.err(), r.err())),
        };
        outcome.insert("check".into(), res);
    }

    // The progress messages of the two data scripts, in order (their statements may differ in form).
    if sc.kind == Kind::Data && !both_failed && (wants("prints") || wants("outcome")) {
        let prints = |l: &str| -> Vec<String> {
            let text = std::fs::read_to_string(dir.join(format!("{l}-sync.sql"))).unwrap_or_default();
            text.lines().map(str::trim).filter(|x| x.starts_with("PRINT(")).map(str::to_string).collect()
        };
        let (q, r) = (prints("quill"), prints("vendor"));
        let res = if q == r { Ok(String::new()) } else { Err(format!("quill prints [{}]; vendor prints [{}]", q.join(" "), r.join(" "))) };
        outcome.insert("prints".into(), res);
    }

    // 4. Difference sets, warnings, exit codes.
    let (mq, mr) = (&main["quill"], &main["vendor"]);
    if wants("diffset") {
        let res = match sc.kind {
            Kind::Schema => match (&reports["quill"], &reports["vendor"]) {
                (Some((q, _)), Some((r, _))) => set_diff(q, r).map_or(Ok(String::new()), Err),
                (None, None) => Ok("no report from either tool".into()),
                (q, _) => Err(format!("only {} wrote a report", if q.is_some() { "quill" } else { "vendor" })),
            },
            Kind::Data => {
                let fmt = |m: BTreeMap<String, u64>| -> BTreeSet<String> { m.into_iter().map(|(k, v)| format!("{k}={v}")).collect() };
                let q = fmt(read_data_summary(&ident["quill"].out));
                let r = fmt(read_data_summary(&ident["vendor"].out));
                set_diff(&q, &r).map_or(Ok(String::new()), Err)
            }
        };
        outcome.insert("diffset".into(), res);
    }
    if wants("names")
        && sc.kind == Kind::Schema
        && let (Some((_, q)), Some((_, r))) = (&reports["quill"], &reports["vendor"])
    {
        outcome.insert("names".into(), set_diff(q, r).map_or(Ok(String::new()), Err));
    }
    if wants("warnings") {
        let res = set_diff(&read_warnings(&mq.out), &read_warnings(&mr.out)).map_or(Ok(String::new()), Err);
        outcome.insert("warnings".into(), res);
    }
    if wants("exit") {
        let res = if mq.code == mr.code {
            Ok(String::new())
        } else {
            let detail = |r: &Run| {
                let e = error_text(&r.out);
                if e.is_empty() { String::new() } else { format!(" ({e})") }
            };
            Err(format!("quill exit {}{}, vendor exit {}{}", mq.code, detail(mq), mr.code, detail(mr)))
        };
        outcome.insert("exit".into(), res);
    }
    outcome
}

// ---------------------------------------------------------------------------------------------
// The test.

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
enum Verdict {
    New,
    UnexpectedPass,
    Known,
    Pass,
}

#[test]
fn differential_against_vendor() {
    let Some(env) = env() else {
        eprintln!("differential: skipped (set QUILL_TEST_SQLCOMPARE, QUILL_TEST_SQLDATACOMPARE and QUILL_TEST_SERVER)");
        return;
    };
    let scenarios = load_scenarios().unwrap();
    let known = load_known().unwrap();
    for k in &known {
        assert!(k.scenario == "*" || scenarios.iter().any(|s| s.name == k.scenario), "known.toml names a missing scenario: {}", k.scenario);
    }
    let only: Vec<String> = std::env::var("QUILL_DIFF_ONLY").map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let chosen: Vec<&Scenario> = scenarios.iter().filter(|s| only.is_empty() || only.iter().any(|o| s.name.contains(o.as_str()))).collect();
    let jobs: usize = std::env::var("QUILL_DIFF_JOBS").ok().and_then(|j| j.parse().ok()).unwrap_or(4).max(1);
    std::fs::create_dir_all(&env.work).unwrap();

    // The tools must run at all (licence, path) before anything is judged.
    for (tool, kind) in [(Tool::Vendor, Kind::Schema), (Tool::Vendor, Kind::Data)] {
        let r = env.run(tool, kind, &["/?".to_string()], &env.work, &format!("probe-{kind:?}"));
        assert!(r.code == 0, "{:?} /? exits {}: {}", env.exe(tool, kind).0, r.code, r.out);
    }

    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..jobs.min(chosen.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(sc) = chosen.get(i) else { break };
                    let started = Instant::now();
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_scenario(&env, sc)))
                        .unwrap_or_else(|_| Outcome::from([("setup".to_string(), Err("the harness panicked".to_string()))]));
                    if std::env::var_os("QUILL_DIFF_KEEP").is_none() {
                        for suffix in ["1", "2", "q", "r"] {
                            env.drop_db(&db_name(&sc.name, suffix));
                        }
                    }
                    eprintln!("differential: {} done in {:.0}s", sc.name, started.elapsed().as_secs_f64());
                    results.lock().unwrap().push((i, outcome));
                }
            });
        }
    });
    env.drop_logins();
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0);

    // Verdicts.
    let mut rows: Vec<(Verdict, &Scenario, String, String)> = Vec::new();
    let mut wildcard_hit = BTreeSet::new();
    for (i, outcome) in &results {
        let sc = chosen[*i];
        for (oracle, res) in outcome {
            let k = known.iter().position(|k| k.scenario == sc.name && &k.oracle == oracle);
            let w = known.iter().position(|k| k.scenario == "*" && &k.oracle == oracle);
            let (verdict, detail) = match (res, k, w) {
                (Ok(note), None, _) => (Verdict::Pass, note.clone()),
                (Ok(note), Some(k), _) => (Verdict::UnexpectedPass, format!("listed in known.toml ({}) but passed {note}", known[k].reason)),
                (Err(e), Some(_), _) => (Verdict::Known, e.clone()),
                (Err(e), None, Some(w)) => {
                    wildcard_hit.insert(w);
                    (Verdict::Known, e.clone())
                }
                (Err(e), None, None) => (Verdict::New, e.clone()),
            };
            rows.push((verdict, sc, oracle.clone(), detail));
        }
    }
    let mut report = String::from("# Differential results: QuillSQL vs SQL Compare / SQL Data Compare\n\n");
    let count = |v: Verdict| rows.iter().filter(|r| r.0 == v).count();
    report += &format!(
        "Scenarios run: {}. Checks: {} pass, {} known difference, {} new difference, {} unexpected pass.\n\n",
        results.len(),
        count(Verdict::Pass),
        count(Verdict::Known),
        count(Verdict::New),
        count(Verdict::UnexpectedPass)
    );
    report += "| Scenario | Oracle | Verdict | Finding | Observed |\n|---|---|---|---|---|\n";
    let mut sorted: Vec<_> = rows.iter().filter(|r| r.0 != Verdict::Pass).collect();
    sorted.sort_by(|a, b| (a.0, &a.1.name, &a.2).cmp(&(b.0, &b.1.name, &b.2)));
    for (v, sc, oracle, detail) in &sorted {
        let finding = known.iter().find(|k| (k.scenario == sc.name || k.scenario == "*") && &k.oracle == oracle).map(|k| k.finding.clone()).filter(|f| !f.is_empty()).unwrap_or_else(|| sc.finding.clone());
        report += &format!("| {} | {} | {:?} | {} | {} |\n", sc.name, oracle, v, finding, detail.replace('|', "\\|").replace('\n', " "));
    }
    report += "\n## Scenarios\n\n";
    for (i, outcome) in &results {
        let sc = chosen[*i];
        let pass: Vec<&str> = outcome.iter().filter(|(_, r)| r.is_ok()).map(|(o, _)| o.as_str()).collect();
        report += &format!("- **{}** ({:?}, {:?}): {}. Passing: {}.\n", sc.name, sc.kind, sc.source, sc.about, if pass.is_empty() { "none".into() } else { pass.join(", ") });
    }
    let path = std::env::var("QUILL_DIFF_REPORT").map(PathBuf::from).unwrap_or_else(|_| env.work.join("differential-results.md"));
    std::fs::write(&path, &report).unwrap();
    eprintln!("differential: results in {}", path.display());

    let mut problems: Vec<String> = rows
        .iter()
        .filter(|r| matches!(r.0, Verdict::New | Verdict::UnexpectedPass))
        .map(|(v, sc, o, d)| format!("{:?} {} / {}: {}", v, sc.name, o, d))
        .collect();
    if only.is_empty() {
        for (i, k) in known.iter().enumerate() {
            if k.scenario == "*" && !wildcard_hit.contains(&i) {
                problems.push(format!("UnexpectedPass * / {}: no scenario shows it any more ({})", k.oracle, k.reason));
            }
        }
    }
    assert!(problems.is_empty(), "{} differential problem(s); see {}\n{}", problems.len(), path.display(), problems.join("\n"));
}

#[test]
fn toml_subset_parses() {
    let t = parse_toml("a = \"x\\\"y\" # c\nb = ['p\\q', \"r\",\n  \"s\"]\nc = true\n[[known]]\nscenario = 'x'\n[[known]]\nscenario = \"y\"\n").unwrap();
    assert_eq!(t.top["a"], Value::Str("x\"y".into()));
    assert_eq!(t.top["b"], Value::List(vec!["p\\q".into(), "r".into(), "s".into()]));
    assert_eq!(t.top["c"], Value::Bool(true));
    assert_eq!(t.arrays["known"].len(), 2);
    assert!(parse_toml("x = [1]").is_err());
}

#[test]
fn output_readers() {
    let out = "Object type     Name                Records      DB1 DB2 \n------\nTable           [dbo].[T]                 3  <>  <>  \nTable           [dbo].[T]                 1  >>      \nTable           [dbo].[U]                 2      <<  \nTable           [dbo].[U]                 5  ==  ==  \n------\n";
    let m = read_data_summary(out);
    assert_eq!(m["dbo.T different"], 3);
    assert_eq!(m["dbo.T only1"], 1);
    assert_eq!(m["dbo.U only2"], 2);
    assert_eq!(m["dbo.U identical"], 5);
    let w = read_warnings("Warnings\n====\nWarning: (High) This deployment adds the column [W] to table\n[dbo].[T] as NOT NULL.\n\nWarning: (High) [dbo].[T]: This deployment adds the column [W] to table [dbo].[T] as NOT NULL.\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(unbracket("[dbo].[a]]b]"), "dbo.a]b");
    // Every scenario folder parses and names known oracles.
    load_scenarios().unwrap();
    load_known().unwrap();
}
