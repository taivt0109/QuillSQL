//! Reads CREATE scripts back into the schema model: the statements a scripts folder holds
//! (CREATE TABLE, ALTER TABLE ... ADD CONSTRAINT, CREATE INDEX, modules, GRANT, extended
//! properties...), interpreted the way SQL Server would store them.
//!
//! Statements are collected from every file first and applied in phases (objects, then what
//! hangs on them, then permissions and properties), so the order of files does not matter.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::catalog::*;
use crate::schema::ObjectType;
use crate::schema::text::{Tok, tokenize};

/// Settings a scripts folder gives every object (RedGateDatabaseInfo.xml).
#[derive(Clone, Debug)]
pub struct Defaults {
    pub collation: String,
    pub schema: String,
    pub filegroup: String,
    pub ansi_nulls: bool,
    pub quoted_identifier: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            collation: "SQL_Latin1_General_CP1_CI_AS".into(),
            schema: "dbo".into(),
            filegroup: "PRIMARY".into(),
            ansi_nulls: true,
            quoted_identifier: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    InvalidSyntax,
    IgnoredStatement,
    IfTakenAsTrue,
    ReferenceNotFound,
}

/// Something the reader skipped or could not understand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub kind: MessageKind,
    /// The token or statement involved.
    pub detail: String,
}

impl Message {
    /// The warning SQL Compare prints for it.
    pub fn text(&self) -> String {
        let f = self.file.display();
        match self.kind {
            MessageKind::InvalidSyntax => format!("Invalid syntax found in file {f} at line {}", self.line),
            MessageKind::IgnoredStatement => format!("Ignored statement found in file {f} at line {}", self.line),
            MessageKind::IfTakenAsTrue => format!("If statement in file {f} at line {} considered as true", self.line),
            MessageKind::ReferenceNotFound => format!("Reference in file {f} at line {} not found", self.line),
        }
    }
}

/// What a scripts-folder file holds: a compared object, or a database DDL trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Object(ObjectType),
    DdlTrigger,
}

/// Identifies a top-level object for [`Origins`].
pub type ObjectKey = (FileKind, String, String);

/// The file each object's CREATE statement came from.
pub type Origins = HashMap<ObjectKey, PathBuf>;

/// One statement waiting for its phase.
struct Pending {
    phase: u8,
    file: usize,
    /// Line of the statement's first character in the file, from 1.
    line: usize,
    sql: String,
    settings: ModuleSettings,
    /// The whole batch is a module (CREATE VIEW / PROCEDURE / FUNCTION / TRIGGER).
    module: bool,
}

/// Collects files, then [`Reader::finish`] builds the database.
pub struct Reader {
    defaults: Defaults,
    files: Vec<PathBuf>,
    pending: Vec<Pending>,
    messages: Vec<Message>,
}

/// The database read from scripts, with what was skipped and where each object came from.
pub struct ReadResult {
    pub database: Database,
    pub messages: Vec<Message>,
    pub origins: Origins,
}

impl Reader {
    pub fn new(defaults: Defaults) -> Reader {
        Reader { defaults, files: Vec::new(), pending: Vec::new(), messages: Vec::new() }
    }

    /// Splits one file into batches and statements.
    pub fn add_file(&mut self, path: &Path, text: &str) {
        let file = self.files.len();
        self.files.push(path.to_path_buf());
        let mut settings = ModuleSettings { ansi_nulls: self.defaults.ansi_nulls, quoted_identifier: self.defaults.quoted_identifier };
        for (batch, first_line) in batches(text) {
            let cur = Cur::new(&batch);
            if cur.t.is_empty() {
                continue;
            }
            if let Some(start) = module_start(&cur) {
                let tok = &cur.t[start];
                let kind = cur.t.get(module_kind_index(&cur, start)).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
                let phase = if kind == "TRIGGER" { 2 } else { 1 };
                self.pending.push(Pending {
                    phase,
                    file,
                    line: first_line + tok.line - 1,
                    sql: module_text(&batch),
                    settings: settings.clone(),
                    module: true,
                });
                continue;
            }
            for (a, b) in statements(&cur) {
                let sql = cur.text(a, b).to_string();
                let line = first_line + cur.t[a].line - 1;
                let first = cur.t[a].text.to_ascii_uppercase();
                if first == "SET" {
                    apply_set(&cur, a, b, &mut settings);
                    continue;
                }
                if first == "IF" {
                    self.messages.push(Message { file: path.into(), line, column: cur.t[a].col, kind: MessageKind::IfTakenAsTrue, detail: sql.clone() });
                    if let Some(body) = if_body(&cur, a, b) {
                        let sql = cur.text(body, b).to_string();
                        let phase = phase_of(&Cur::new(&sql));
                        self.pending.push(Pending { phase, file, line: first_line + cur.t[body].line - 1, sql, settings: settings.clone(), module: false });
                    }
                    continue;
                }
                let phase = phase_of(&Cur::new(&sql));
                if phase == IGNORE {
                    if !QUIET.contains(&first.as_str()) {
                        self.messages.push(Message { file: path.into(), line, column: cur.t[a].col, kind: MessageKind::IgnoredStatement, detail: sql });
                    }
                    continue;
                }
                self.pending.push(Pending { phase, file, line, sql, settings: settings.clone(), module: false });
            }
        }
    }

    pub fn finish(mut self, name: &str) -> ReadResult {
        let mut b = Builder {
            db: Database {
                model_version: MODEL_VERSION,
                name: name.to_string(),
                collation: self.defaults.collation.clone(),
                ..Default::default()
            },
            defaults: self.defaults.clone(),
            origins: Origins::new(),
            file: PathBuf::new(),
            counter: 0,
        };
        b.db.schemas.push(Schema { name: "dbo".into(), owner: "dbo".into() });
        let mut pending = std::mem::take(&mut self.pending);
        pending.sort_by_key(|p| p.phase);
        for p in pending {
            b.file = self.files[p.file].clone();
            let result = if p.module { b.module(&p.sql, &p.settings) } else { b.statement(&p.sql) };
            if let Err(e) = result {
                let (line, column) = (p.line + e.line.saturating_sub(1), e.column);
                self.messages.push(Message { file: b.file.clone(), line, column, kind: e.kind, detail: e.text });
            }
        }
        b.finalize();
        self.messages.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        ReadResult { database: b.db, messages: self.messages, origins: b.origins }
    }
}

/// A module's stored text from its batch. SQL Compare writes the text and then a line break
/// before `GO` unless the text already ends with one; that line break is not part of the text.
fn module_text(batch: &str) -> String {
    let start = batch.len() - batch.trim_start_matches(['\r', '\n']).len();
    let text = &batch[start..];
    match text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')) {
        Some(rest) if rest.ends_with('\n') => rest.to_string(),
        _ => text.to_string(),
    }
}

/// Splits a file on `GO` lines; each batch comes with the line it starts on.
pub fn batches(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start = 1;
    // GO inside a block comment or a string does not end a batch.
    let mut in_comment = 0usize;
    let mut in_string = false;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let is_go = !in_string && in_comment == 0 && {
            let mut w = trimmed.split_whitespace();
            w.next().is_some_and(|g| g.eq_ignore_ascii_case("GO") || g.eq_ignore_ascii_case("GO;"))
                && w.next().is_none_or(|x| x.starts_with("--") || x.chars().all(|c| c.is_ascii_digit()))
        };
        if is_go {
            out.push((std::mem::take(&mut cur), start));
            start = n + 2;
            continue;
        }
        track_state(line, &mut in_comment, &mut in_string);
        cur.push_str(line);
    }
    if !cur.trim().is_empty() {
        out.push((cur, start));
    }
    out
}

fn track_state(line: &str, depth: &mut usize, in_string: &mut bool) {
    let c: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if *in_string {
            if c[i] == '\'' {
                if c.get(i + 1) == Some(&'\'') {
                    i += 1;
                } else {
                    *in_string = false;
                }
            }
        } else if *depth > 0 {
            if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                *depth -= 1;
                i += 1;
            } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                *depth += 1;
                i += 1;
            }
        } else if c[i] == '-' && c.get(i + 1) == Some(&'-') {
            return;
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            *depth += 1;
            i += 1;
        } else if c[i] == '\'' {
            *in_string = true;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// Tokens

#[derive(Clone, Debug)]
struct Tk {
    kind: Tok,
    text: String,
    start: usize,
    end: usize,
    line: usize,
    col: usize,
}

/// A cursor over the significant tokens of one piece of SQL.
struct Cur<'a> {
    src: &'a str,
    t: Vec<Tk>,
    i: usize,
}

/// Where a statement went wrong.
struct Fail {
    kind: MessageKind,
    line: usize,
    column: usize,
    text: String,
}

type R<T> = Result<T, Fail>;

impl<'a> Cur<'a> {
    fn new(src: &'a str) -> Cur<'a> {
        let mut t = Vec::new();
        let (mut pos, mut line, mut col) = (0, 1, 1);
        for tok in tokenize(src) {
            let len = tok.text.len();
            if !matches!(tok.kind, Tok::Space | Tok::LineComment | Tok::BlockComment) {
                t.push(Tk { kind: tok.kind, text: tok.text.clone(), start: pos, end: pos + len, line, col });
            }
            for ch in tok.text.chars() {
                if ch == '\n' {
                    line += 1;
                    col = 1;
                } else {
                    col += 1;
                }
            }
            pos += len;
        }
        Cur { src, t, i: 0 }
    }

    fn done(&self) -> bool {
        self.i >= self.t.len()
    }

    fn peek(&self) -> Option<&Tk> {
        self.t.get(self.i)
    }

    fn word_at(&self, k: usize, w: &str) -> bool {
        self.t.get(k).is_some_and(|t| t.kind == Tok::Word && t.text.eq_ignore_ascii_case(w))
    }

    fn at(&self, w: &str) -> bool {
        self.word_at(self.i, w)
    }

    fn at_seq(&self, ws: &[&str]) -> bool {
        ws.iter().enumerate().all(|(k, w)| self.word_at(self.i + k, w))
    }

    fn eat(&mut self, w: &str) -> bool {
        if self.at(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_seq(&mut self, ws: &[&str]) -> bool {
        if self.at_seq(ws) {
            self.i += ws.len();
            true
        } else {
            false
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        self.peek().is_some_and(|t| t.kind == Tok::Punct && t.text == p)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn fail<T>(&self, kind: MessageKind) -> R<T> {
        let t = self.peek().or(self.t.last());
        Err(Fail {
            kind,
            line: t.map(|t| t.line).unwrap_or(1),
            column: t.map(|t| t.col).unwrap_or(1),
            text: t.map(|t| t.text.clone()).unwrap_or_default(),
        })
    }

    fn syntax<T>(&self) -> R<T> {
        self.fail(MessageKind::InvalidSyntax)
    }

    fn expect(&mut self, w: &str) -> R<()> {
        if self.eat(w) { Ok(()) } else { self.syntax() }
    }

    fn expect_punct(&mut self, p: &str) -> R<()> {
        if self.eat_punct(p) { Ok(()) } else { self.syntax() }
    }

    /// An identifier: a word, `[quoted]` or `"quoted"`.
    fn ident(&mut self) -> R<String> {
        match self.peek() {
            Some(t) if matches!(t.kind, Tok::Word | Tok::Quoted) => {
                let s = unquote(t);
                self.i += 1;
                Ok(s)
            }
            _ => self.syntax(),
        }
    }

    /// An identifier or a string literal (sp_ arguments).
    fn ident_or_string(&mut self) -> R<String> {
        if let Some(t) = self.peek()
            && t.kind == Tok::Str
        {
            let s = string_value(&t.text);
            self.i += 1;
            return Ok(s);
        }
        self.ident()
    }

    /// `a.b.c`, parts unquoted.
    fn multipart(&mut self) -> R<Vec<String>> {
        let mut parts = vec![self.ident()?];
        while self.is_punct(".") {
            self.i += 1;
            // `a..b` skips the schema.
            if self.is_punct(".") {
                parts.push(String::new());
                continue;
            }
            parts.push(self.ident()?);
        }
        Ok(parts)
    }

    /// A schema-qualified object name.
    fn object(&mut self, default_schema: &str) -> R<ObjectName> {
        let parts = self.multipart()?;
        Ok(match parts.as_slice() {
            [name] => ObjectName::new(default_schema, name),
            [.., schema, name] => ObjectName::new(if schema.is_empty() { default_schema } else { schema }, name),
            [] => return self.syntax(),
        })
    }

    /// Index of the `)` matching the `(` at `open`.
    fn close_of(&self, open: usize) -> Option<usize> {
        let mut depth = 0;
        for k in open..self.t.len() {
            let t = &self.t[k];
            if t.kind == Tok::Punct {
                if t.text == "(" {
                    depth += 1;
                } else if t.text == ")" {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
            }
        }
        None
    }

    /// At `(`: the token range inside the parentheses; moves past `)`.
    fn group(&mut self) -> R<(usize, usize)> {
        if !self.is_punct("(") {
            return self.syntax();
        }
        let Some(close) = self.close_of(self.i) else { return self.syntax() };
        let inner = (self.i + 1, close);
        self.i = close + 1;
        Ok(inner)
    }

    /// The source text from token `a` to token `b` (inclusive).
    fn text(&self, a: usize, b: usize) -> &'a str {
        if a > b || b >= self.t.len() {
            return "";
        }
        &self.src[self.t[a].start..self.t[b].end]
    }

    /// Splits tokens `a..b` (exclusive end) on commas at depth 0.
    fn split_commas(&self, a: usize, b: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut start = a;
        for k in a..b {
            let t = &self.t[k];
            if t.kind == Tok::Punct {
                match t.text.as_str() {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    "," if depth == 0 => {
                        out.push((start, k));
                        start = k + 1;
                    }
                    _ => {}
                }
            }
        }
        if start < b {
            out.push((start, b));
        }
        out
    }

    /// A sub-cursor over tokens `a..b`.
    fn sub(&self, a: usize, b: usize) -> Cur<'a> {
        Cur { src: self.src, t: self.t[a..b].to_vec(), i: 0 }
    }

    /// A number, with an optional sign.
    fn number(&mut self) -> R<String> {
        let mut s = String::new();
        if self.is_punct("-") || self.is_punct("+") {
            if self.is_punct("-") {
                s.push('-');
            }
            self.i += 1;
        }
        match self.peek() {
            Some(t) if t.kind == Tok::Word && t.text.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                s.push_str(&t.text);
                self.i += 1;
                Ok(s)
            }
            _ => self.syntax(),
        }
    }

    fn on_off(&mut self) -> R<bool> {
        if self.eat("ON") {
            Ok(true)
        } else if self.eat("OFF") {
            Ok(false)
        } else {
            self.syntax()
        }
    }
}

fn unquote(t: &Tk) -> String {
    match t.kind {
        Tok::Quoted => {
            let inner = &t.text[1..t.text.len().saturating_sub(1)];
            if t.text.starts_with('[') { inner.replace("]]", "]") } else { inner.replace("\"\"", "\"") }
        }
        _ => t.text.clone(),
    }
}

/// `N'it''s'` → `it's`.
fn string_value(s: &str) -> String {
    let s = s.strip_prefix(['N', 'n']).unwrap_or(s);
    let inner = s.strip_prefix('\'').unwrap_or(s);
    let inner = inner.strip_suffix('\'').unwrap_or(inner);
    inner.replace("''", "'")
}

// ---------------------------------------------------------------------------------------------
// Batches and statements

const IGNORE: u8 = 9;

/// Statements that are skipped without a warning.
const QUIET: &[&str] = &["BEGIN", "COMMIT", "ROLLBACK", "SET"];

/// The first token of a module batch (CREATE VIEW / PROCEDURE / FUNCTION / TRIGGER).
fn module_start(c: &Cur) -> Option<usize> {
    if !c.word_at(0, "CREATE") && !c.word_at(0, "ALTER") {
        return None;
    }
    let k = module_kind_index(c, 0);
    let kind = c.t.get(k)?.text.to_ascii_uppercase();
    matches!(kind.as_str(), "VIEW" | "PROC" | "PROCEDURE" | "FUNCTION" | "TRIGGER").then_some(0)
}

/// Index of the object-kind word after CREATE [OR ALTER].
fn module_kind_index(c: &Cur, start: usize) -> usize {
    if c.word_at(start + 1, "OR") && c.word_at(start + 2, "ALTER") { start + 3 } else { start + 1 }
}

/// Words that begin a new statement inside a batch.
fn starts_statement(c: &Cur, k: usize) -> bool {
    let t = &c.t[k];
    if t.kind != Tok::Word {
        return false;
    }
    let prev = k.checked_sub(1).map(|p| c.t[p].text.to_ascii_uppercase()).unwrap_or_default();
    let next = c.t.get(k + 1).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
    match t.text.to_ascii_uppercase().as_str() {
        "CREATE" => !matches!(prev.as_str(), "GRANT" | "DENY" | "REVOKE" | ","),
        "DENY" | "REVOKE" | "PRINT" | "USE" | "DECLARE" | "IF" => true,
        "GRANT" => prev != "WITH",
        "EXEC" | "EXECUTE" => prev != "WITH" && prev != "GRANT" && prev != "," && prev != "DENY" && prev != "REVOKE" && next != "ON" && next != "AS",
        "ALTER" => prev != "GRANT" && prev != "," && prev != "DENY" && prev != "REVOKE" && matches!(next.as_str(), "TABLE" | "ROLE" | "USER" | "INDEX" | "SCHEMA" | "AUTHORIZATION" | "DATABASE" | "SEQUENCE"),
        "SET" => !matches!(prev.as_str(), "DELETE" | "UPDATE") && next != "NULL" && next != "DEFAULT" && next != "(",
        "DROP" => matches!(next.as_str(), "TABLE" | "VIEW" | "PROCEDURE" | "PROC" | "FUNCTION" | "INDEX" | "SYNONYM" | "TYPE" | "SEQUENCE" | "SCHEMA" | "ROLE" | "USER" | "TRIGGER") && prev != "ADD",
        "DISABLE" | "ENABLE" => next == "TRIGGER" && (c.word_at(k + 3, "ON") || c.word_at(k + 5, "ON")),
        _ => false,
    }
}

/// Statement ranges (inclusive token indexes) of a batch.
fn statements(c: &Cur) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    let mut k = 0;
    while k < c.t.len() {
        let t = &c.t[k];
        if t.kind == Tok::Punct && t.text == ";" && depth == 0 {
            if let Some(s) = start.take() {
                out.push((s, k - 1));
            }
            k += 1;
            continue;
        }
        if depth == 0 && start.is_some() && starts_statement(c, k) {
            out.push((start.unwrap(), k - 1));
            start = Some(k);
        }
        if start.is_none() {
            start = Some(k);
        }
        if t.kind == Tok::Punct {
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                depth -= 1;
            }
        }
        // IF <condition> <statement>: the condition is not split off.
        if t.text.eq_ignore_ascii_case("IF") && depth == 0 && start == Some(k) {
            k = skip_condition(c, k + 1);
            continue;
        }
        k += 1;
    }
    if let Some(s) = start {
        out.push((s, c.t.len() - 1));
    }
    out
}

/// Past an IF condition: the next statement keyword at depth 0.
fn skip_condition(c: &Cur, mut k: usize) -> usize {
    let mut depth = 0;
    while k < c.t.len() {
        let t = &c.t[k];
        if t.kind == Tok::Punct && t.text == "(" {
            depth += 1;
        } else if t.kind == Tok::Punct && t.text == ")" {
            depth -= 1;
        } else if depth == 0 && t.kind == Tok::Word && is_body_start(&t.text) {
            return k;
        }
        k += 1;
    }
    k
}

fn is_body_start(w: &str) -> bool {
    ["CREATE", "ALTER", "EXEC", "EXECUTE", "GRANT", "DENY", "BEGIN", "DROP", "PRINT"].iter().any(|x| w.eq_ignore_ascii_case(x))
}

/// The statement an `IF <condition>` guards, as a token index.
fn if_body(c: &Cur, a: usize, b: usize) -> Option<usize> {
    let mut k = skip_condition(c, a + 1);
    if c.word_at(k, "BEGIN") {
        k += 1;
    }
    (k <= b && !c.word_at(k, "PRINT")).then_some(k)
}

fn apply_set(c: &Cur, a: usize, b: usize, settings: &mut ModuleSettings) {
    let on = c.word_at(b, "ON");
    for k in a + 1..b {
        if c.word_at(k, "ANSI_NULLS") {
            settings.ansi_nulls = on;
        } else if c.word_at(k, "QUOTED_IDENTIFIER") {
            settings.quoted_identifier = on;
        }
    }
}

/// When a statement is applied: 0 schemas, types and principals, 1 tables and modules,
/// 2 what hangs on tables, 3 security and properties.
fn phase_of(c: &Cur) -> u8 {
    let w = |k: usize| c.t.get(k).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
    match (w(0).as_str(), w(1).as_str()) {
        ("CREATE", "SCHEMA" | "TYPE" | "SEQUENCE" | "ROLE" | "USER") => 0,
        ("CREATE", "TABLE" | "SYNONYM") => 1,
        ("CREATE", _) => 2,
        ("ALTER", "TABLE" | "INDEX") => 2,
        ("DISABLE" | "ENABLE", "TRIGGER") => 2,
        ("ALTER", "ROLE" | "USER" | "AUTHORIZATION" | "SCHEMA") => 3,
        ("GRANT" | "DENY" | "REVOKE", _) => 3,
        ("EXEC" | "EXECUTE", _) => 3,
        _ => IGNORE,
    }
}

// ---------------------------------------------------------------------------------------------
// Building the model

struct Builder {
    db: Database,
    defaults: Defaults,
    origins: Origins,
    file: PathBuf,
    /// For generated constraint names.
    counter: u32,
}

/// Which object a statement was about, for where its sub-objects go.
#[derive(Clone, Copy)]
enum Parent {
    Table(usize),
    View(usize),
}

const SYSTEM_TYPES: &[&str] = &[
    "bigint", "int", "smallint", "tinyint", "bit", "decimal", "numeric", "money", "smallmoney", "float", "real", "date",
    "datetime", "datetime2", "datetimeoffset", "smalldatetime", "time", "char", "varchar", "nchar", "nvarchar", "text",
    "ntext", "binary", "varbinary", "image", "uniqueidentifier", "xml", "sql_variant", "timestamp", "rowversion",
    "hierarchyid", "geometry", "geography", "sysname", "json", "vector",
];

fn is_character(t: &DataType) -> bool {
    t.schema.is_none() && matches!(t.name.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname")
}

/// Generated constraint names look like `PK__Customer__3214EC07019F7773`.
pub fn looks_system_named(name: &str) -> bool {
    let Some((_, tail)) = name.rsplit_once("__") else { return false };
    let prefix = ["PK__", "UQ__", "DF__", "CK__", "FK__"].iter().any(|p| name.starts_with(p));
    prefix && tail.len() >= 8 && tail.chars().all(|c| c.is_ascii_hexdigit())
}

impl Builder {
    fn schema_default(&self) -> String {
        self.defaults.schema.clone()
    }

    fn origin(&mut self, ty: ObjectType, schema: &str, name: &str) {
        self.origin_of(FileKind::Object(ty), schema, name);
    }

    fn origin_of(&mut self, kind: FileKind, schema: &str, name: &str) {
        self.origins.entry((kind, schema.to_string(), name.to_string())).or_insert_with(|| self.file.clone());
    }

    fn table_index(&self, n: &ObjectName) -> Option<usize> {
        self.db.tables.iter().position(|t| t.schema.eq_ignore_ascii_case(&n.schema) && t.name.eq_ignore_ascii_case(&n.name))
    }

    fn view_index(&self, n: &ObjectName) -> Option<usize> {
        self.db.views.iter().position(|t| t.schema.eq_ignore_ascii_case(&n.schema) && t.name.eq_ignore_ascii_case(&n.name))
    }

    fn parent(&self, c: &Cur, n: &ObjectName) -> R<Parent> {
        if let Some(t) = self.table_index(n) {
            return Ok(Parent::Table(t));
        }
        if let Some(v) = self.view_index(n) {
            return Ok(Parent::View(v));
        }
        c.fail(MessageKind::ReferenceNotFound)
    }

    fn generated(&mut self, prefix: &str, table: &str, column: Option<&str>) -> String {
        self.counter += 1;
        let short = |s: &str| s.chars().take(9).collect::<String>();
        match column {
            Some(col) => format!("{prefix}__{}__{}__{:08X}", short(table), short(col), 0x5E00_0000u32 + self.counter),
            None => format!("{prefix}__{}__{:016X}", short(table), 0x3214_EC07_0000_0000u64 + u64::from(self.counter)),
        }
    }

    fn statement(&mut self, sql: &str) -> R<()> {
        let mut c = Cur::new(sql);
        let w0 = c.peek().map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
        let w1 = c.t.get(1).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
        match (w0.as_str(), w1.as_str()) {
            ("CREATE", "SCHEMA") => self.create_schema(&mut c),
            ("CREATE", "TABLE") => self.create_table(&mut c),
            ("CREATE", "TYPE") => self.create_type(&mut c),
            ("CREATE", "SEQUENCE") => self.create_sequence(&mut c),
            ("CREATE", "SYNONYM") => self.create_synonym(&mut c),
            ("CREATE", "ROLE") => self.create_role(&mut c),
            ("CREATE", "USER") => self.create_user(&mut c),
            ("CREATE", _) if is_index_statement(&c) => self.create_index(&mut c),
            ("ALTER", "TABLE") => self.alter_table(&mut c),
            ("ALTER", "INDEX") => self.alter_index(&mut c),
            ("ALTER", "ROLE") => self.alter_role(&mut c),
            ("ALTER", "AUTHORIZATION") => self.alter_authorization(&mut c),
            ("DISABLE" | "ENABLE", "TRIGGER") => self.enable_trigger(&mut c),
            ("GRANT" | "DENY" | "REVOKE", _) => self.permission(&mut c),
            ("EXEC" | "EXECUTE", _) => self.exec(&mut c),
            _ => c.fail(MessageKind::IgnoredStatement),
        }
    }

    fn create_schema(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let owner = if c.eat("AUTHORIZATION") { c.ident()? } else { "dbo".to_string() };
        match self.db.schemas.iter_mut().find(|s| s.name.eq_ignore_ascii_case(&name)) {
            Some(s) => s.owner = owner,
            None => self.db.schemas.push(Schema { name: name.clone(), owner }),
        }
        self.origin(ObjectType::Schema, "", &name);
        Ok(())
    }

    fn create_role(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let owner = if c.eat("AUTHORIZATION") { c.ident()? } else { "dbo".to_string() };
        self.db.principals.retain(|p| !p.name.eq_ignore_ascii_case(&name));
        self.db.principals.push(Principal { name: name.clone(), kind: "DATABASE_ROLE".into(), owner: Some(owner), ..Default::default() });
        self.origin(ObjectType::Role, "", &name);
        Ok(())
    }

    fn create_user(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let mut p = Principal { name: name.clone(), kind: "SQL_USER".into(), ..Default::default() };
        let mut login = Some(name.clone());
        let mut default_schema: Option<String> = Some("dbo".into());
        while !c.done() {
            if c.eat_seq(&["WITHOUT", "LOGIN"]) {
                login = None;
            } else if c.eat_seq(&["FOR", "LOGIN"]) || c.eat_seq(&["FROM", "LOGIN"]) {
                login = Some(c.ident()?);
            } else if c.eat_seq(&["FROM", "EXTERNAL", "PROVIDER"]) {
                p.kind = "EXTERNAL_USER".into();
                login = None;
            } else if c.eat_seq(&["FOR", "CERTIFICATE"]) || c.eat_seq(&["FROM", "CERTIFICATE"]) {
                p.kind = "CERTIFICATE_MAPPED_USER".into();
                c.ident()?;
                login = None;
            } else if c.eat_seq(&["FOR", "ASYMMETRIC", "KEY"]) || c.eat_seq(&["FROM", "ASYMMETRIC", "KEY"]) {
                p.kind = "ASYMMETRIC_KEY_MAPPED_USER".into();
                c.ident()?;
                login = None;
            } else if c.eat("DEFAULT_SCHEMA") {
                c.expect_punct("=")?;
                default_schema = Some(c.ident()?);
            } else {
                c.i += 1;
            }
        }
        if let Some(l) = &login
            && l.contains('\\')
        {
            p.kind = "WINDOWS_USER".into();
        }
        p.login = login;
        p.default_schema = default_schema;
        self.db.principals.retain(|x| !x.name.eq_ignore_ascii_case(&name));
        self.db.principals.push(p);
        self.origin(ObjectType::User, "", &name);
        Ok(())
    }

    fn add_member(&mut self, role: &str, member: &str) {
        if let Some(p) = self.db.principals.iter_mut().find(|p| p.name.eq_ignore_ascii_case(member))
            && !p.member_of.iter().any(|r| r.eq_ignore_ascii_case(role))
        {
            p.member_of.push(role.to_string());
        }
    }

    fn alter_role(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let role = c.ident()?;
        if c.eat_seq(&["ADD", "MEMBER"]) {
            let member = c.ident()?;
            if !self.db.principals.iter().any(|p| p.name.eq_ignore_ascii_case(&member)) {
                return c.fail(MessageKind::ReferenceNotFound);
            }
            self.add_member(&role, &member);
            return Ok(());
        }
        if c.eat_seq(&["DROP", "MEMBER"]) {
            let member = c.ident()?;
            if let Some(p) = self.db.principals.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&member)) {
                p.member_of.retain(|r| !r.eq_ignore_ascii_case(&role));
            }
            return Ok(());
        }
        c.fail(MessageKind::IgnoredStatement)
    }

    /// `ALTER AUTHORIZATION ON SCHEMA::s TO owner`.
    fn alter_authorization(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        c.expect("ON")?;
        let class = if c.peek().is_some_and(|t| t.kind == Tok::Word) && c.t.get(c.i + 1).is_some_and(|t| t.text == ":") {
            let w = c.ident()?.to_ascii_uppercase();
            c.expect_punct(":")?;
            c.expect_punct(":")?;
            w
        } else {
            "OBJECT".into()
        };
        let parts = c.multipart()?;
        c.expect("TO")?;
        let owner = c.ident()?;
        match class.as_str() {
            "SCHEMA" => {
                if let Some(s) = self.db.schemas.iter_mut().find(|s| parts.last().is_some_and(|n| s.name.eq_ignore_ascii_case(n))) {
                    s.owner = owner;
                }
            }
            "ROLE" => {
                if let Some(p) = self.db.principals.iter_mut().find(|p| parts.last().is_some_and(|n| p.name.eq_ignore_ascii_case(n))) {
                    p.owner = Some(owner);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn data_type(&self, c: &mut Cur) -> R<DataType> {
        let parts = c.multipart()?;
        let (schema, name) = match parts.as_slice() {
            [n] => (None, n.clone()),
            [s, n] if s.eq_ignore_ascii_case("sys") => (None, n.clone()),
            [s, n] => (Some(s.clone()), n.clone()),
            _ => return c.syntax(),
        };
        let lower = name.to_ascii_lowercase();
        if schema.is_some() || !SYSTEM_TYPES.contains(&lower.as_str()) {
            let t = DataType { name, schema: Some(schema.unwrap_or_else(|| self.schema_default())), ..Default::default() };
            if c.is_punct("(") {
                c.group()?;
            }
            return Ok(t);
        }
        let mut t = DataType { name: if lower == "rowversion" { "timestamp".into() } else { lower.clone() }, ..Default::default() };
        let mut args: Vec<String> = Vec::new();
        if c.is_punct("(") {
            let (a, b) = c.group()?;
            if t.name == "xml" {
                let mut s = c.sub(a, b);
                if s.eat("DOCUMENT") {
                    t.xml_document = true;
                } else {
                    s.eat("CONTENT");
                }
                let n = s.object("dbo")?;
                t.xml_collection = Some(n.quoted());
                return Ok(t);
            }
            for (x, y) in c.split_commas(a, b) {
                args.push(c.text(x, y - 1).trim().to_ascii_lowercase());
            }
        }
        let num = |k: usize| args.get(k).and_then(|a| a.parse::<i64>().ok());
        match t.name.as_str() {
            "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => {
                t.length = Some(if args.first().is_some_and(|a| a == "max") { -1 } else { num(0).unwrap_or(1) as i32 });
            }
            "decimal" | "numeric" => {
                t.precision = Some(num(0).unwrap_or(18) as u8);
                t.scale = Some(num(1).unwrap_or(0) as u8);
            }
            "datetime2" | "time" | "datetimeoffset" => t.scale = Some(num(0).unwrap_or(7) as u8),
            "float" => t.precision = Some(if num(0).unwrap_or(53) <= 24 { 24 } else { 53 }),
            _ => {}
        }
        Ok(t)
    }

    /// Parses one column definition (CREATE TABLE, ALTER TABLE ADD, table types).
    /// Constraints written on the column go to `extra`.
    fn column(&mut self, c: &mut Cur, table: &str, extra: &mut Vec<Constraint>) -> R<Column> {
        let name = c.ident()?;
        let mut col = Column { name: name.clone(), nullable: true, ..Default::default() };
        let mut nullable: Option<bool> = None;
        if c.eat("AS") {
            let (a, b) = expression(c, &["PERSISTED", "CONSTRAINT", "NOT", "NULL", "PRIMARY", "UNIQUE", "CHECK", "REFERENCES", "FOREIGN"]);
            let mut computed = Computed { definition: paren(c.text(a, b)), persisted: false };
            if c.eat("PERSISTED") {
                computed.persisted = true;
            }
            col.computed = Some(computed);
            col.data_type = DataType { name: "int".into(), ..Default::default() };
        } else {
            col.data_type = self.data_type(c)?;
        }
        let mut pending_name: Option<String> = None;
        while !c.done() && !c.is_punct(",") && !c.is_punct(")") {
            if c.eat("CONSTRAINT") {
                pending_name = Some(c.ident()?);
                continue;
            }
            if c.eat("COLLATE") {
                col.collation = Some(c.ident()?);
            } else if c.eat_seq(&["NOT", "NULL"]) {
                nullable = Some(false);
            } else if c.eat("NULL") {
                nullable = Some(true);
            } else if c.eat("IDENTITY") {
                let mut id = Identity { seed: "1".into(), increment: "1".into(), not_for_replication: false };
                if c.is_punct("(") {
                    let (a, b) = c.group()?;
                    let parts = c.split_commas(a, b);
                    if let [(s0, s1), (i0, i1)] = parts.as_slice() {
                        id.seed = c.text(*s0, s1 - 1).replace(' ', "");
                        id.increment = c.text(*i0, i1 - 1).replace(' ', "");
                    }
                }
                if c.eat_seq(&["NOT", "FOR", "REPLICATION"]) {
                    id.not_for_replication = true;
                }
                col.identity = Some(id);
            } else if c.eat("ROWGUIDCOL") {
                col.rowguidcol = true;
            } else if c.eat("SPARSE") {
                col.sparse = true;
            } else if c.eat("FILESTREAM") {
                col.filestream = true;
            } else if c.eat_seq(&["COLUMN_SET", "FOR", "ALL_SPARSE_COLUMNS"]) {
                col.column_set = true;
            } else if c.eat("HIDDEN") {
                col.hidden = true;
            } else if c.eat_seq(&["GENERATED", "ALWAYS", "AS"]) {
                let mut words = Vec::new();
                while let Some(t) = c.peek() {
                    let u = t.text.to_ascii_uppercase();
                    if matches!(u.as_str(), "ROW" | "TRANSACTION_ID" | "SEQUENCE_NUMBER" | "START" | "END") {
                        words.push(u);
                        c.i += 1;
                    } else {
                        break;
                    }
                }
                col.generated_always = Some(words.join(" "));
            } else if c.eat("MASKED") {
                c.expect("WITH")?;
                let (a, b) = c.group()?;
                let mut s = c.sub(a, b);
                s.expect("FUNCTION")?;
                s.expect_punct("=")?;
                col.masking_function = s.peek().map(|t| string_value(&t.text));
            } else if c.eat("ENCRYPTED") {
                c.expect("WITH")?;
                c.group()?;
            } else if c.eat("DEFAULT") {
                let (a, b) = expression(c, &["CONSTRAINT", "NOT", "NULL", "PRIMARY", "UNIQUE", "CHECK", "REFERENCES", "FOREIGN", "WITH", "IDENTITY", "COLLATE", "ROWGUIDCOL", "SPARSE"]);
                let definition = paren(c.text(a, b));
                if c.at_seq(&["WITH", "VALUES"]) {
                    c.i += 2;
                }
                let (dname, system_named) = match pending_name.take() {
                    Some(n) => {
                        let s = looks_system_named(&n);
                        (n, s)
                    }
                    None => (self.generated("DF", table, Some(&name)), true),
                };
                col.default = Some(DefaultConstraint { name: dname, system_named, definition });
            } else if c.at("PRIMARY") || c.at("UNIQUE") || c.at("CHECK") || c.at("REFERENCES") || c.at("FOREIGN") || c.at("INDEX") {
                let mut k = self.constraint(c, pending_name.take(), table, Some(&name))?;
                if let Constraint::Key(ix) | Constraint::KeyWithKind(ix, _) = &mut k
                    && ix.columns.is_empty()
                {
                    ix.columns.push(IndexColumn { name: name.clone(), descending: false });
                }
                if let Constraint::Foreign(fk) = &mut k
                    && fk.columns.is_empty()
                {
                    fk.columns.push(name.clone());
                }
                extra.push(k);
            } else {
                return c.syntax();
            }
        }
        // Identity columns, and columns of a NOT NULL alias type, are NOT NULL unless declared NULL.
        let not_null_type = |t: &DataType| {
            t.schema.as_ref().is_some_and(|s| {
                self.db.types.iter().any(|u| u.kind == UserTypeKind::Alias && !u.nullable && u.schema.eq_ignore_ascii_case(s) && u.name.eq_ignore_ascii_case(&t.name))
            })
        };
        if nullable.is_none() && (col.identity.is_some() || not_null_type(&col.data_type)) {
            nullable = Some(false);
        }
        col.nullable = nullable.unwrap_or(true);
        if col.collation.is_none() && is_character(&col.data_type) && col.computed.is_none() {
            col.collation = Some(self.defaults.collation.clone());
        }
        if !is_character(&col.data_type) || col.computed.is_some() {
            col.collation = None;
        }
        Ok(col)
    }

    /// PRIMARY KEY / UNIQUE / FOREIGN KEY / CHECK / INDEX after an optional CONSTRAINT name.
    fn constraint(&mut self, c: &mut Cur, name: Option<String>, table: &str, column: Option<&str>) -> R<Constraint> {
        let system_named = name.as_deref().is_none_or(looks_system_named);
        if c.at("PRIMARY") || c.at("UNIQUE") {
            let primary = c.eat_seq(&["PRIMARY", "KEY"]);
            if !primary {
                c.expect("UNIQUE")?;
            }
            let mut ix = Index {
                name: name.unwrap_or_else(|| self.generated(if primary { "PK" } else { "UQ" }, table, None)),
                primary_key: primary,
                unique: true,
                unique_constraint: !primary,
                system_named,
                allow_row_locks: true,
                allow_page_locks: true,
                kind: IndexKind::Nonclustered,
                ..Default::default()
            };
            let mut explicit = None;
            if c.eat("CLUSTERED") {
                explicit = Some(IndexKind::Clustered);
            } else if c.eat("NONCLUSTERED") {
                explicit = Some(if c.eat("HASH") { IndexKind::NonclusteredHash } else { IndexKind::Nonclustered });
            }
            if c.is_punct("(") {
                ix.columns = self.key_columns(c)?;
            }
            self.index_tail(c, &mut ix)?;
            return Ok(Constraint::KeyWithKind(ix, explicit));
        }
        if c.eat_seq(&["FOREIGN", "KEY"]) || c.at("REFERENCES") {
            let mut fk = ForeignKey {
                name: name.unwrap_or_else(|| self.generated("FK", table, None)),
                system_named,
                on_delete: "NO_ACTION".into(),
                on_update: "NO_ACTION".into(),
                ..Default::default()
            };
            if c.is_punct("(") {
                fk.columns = self.name_list(c)?;
            }
            c.expect("REFERENCES")?;
            fk.referenced_table = c.object(&self.schema_default())?;
            if c.is_punct("(") {
                fk.referenced_columns = self.name_list(c)?;
            }
            loop {
                if c.eat_seq(&["ON", "DELETE"]) {
                    fk.on_delete = referential_action(c)?;
                } else if c.eat_seq(&["ON", "UPDATE"]) {
                    fk.on_update = referential_action(c)?;
                } else if c.eat_seq(&["NOT", "FOR", "REPLICATION"]) {
                    fk.not_for_replication = true;
                } else {
                    break;
                }
            }
            return Ok(Constraint::Foreign(fk));
        }
        if c.eat("CHECK") {
            let mut ck = CheckConstraint { system_named, column: column.map(str::to_string), ..Default::default() };
            if c.eat_seq(&["NOT", "FOR", "REPLICATION"]) {
                ck.not_for_replication = true;
            }
            let (a, b) = c.group()?;
            ck.definition = paren(&expand_in_lists(c.text(a, b - 1)));
            ck.name = name.unwrap_or_else(|| self.generated("CK", table, Some(column.unwrap_or(""))));
            return Ok(Constraint::Check(ck));
        }
        if c.eat("INDEX") {
            let mut ix = Index { name: c.ident()?, allow_row_locks: true, allow_page_locks: true, ..Default::default() };
            if c.eat("UNIQUE") {
                ix.unique = true;
            }
            ix.kind = if c.eat("CLUSTERED") {
                if c.eat("COLUMNSTORE") { IndexKind::ClusteredColumnstore } else { IndexKind::Clustered }
            } else if c.eat("NONCLUSTERED") {
                if c.eat("HASH") {
                    IndexKind::NonclusteredHash
                } else if c.eat("COLUMNSTORE") {
                    IndexKind::NonclusteredColumnstore
                } else {
                    IndexKind::Nonclustered
                }
            } else if c.eat("HASH") {
                IndexKind::NonclusteredHash
            } else {
                IndexKind::Nonclustered
            };
            if c.is_punct("(") {
                ix.columns = self.key_columns(c)?;
            } else if let Some(col) = column {
                ix.columns.push(IndexColumn { name: col.to_string(), descending: false });
            }
            if c.eat("INCLUDE") {
                ix.included = self.name_list(c)?;
            }
            if c.eat("WHERE") {
                let (a, b) = expression(c, &["WITH", "ON"]);
                ix.filter = Some(paren(c.text(a, b)));
            }
            self.index_tail(c, &mut ix)?;
            return Ok(Constraint::Key(ix));
        }
        c.syntax()
    }

    fn name_list(&self, c: &mut Cur) -> R<Vec<String>> {
        let (a, b) = c.group()?;
        let mut out = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            out.push(s.ident()?);
        }
        Ok(out)
    }

    fn key_columns(&self, c: &mut Cur) -> R<Vec<IndexColumn>> {
        let (a, b) = c.group()?;
        let mut out = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let name = s.ident()?;
            let descending = s.eat("DESC");
            out.push(IndexColumn { name, descending });
        }
        Ok(out)
    }

    /// WITH (...) and ON <filegroup> after an index or key.
    fn index_tail(&self, c: &mut Cur, ix: &mut Index) -> R<()> {
        loop {
            if c.at("WITH") && c.t.get(c.i + 1).is_some_and(|t| t.text == "(") {
                c.i += 1;
                let (a, b) = c.group()?;
                for (x, y) in c.split_commas(a, b) {
                    let mut s = c.sub(x, y);
                    index_option(&mut s, ix)?;
                }
            } else if c.at("WITH") && c.word_at(c.i + 1, "FILLFACTOR") {
                c.i += 1;
                let (x, y) = (c.i, c.t.len());
                let mut s = c.sub(x, y);
                index_option(&mut s, ix)?;
                c.i += s.i;
            } else if c.eat("ON") {
                let ds = c.ident()?;
                ix.storage.data_space = Some(ds);
                if c.is_punct("(") {
                    let (a, b) = c.group()?;
                    let mut s = c.sub(a, b);
                    ix.storage.partition_column = Some(s.ident()?);
                }
            } else if c.eat("FILESTREAM_ON") {
                c.ident()?;
            } else {
                return Ok(());
            }
        }
    }

    fn create_table(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        if name.name.starts_with('#') {
            return c.fail(MessageKind::IgnoredStatement);
        }
        let mut t = Table { schema: name.schema.clone(), name: name.name.clone(), lock_escalation: "TABLE".into(), ..Default::default() };
        if c.eat_seq(&["AS", "FILETABLE"]) {
            return c.fail(MessageKind::IgnoredStatement);
        }
        let mut constraints = Vec::new();
        let mut period: Option<(String, String)> = None;
        if c.is_punct("(") {
            let (a, b) = c.group()?;
            let after = c.i;
            for (x, y) in c.split_commas(a, b) {
                let mut s = c.sub(x, y);
                if s.eat("CONSTRAINT") {
                    let n = s.ident()?;
                    constraints.push(self.constraint(&mut s, Some(n), &t.name, None)?);
                } else if s.at("PRIMARY") || s.at("UNIQUE") || s.at("FOREIGN") || s.at("CHECK") || s.at("INDEX") {
                    constraints.push(self.constraint(&mut s, None, &t.name, None)?);
                } else if s.eat_seq(&["PERIOD", "FOR", "SYSTEM_TIME"]) {
                    let cols = self.name_list(&mut s)?;
                    if let [a, b] = cols.as_slice() {
                        period = Some((a.clone(), b.clone()));
                    }
                } else {
                    let mut col = self.column(&mut s, &t.name, &mut constraints)?;
                    col.ordinal = t.columns.len() as u32 + 1;
                    t.columns.push(col);
                }
                if !s.done() {
                    return s.syntax();
                }
            }
            c.i = after;
        }
        // AS NODE / AS EDGE, storage and table options.
        let mut compression: Option<Vec<PartitionCompression>> = None;
        let mut versioning: Option<Temporal> = None;
        loop {
            if c.eat("AS") {
                let g = c.ident()?.to_ascii_uppercase();
                t.graph = Some(g);
            } else if c.eat("ON") {
                t.storage.data_space = Some(c.ident()?);
                if c.is_punct("(") {
                    let (a, b) = c.group()?;
                    let mut s = c.sub(a, b);
                    t.storage.partition_column = Some(s.ident()?);
                }
            } else if c.eat("TEXTIMAGE_ON") {
                t.text_filegroup = Some(c.ident()?);
            } else if c.eat("FILESTREAM_ON") {
                t.filestream_filegroup = Some(c.ident()?);
            } else if c.eat("WITH") {
                let (a, b) = c.group()?;
                for (x, y) in c.split_commas(a, b) {
                    let mut s = c.sub(x, y);
                    if s.eat("DATA_COMPRESSION") {
                        s.expect_punct("=")?;
                        let mut list = compression.take().unwrap_or_default();
                        list.extend(compression_option(&mut s)?);
                        compression = Some(list);
                    } else if s.eat("MEMORY_OPTIMIZED") {
                        s.expect_punct("=")?;
                        t.memory_optimized = s.on_off()?;
                    } else if s.eat("DURABILITY") {
                        s.expect_punct("=")?;
                        t.durability = Some(s.ident()?.to_ascii_uppercase());
                    } else if s.eat("SYSTEM_VERSIONING") {
                        s.expect_punct("=")?;
                        if s.on_off()? {
                            let mut tv = Temporal::default();
                            if s.is_punct("(") {
                                let (p, q) = s.group()?;
                                for (u, v) in s.split_commas(p, q) {
                                    let mut o = s.sub(u, v);
                                    if o.eat("HISTORY_TABLE") {
                                        o.expect_punct("=")?;
                                        tv.history_table = o.object(&self.schema_default())?;
                                    } else if o.eat("HISTORY_RETENTION_PERIOD") {
                                        o.expect_punct("=")?;
                                        let r = o.text(o.i, o.t.len() - 1).to_ascii_uppercase();
                                        if r != "INFINITE" {
                                            tv.retention = Some(r);
                                        }
                                    }
                                }
                            }
                            versioning = Some(tv);
                        }
                    }
                }
                // A further WITH clause may follow; keep looping.
                continue;
            } else {
                break;
            }
        }
        if !c.done() {
            return c.syntax();
        }
        if let Some(mut tv) = versioning {
            if let Some((a, b)) = &period {
                tv.period_start = a.clone();
                tv.period_end = b.clone();
            }
            if tv.history_table.name.is_empty() {
                tv.history_table = ObjectName::new(&t.schema, &format!("MSSQL_TemporalHistoryFor_{}", t.name));
            }
            t.temporal = Some(tv);
        }
        if let Some(list) = compression {
            t.storage.compression = normalize_compression(list);
        }
        if t.storage.data_space.is_none() {
            t.storage.data_space = Some(self.defaults.filegroup.clone());
        }
        if t.text_filegroup.is_none() {
            t.text_filegroup = t.storage.data_space.clone().filter(|_| t.storage.partition_column.is_none());
        }
        let key = ObjectName::new(&t.schema, &t.name);
        if let Some(i) = self.table_index(&key) {
            self.db.tables.remove(i);
        }
        self.db.tables.push(t);
        let ti = self.db.tables.len() - 1;
        self.origin(ObjectType::Table, &key.schema, &key.name);
        for k in constraints {
            self.add_constraint(ti, k, false, true);
        }
        Ok(())
    }

    /// Adds a key, index, foreign key or check to table `ti`.
    fn add_constraint(&mut self, ti: usize, k: Constraint, nocheck: bool, inline: bool) {
        let table_compression = self.db.tables[ti].storage.compression.clone();
        let t = &mut self.db.tables[ti];
        match k {
            Constraint::KeyWithKind(mut ix, explicit) => {
                let has_clustered = t.indexes.iter().any(|i| matches!(i.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore));
                ix.kind = explicit.unwrap_or(if ix.primary_key && !has_clustered { IndexKind::Clustered } else { IndexKind::Nonclustered });
                if ix.primary_key {
                    for kc in &ix.columns {
                        if let Some(col) = t.columns.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&kc.name)) {
                            col.nullable = false;
                        }
                    }
                }
                if inline && ix.kind == IndexKind::Clustered && ix.storage.compression.is_empty() {
                    ix.storage.compression = table_compression;
                }
                t.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                t.indexes.push(ix);
            }
            Constraint::Key(ix) => {
                t.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                t.indexes.push(ix);
            }
            Constraint::Foreign(mut fk) => {
                fk.not_trusted = nocheck;
                t.foreign_keys.retain(|f| !f.name.eq_ignore_ascii_case(&fk.name));
                t.foreign_keys.push(fk);
            }
            Constraint::Check(mut ck) => {
                ck.not_trusted = nocheck;
                t.check_constraints.retain(|f| !f.name.eq_ignore_ascii_case(&ck.name));
                t.check_constraints.push(ck);
            }
            Constraint::Default(col, d) => {
                if let Some(c) = t.columns.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&col)) {
                    c.default = Some(d);
                }
            }
        }
    }

    fn alter_table(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        let Some(ti) = self.table_index(&name) else { return c.fail(MessageKind::ReferenceNotFound) };
        let table = self.db.tables[ti].name.clone();
        let mut nocheck = false;
        if c.eat_seq(&["WITH", "NOCHECK"]) {
            nocheck = true;
        } else {
            c.eat_seq(&["WITH", "CHECK"]);
        }
        if c.eat("ADD") {
            let start = c.i;
            let end = c.t.len();
            for (x, y) in c.split_commas(start, end) {
                let mut s = c.sub(x, y);
                let n = if s.eat("CONSTRAINT") { Some(s.ident()?) } else { None };
                if s.at("DEFAULT") {
                    s.i += 1;
                    let (a, b) = expression(&mut s, &["FOR"]);
                    let definition = paren(s.text(a, b));
                    s.expect("FOR")?;
                    let col = s.ident()?;
                    let (dname, system_named) = match n {
                        Some(n) => {
                            let sys = looks_system_named(&n);
                            (n, sys)
                        }
                        None => (self.generated("DF", &table, Some(&col)), true),
                    };
                    s.eat_seq(&["WITH", "VALUES"]);
                    self.add_constraint(ti, Constraint::Default(col, DefaultConstraint { name: dname, system_named, definition }), false, false);
                } else if s.at("PRIMARY") || s.at("UNIQUE") || s.at("FOREIGN") || s.at("CHECK") || s.at("INDEX") {
                    let k = self.constraint(&mut s, n, &table, None)?;
                    self.add_constraint(ti, k, nocheck, false);
                } else if s.eat_seq(&["PERIOD", "FOR", "SYSTEM_TIME"]) {
                    s.group()?;
                } else {
                    let mut extra = Vec::new();
                    let mut col = self.column(&mut s, &table, &mut extra)?;
                    let t = &mut self.db.tables[ti];
                    col.ordinal = t.columns.len() as u32 + 1;
                    t.columns.push(col);
                    for k in extra {
                        self.add_constraint(ti, k, nocheck, false);
                    }
                }
                if !s.done() {
                    return s.syntax();
                }
            }
            return Ok(());
        }
        if c.eat("NOCHECK") || c.at("CHECK") {
            let disable = !c.eat("CHECK");
            c.expect("CONSTRAINT")?;
            let all = c.eat("ALL");
            let names: Vec<String> = if all { Vec::new() } else { vec![c.ident()?] };
            let t = &mut self.db.tables[ti];
            let hit = |n: &str| all || names.iter().any(|x| x.eq_ignore_ascii_case(n));
            for fk in t.foreign_keys.iter_mut().filter(|f| hit(&f.name)) {
                fk.disabled = disable;
                if disable {
                    fk.not_trusted = true;
                }
            }
            for ck in t.check_constraints.iter_mut().filter(|f| hit(&f.name)) {
                ck.disabled = disable;
                if disable {
                    ck.not_trusted = true;
                }
            }
            return Ok(());
        }
        if c.eat("SET") {
            let (a, b) = c.group()?;
            let mut s = c.sub(a, b);
            if s.eat("LOCK_ESCALATION") {
                s.expect_punct("=")?;
                self.db.tables[ti].lock_escalation = s.ident()?.to_ascii_uppercase();
            } else if s.eat("SYSTEM_VERSIONING") {
                s.expect_punct("=")?;
                if s.on_off()? {
                    let mut tv = Temporal::default();
                    if s.is_punct("(") {
                        let (p, q) = s.group()?;
                        for (u, v) in s.split_commas(p, q) {
                            let mut o = s.sub(u, v);
                            if o.eat("HISTORY_TABLE") {
                                o.expect_punct("=")?;
                                tv.history_table = o.object(&self.schema_default())?;
                            }
                        }
                    }
                    self.db.tables[ti].temporal = Some(tv);
                }
            }
            return Ok(());
        }
        if c.eat_seq(&["ENABLE", "CHANGE_TRACKING"]) {
            let mut ct = ChangeTracking::default();
            if c.eat("WITH") {
                let (a, b) = c.group()?;
                let mut s = c.sub(a, b);
                if s.eat("TRACK_COLUMNS_UPDATED") {
                    s.expect_punct("=")?;
                    ct.track_columns_updated = s.on_off()?;
                }
            }
            self.db.tables[ti].change_tracking = Some(ct);
            return Ok(());
        }
        if c.at("DISABLE") || c.at("ENABLE") {
            let disable = c.eat("DISABLE");
            if !disable {
                c.expect("ENABLE")?;
            }
            c.expect("TRIGGER")?;
            let all = c.eat("ALL");
            let name = if all { String::new() } else { c.multipart()?.pop().unwrap_or_default() };
            for tr in self.db.tables[ti].triggers.iter_mut().filter(|t| all || t.name.eq_ignore_ascii_case(&name)) {
                tr.disabled = disable;
            }
            return Ok(());
        }
        c.fail(MessageKind::IgnoredStatement)
    }

    /// `DISABLE TRIGGER t ON table`.
    fn enable_trigger(&mut self, c: &mut Cur) -> R<()> {
        let disable = c.eat("DISABLE");
        if !disable {
            c.expect("ENABLE")?;
        }
        c.expect("TRIGGER")?;
        let names = if c.eat("ALL") { Vec::new() } else { vec![c.multipart()?.pop().unwrap_or_default()] };
        c.expect("ON")?;
        if c.eat("DATABASE") {
            for t in self.db.ddl_triggers.iter_mut().filter(|t| names.is_empty() || names.iter().any(|x| x.eq_ignore_ascii_case(&t.name))) {
                t.disabled = disable;
            }
            return Ok(());
        }
        if c.at_seq(&["ALL", "SERVER"]) {
            return Ok(());
        }
        let on = c.object(&self.schema_default())?;
        let hit = |n: &str| names.is_empty() || names.iter().any(|x| x.eq_ignore_ascii_case(n));
        match self.parent(c, &on)? {
            Parent::Table(t) => self.db.tables[t].triggers.iter_mut().filter(|t| hit(&t.name)).for_each(|t| t.disabled = disable),
            Parent::View(v) => self.db.views[v].triggers.iter_mut().filter(|t| hit(&t.name)).for_each(|t| t.disabled = disable),
        }
        Ok(())
    }

    fn create_index(&mut self, c: &mut Cur) -> R<()> {
        c.i = 1;
        let mut ix = Index { allow_row_locks: true, allow_page_locks: true, ..Default::default() };
        ix.unique = c.eat("UNIQUE");
        ix.kind = if c.eat("CLUSTERED") {
            if c.eat("COLUMNSTORE") { IndexKind::ClusteredColumnstore } else { IndexKind::Clustered }
        } else if c.eat("NONCLUSTERED") {
            if c.eat("COLUMNSTORE") { IndexKind::NonclusteredColumnstore } else { IndexKind::Nonclustered }
        } else if c.eat("COLUMNSTORE") {
            IndexKind::NonclusteredColumnstore
        } else if c.at("XML") || c.at("PRIMARY") || c.at("SELECTIVE") || c.at("SPATIAL") {
            return self.xml_or_spatial_index(c);
        } else {
            IndexKind::Nonclustered
        };
        c.expect("INDEX")?;
        ix.name = c.ident()?;
        c.expect("ON")?;
        let on = c.object(&self.schema_default())?;
        if c.is_punct("(") {
            ix.columns = self.key_columns(c)?;
        }
        if c.eat("INCLUDE") {
            ix.included = self.name_list(c)?;
        }
        if c.eat("WHERE") {
            let (a, b) = expression(c, &["WITH", "ON"]);
            ix.filter = Some(paren(c.text(a, b)));
        }
        self.index_tail(c, &mut ix)?;
        if matches!(ix.kind, IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore) && ix.storage.compression.is_empty() {
            ix.storage.compression = vec![PartitionCompression { partition: 0, compression: "COLUMNSTORE".into() }];
        }
        match self.parent(c, &on)? {
            Parent::Table(t) => {
                let t = &mut self.db.tables[t];
                t.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                t.indexes.push(ix);
            }
            Parent::View(v) => {
                let v = &mut self.db.views[v];
                v.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                v.indexes.push(ix);
            }
        }
        Ok(())
    }

    /// CREATE [PRIMARY | SELECTIVE] XML INDEX and CREATE SPATIAL INDEX.
    fn xml_or_spatial_index(&mut self, c: &mut Cur) -> R<()> {
        let mut ix = Index { allow_row_locks: true, allow_page_locks: true, ..Default::default() };
        let spatial = c.eat("SPATIAL");
        let mut xml = XmlIndex::default();
        if !spatial {
            xml.kind = if c.eat("PRIMARY") {
                XmlIndexKind::Primary
            } else if c.eat("SELECTIVE") {
                XmlIndexKind::Selective
            } else {
                XmlIndexKind::Secondary
            };
            c.expect("XML")?;
        }
        c.expect("INDEX")?;
        ix.name = c.ident()?;
        c.expect("ON")?;
        let on = c.object(&self.schema_default())?;
        let parent = self.parent(c, &on)?;
        ix.columns = self.key_columns(c)?;
        if spatial {
            ix.kind = IndexKind::Spatial;
            let column = ix.columns.first().map(|k| k.name.clone()).unwrap_or_default();
            let geography = match parent {
                Parent::Table(t) => self.db.tables[t].columns.iter().any(|x| x.name.eq_ignore_ascii_case(&column) && x.data_type.name.eq_ignore_ascii_case("geography")),
                Parent::View(_) => false,
            };
            let mut sp = SpatialIndex { geography, ..Default::default() };
            if c.eat("USING") {
                sp.tessellation = c.ident()?.to_ascii_uppercase();
            }
            if sp.tessellation.is_empty() {
                sp.tessellation = if geography { "GEOGRAPHY_AUTO_GRID".into() } else { "GEOMETRY_AUTO_GRID".into() };
            }
            let auto = sp.tessellation.ends_with("AUTO_GRID");
            sp.grids = vec!["MEDIUM".into(); 4];
            sp.cells_per_object = match (auto, geography) {
                (false, _) => 16,
                (true, false) => 8,
                (true, true) => 12,
            };
            if c.at("WITH") && c.t.get(c.i + 1).is_some_and(|t| t.text == "(") {
                c.i += 1;
                let (a, b) = c.group()?;
                for (x, y) in c.split_commas(a, b) {
                    let mut s = c.sub(x, y);
                    let opt = s.ident()?.to_ascii_uppercase();
                    match opt.as_str() {
                        "BOUNDING_BOX" => {
                            s.expect_punct("=")?;
                            let (p, q) = s.group()?;
                            let mut named: Vec<(String, String)> = Vec::new();
                            for (u, v) in s.split_commas(p, q) {
                                let mut e = s.sub(u, v);
                                let key = if e.t.get(1).is_some_and(|t| t.text == "=") {
                                    let k = e.ident()?.to_ascii_uppercase();
                                    e.i += 1;
                                    k
                                } else {
                                    String::new()
                                };
                                let n: f64 = match e.number()?.parse() {
                                    Ok(n) => n,
                                    Err(_) => return e.syntax(),
                                };
                                named.push((key, n.to_string()));
                            }
                            let pick = |k: &str, i: usize| named.iter().find(|(n, _)| n == k).or(named.get(i)).map(|(_, v)| v.clone()).unwrap_or_default();
                            sp.bounding_box = vec![pick("XMIN", 0), pick("YMIN", 1), pick("XMAX", 2), pick("YMAX", 3)];
                        }
                        "GRIDS" => {
                            s.expect_punct("=")?;
                            let (p, q) = s.group()?;
                            for (i, (u, v)) in s.split_commas(p, q).into_iter().enumerate() {
                                let mut e = s.sub(u, v);
                                let mut level = i;
                                if e.t.get(1).is_some_and(|t| t.text == "=") {
                                    let k = e.ident()?.to_ascii_uppercase();
                                    level = k.strip_prefix("LEVEL_").and_then(|n| n.parse::<usize>().ok()).map(|n| n.saturating_sub(1)).unwrap_or(i);
                                    e.i += 1;
                                }
                                if level < 4 {
                                    sp.grids[level] = e.ident()?.to_ascii_uppercase();
                                }
                            }
                        }
                        "CELLS_PER_OBJECT" => {
                            s.expect_punct("=")?;
                            sp.cells_per_object = s.number()?.parse().unwrap_or(16);
                        }
                        _ => {
                            let mut probe = c.sub(x, y);
                            index_option(&mut probe, &mut ix)?;
                        }
                    }
                }
            }
            if geography {
                sp.bounding_box.clear();
            }
            ix.spatial = Some(sp);
        } else {
            ix.kind = IndexKind::Xml;
            if c.eat_seq(&["USING", "XML", "INDEX"]) {
                xml.using_index = Some(c.ident()?);
                c.expect("FOR")?;
                if c.is_punct("(") {
                    let (a, b) = c.group()?;
                    let mut s = c.sub(a, b);
                    xml.kind = XmlIndexKind::SecondarySelective;
                    xml.using_path = Some(s.ident()?);
                } else {
                    xml.secondary_type = Some(c.ident()?.to_ascii_uppercase());
                }
            }
            if c.eat_seq(&["WITH", "XMLNAMESPACES"]) {
                let (a, b) = c.group()?;
                for (x, y) in c.split_commas(a, b) {
                    let mut s = c.sub(x, y);
                    let default = s.eat("DEFAULT");
                    let uri = s.ident_or_string()?;
                    let prefix = if default {
                        None
                    } else {
                        s.expect("AS")?;
                        Some(s.ident()?)
                    };
                    xml.namespaces.push(XmlNamespace { prefix, uri });
                }
            }
            if xml.kind == XmlIndexKind::Selective && c.eat("FOR") {
                let (a, b) = c.group()?;
                for (n, (x, y)) in c.split_commas(a, b).into_iter().enumerate() {
                    let mut s = c.sub(x, y);
                    let mut p = SelectivePath { id: n as i32 + 1, name: s.ident()?, ..Default::default() };
                    s.expect_punct("=")?;
                    p.path = s.ident_or_string()?;
                    if s.eat("AS") {
                        if s.eat("SQL") {
                            p.sql = true;
                            p.data_type = Some(self.data_type(&mut s)?);
                        } else {
                            s.expect("XQUERY")?;
                            if let Some(t) = s.peek().filter(|t| t.kind == Tok::Str) {
                                let ty = string_value(&t.text);
                                s.i += 1;
                                if ty == "node()" {
                                    p.node_hint = true;
                                } else {
                                    p.xquery_type = Some(ty);
                                }
                            }
                            if s.eat("MAXLENGTH") {
                                let (u, v) = s.group()?;
                                p.max_length = s.text(u, v - 1).trim().parse().ok();
                            }
                        }
                    }
                    p.singleton = s.eat("SINGLETON");
                    xml.paths.push(p);
                }
            }
            ix.xml = Some(xml);
        }
        self.index_tail(c, &mut ix)?;
        match parent {
            Parent::Table(t) => {
                let t = &mut self.db.tables[t];
                t.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                t.indexes.push(ix);
            }
            Parent::View(v) => {
                let v = &mut self.db.views[v];
                v.indexes.retain(|i| !i.name.eq_ignore_ascii_case(&ix.name));
                v.indexes.push(ix);
            }
        }
        Ok(())
    }

    fn alter_index(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        c.expect("ON")?;
        let on = c.object(&self.schema_default())?;
        if !c.eat("DISABLE") {
            return Ok(());
        }
        let list = match self.parent(c, &on)? {
            Parent::Table(t) => &mut self.db.tables[t].indexes,
            Parent::View(v) => &mut self.db.views[v].indexes,
        };
        for ix in list.iter_mut().filter(|i| i.name.eq_ignore_ascii_case(&name)) {
            ix.disabled = true;
        }
        Ok(())
    }

    fn create_type(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        let mut t = UserType { schema: name.schema.clone(), name: name.name.clone(), nullable: true, ..Default::default() };
        if c.eat("FROM") {
            t.kind = UserTypeKind::Alias;
            t.base_type = Some(self.data_type(c)?);
            if c.eat_seq(&["NOT", "NULL"]) {
                t.nullable = false;
            } else {
                c.eat("NULL");
            }
        } else if c.eat_seq(&["EXTERNAL", "NAME"]) {
            t.kind = UserTypeKind::Clr;
            t.nullable = true;
            t.clr_class = Some(c.multipart()?.join("."));
        } else if c.eat_seq(&["AS", "TABLE"]) {
            t.kind = UserTypeKind::Table;
            t.nullable = false;
            let (a, b) = c.group()?;
            let after = c.i;
            let mut constraints = Vec::new();
            for (x, y) in c.split_commas(a, b) {
                let mut s = c.sub(x, y);
                if s.eat("CONSTRAINT") {
                    let n = s.ident()?;
                    constraints.push(self.constraint(&mut s, Some(n), &format!("TT_{}", t.name), None)?);
                } else if s.at("PRIMARY") || s.at("UNIQUE") || s.at("CHECK") || s.at("INDEX") {
                    constraints.push(self.constraint(&mut s, None, &format!("TT_{}", t.name), None)?);
                } else {
                    let mut col = self.column(&mut s, &format!("TT_{}", t.name), &mut constraints)?;
                    col.ordinal = t.columns.len() as u32 + 1;
                    t.columns.push(col);
                }
            }
            c.i = after;
            if c.eat("WITH") {
                let (a, b) = c.group()?;
                let mut s = c.sub(a, b);
                if s.eat("MEMORY_OPTIMIZED") {
                    s.expect_punct("=")?;
                    t.memory_optimized = s.on_off()?;
                }
            }
            for k in constraints {
                match k {
                    Constraint::KeyWithKind(mut ix, explicit) => {
                        let has_clustered = t.indexes.iter().any(|i| i.kind == IndexKind::Clustered);
                        ix.kind = explicit.unwrap_or(if ix.primary_key && !has_clustered { IndexKind::Clustered } else { IndexKind::Nonclustered });
                        if ix.primary_key {
                            for kc in &ix.columns {
                                if let Some(col) = t.columns.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&kc.name)) {
                                    col.nullable = false;
                                }
                            }
                        }
                        ix.storage.data_space = Some(self.defaults.filegroup.clone());
                        t.indexes.push(ix);
                    }
                    Constraint::Key(mut ix) => {
                        ix.storage.data_space = Some(self.defaults.filegroup.clone());
                        t.indexes.push(ix);
                    }
                    Constraint::Check(ck) => t.check_constraints.push(ck),
                    _ => {}
                }
            }
        } else {
            return c.syntax();
        }
        self.db.types.retain(|x| !(x.schema.eq_ignore_ascii_case(&t.schema) && x.name.eq_ignore_ascii_case(&t.name)));
        self.origin(ObjectType::UserDefinedType, &t.schema, &t.name);
        self.db.types.push(t);
        Ok(())
    }

    fn create_sequence(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        let mut s = Sequence {
            schema: name.schema.clone(),
            name: name.name.clone(),
            data_type: DataType { name: "bigint".into(), ..Default::default() },
            increment: "1".into(),
            cached: true,
            ..Default::default()
        };
        let (mut start, mut min, mut max) = (None, None, None);
        while !c.done() {
            if c.eat("AS") {
                s.data_type = self.data_type(c)?;
            } else if c.eat_seq(&["START", "WITH"]) {
                start = Some(c.number()?);
            } else if c.eat_seq(&["INCREMENT", "BY"]) {
                s.increment = c.number()?;
            } else if c.eat_seq(&["NO", "MINVALUE"]) {
                min = None;
            } else if c.eat("MINVALUE") {
                min = Some(c.number()?);
            } else if c.eat_seq(&["NO", "MAXVALUE"]) {
                max = None;
            } else if c.eat("MAXVALUE") {
                max = Some(c.number()?);
            } else if c.eat_seq(&["NO", "CYCLE"]) {
                s.cycle = false;
            } else if c.eat("CYCLE") {
                s.cycle = true;
            } else if c.eat_seq(&["NO", "CACHE"]) {
                s.cached = false;
            } else if c.eat("CACHE") {
                s.cached = true;
                if c.peek().is_some_and(|t| t.kind == Tok::Word && t.text.chars().all(|ch| ch.is_ascii_digit())) {
                    s.cache_size = c.number()?.parse().ok();
                }
            } else {
                return c.syntax();
            }
        }
        let (lo, hi) = type_range(&s.data_type);
        s.minimum = min.unwrap_or(lo);
        s.maximum = max.unwrap_or(hi);
        let ascending = !s.increment.starts_with('-');
        s.start = start.unwrap_or_else(|| if ascending { s.minimum.clone() } else { s.maximum.clone() });
        self.db.sequences.retain(|x| !(x.schema.eq_ignore_ascii_case(&s.schema) && x.name.eq_ignore_ascii_case(&s.name)));
        self.origin(ObjectType::Sequence, &s.schema, &s.name);
        self.db.sequences.push(s);
        Ok(())
    }

    fn create_synonym(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        c.expect("FOR")?;
        let parts = c.multipart()?;
        let base = if parts.len() == 1 {
            format!("[{}].{}", self.schema_default(), crate::db::quote(&parts[0]))
        } else {
            parts.iter().map(|p| if p.is_empty() { String::new() } else { crate::db::quote(p) }).collect::<Vec<_>>().join(".")
        };
        self.db.synonyms.retain(|x| !(x.schema.eq_ignore_ascii_case(&name.schema) && x.name.eq_ignore_ascii_case(&name.name)));
        self.origin(ObjectType::Synonym, &name.schema, &name.name);
        self.db.synonyms.push(Synonym { schema: name.schema, name: name.name, base_object: base });
        Ok(())
    }

    /// CREATE VIEW / PROCEDURE / FUNCTION / TRIGGER: the whole batch is the definition.
    fn module(&mut self, sql: &str, settings: &ModuleSettings) -> R<()> {
        let mut c = Cur::new(sql);
        let k = module_kind_index(&c, 0);
        let kind = c.t[k].text.to_ascii_uppercase();
        c.i = k + 1;
        let name = c.object(&self.schema_default())?;
        // Text as SQL Server keeps it: CREATE OR ALTER and ALTER are stored as CREATE.
        let definition = sql.to_string();
        match kind.as_str() {
            "VIEW" => {
                let mut v = View { schema: name.schema.clone(), name: name.name.clone(), settings: settings.clone(), ..Default::default() };
                if c.is_punct("(") {
                    c.group()?;
                }
                if c.eat("WITH") {
                    while !c.done() && !c.at("AS") {
                        if c.eat("SCHEMABINDING") {
                            v.schema_bound = true;
                        } else if c.eat("ENCRYPTION") {
                            v.encrypted = true;
                        } else {
                            c.i += 1;
                        }
                    }
                }
                v.definition = if v.encrypted { String::new() } else { definition };
                if let Some(i) = self.view_index(&name) {
                    // Indexes and triggers read earlier stay with the view.
                    let old = self.db.views.remove(i);
                    v.indexes = old.indexes;
                    v.triggers = old.triggers;
                }
                self.origin(ObjectType::View, &v.schema, &v.name);
                self.db.views.push(v);
            }
            "PROC" | "PROCEDURE" | "FUNCTION" => {
                let mut r = Routine { schema: name.schema.clone(), name: name.name.clone(), settings: settings.clone(), ..Default::default() };
                let function = kind == "FUNCTION";
                r.kind = if function { RoutineKind::ScalarFunction } else { RoutineKind::Procedure };
                let mut depth = 0;
                let mut clr = false;
                while let Some(t) = c.peek() {
                    if t.kind == Tok::Punct && t.text == "(" {
                        depth += 1;
                    } else if t.kind == Tok::Punct && t.text == ")" {
                        depth -= 1;
                    } else if depth == 0 && t.kind == Tok::Word {
                        let u = t.text.to_ascii_uppercase();
                        if function && u == "RETURNS" {
                            c.i += 1;
                            if c.at("TABLE") {
                                r.kind = RoutineKind::InlineTableFunction;
                            } else if c.peek().is_some_and(|t| t.text.starts_with('@')) {
                                r.kind = RoutineKind::TableFunction;
                                c.i += 1;
                                c.expect("TABLE")?;
                                if c.is_punct("(") {
                                    c.group()?;
                                }
                            }
                            continue;
                        }
                        if u == "WITH" {
                            c.i += 1;
                            routine_options(&mut c, &mut r)?;
                            continue;
                        }
                        if u == "EXTERNAL" {
                            clr = true;
                            c.i += 1;
                            c.expect("NAME")?;
                            r.clr_method = Some(c.multipart()?.join("."));
                            break;
                        }
                        if u == "AS" || u == "BEGIN" || u == "RETURN" {
                            break;
                        }
                    }
                    c.i += 1;
                }
                if clr {
                    r.kind = match r.kind {
                        RoutineKind::Procedure => RoutineKind::ClrProcedure,
                        RoutineKind::ScalarFunction => RoutineKind::ClrScalarFunction,
                        _ => RoutineKind::ClrTableFunction,
                    };
                } else if !r.encrypted {
                    r.definition = definition;
                }
                self.db.routines.retain(|x| !(x.schema.eq_ignore_ascii_case(&r.schema) && x.name.eq_ignore_ascii_case(&r.name)));
                let ty = if function { ObjectType::Function } else { ObjectType::StoredProcedure };
                self.origin(ty, &r.schema, &r.name);
                self.db.routines.push(r);
            }
            "TRIGGER" => {
                c.expect("ON")?;
                if c.at_seq(&["ALL", "SERVER"]) {
                    return c.fail(MessageKind::IgnoredStatement);
                }
                if c.eat("DATABASE") {
                    let encrypted = c.at_seq(&["WITH", "ENCRYPTION"]);
                    self.db.ddl_triggers.retain(|t| !t.name.eq_ignore_ascii_case(&name.name));
                    self.db.ddl_triggers.push(DdlTrigger {
                        name: name.name.clone(),
                        definition: if encrypted { String::new() } else { definition },
                        encrypted,
                        disabled: false,
                    });
                    self.origin_of(FileKind::DdlTrigger, "", &name.name);
                    return Ok(());
                }
                let on = c.object(&self.schema_default())?;
                let mut tr = Trigger { name: name.name.clone(), ..Default::default() };
                if c.eat("WITH") {
                    while !c.done() && !c.at("FOR") && !c.at("AFTER") && !c.at("INSTEAD") {
                        if c.eat("ENCRYPTION") {
                            tr.encrypted = true;
                        } else {
                            c.i += 1;
                        }
                    }
                }
                if c.eat_seq(&["INSTEAD", "OF"]) {
                    tr.instead_of = true;
                } else if !c.eat("FOR") && !c.eat("AFTER") {
                    return c.syntax();
                }
                let mut events = HashSet::new();
                while let Some(t) = c.peek() {
                    let u = t.text.to_ascii_uppercase();
                    if matches!(u.as_str(), "INSERT" | "UPDATE" | "DELETE") {
                        events.insert(u);
                    } else if t.text != "," {
                        break;
                    }
                    c.i += 1;
                }
                tr.events = ["INSERT", "UPDATE", "DELETE"].iter().filter(|e| events.contains(**e)).map(|e| e.to_string()).collect();
                c.eat_seq(&["WITH", "APPEND"]);
                if c.eat_seq(&["NOT", "FOR", "REPLICATION"]) {
                    tr.not_for_replication = true;
                }
                if !tr.encrypted {
                    tr.definition = definition;
                }
                match self.parent(&c, &on)? {
                    Parent::Table(t) => {
                        let t = &mut self.db.tables[t];
                        t.triggers.retain(|x| !x.name.eq_ignore_ascii_case(&tr.name));
                        t.triggers.push(tr);
                    }
                    Parent::View(v) => {
                        let v = &mut self.db.views[v];
                        v.triggers.retain(|x| !x.name.eq_ignore_ascii_case(&tr.name));
                        v.triggers.push(tr);
                    }
                }
            }
            _ => return c.fail(MessageKind::IgnoredStatement),
        }
        Ok(())
    }

    /// GRANT / DENY / REVOKE on the database, a schema or an object.
    fn permission(&mut self, c: &mut Cur) -> R<()> {
        let verb = c.ident()?.to_ascii_uppercase();
        if verb == "REVOKE" {
            c.eat_seq(&["GRANT", "OPTION", "FOR"]);
        }
        let mut perms = Vec::new();
        let mut cur = Vec::new();
        while !c.done() && !c.at("ON") && !c.at("TO") && !c.at("FROM") {
            let t = c.peek().unwrap();
            if t.text == "," {
                perms.push(cur.join(" "));
                cur.clear();
            } else if t.text == "(" {
                // Column permissions are not modelled.
                return c.fail(MessageKind::IgnoredStatement);
            } else {
                cur.push(t.text.to_ascii_uppercase());
            }
            c.i += 1;
        }
        if !cur.is_empty() {
            perms.push(cur.join(" "));
        }
        let mut p = Permission::default();
        if c.eat("ON") {
            let class = if c.t.get(c.i + 1).is_some_and(|t| t.text == ":") {
                let w = c.ident()?.to_ascii_uppercase();
                c.expect_punct(":")?;
                c.expect_punct(":")?;
                w
            } else {
                "OBJECT".to_string()
            };
            match class.as_str() {
                "OBJECT" => {
                    let n = c.object(&self.schema_default())?;
                    if c.is_punct("(") {
                        return c.fail(MessageKind::IgnoredStatement);
                    }
                    p.class = "OBJECT".into();
                    p.schema = Some(n.schema);
                    p.object = Some(n.name);
                }
                "SCHEMA" => {
                    p.class = "SCHEMA".into();
                    p.schema = Some(c.ident()?);
                }
                "DATABASE" => {
                    c.ident()?;
                    p.class = "DATABASE".into();
                }
                // TYPE::, XML SCHEMA COLLECTION::, ROLE::... are not modelled.
                _ => return c.fail(MessageKind::IgnoredStatement),
            }
        } else {
            p.class = "DATABASE".into();
        }
        if !c.eat("TO") && !c.eat("FROM") {
            return c.syntax();
        }
        let mut grantees = vec![c.ident()?];
        while c.eat_punct(",") {
            grantees.push(c.ident()?);
        }
        let with_grant = c.eat_seq(&["WITH", "GRANT", "OPTION"]);
        // CONNECT comes with CREATE USER: a plain GRANT or a REVOKE only says whether the user
        // still has it.
        if p.class == "DATABASE" && perms.iter().any(|x| x == "CONNECT") && (verb == "REVOKE" || (verb == "GRANT" && !with_grant)) {
            for g in &grantees {
                if let Some(u) = self.db.principals.iter_mut().find(|u| u.name.eq_ignore_ascii_case(g) && !u.is_role()) {
                    u.no_connect = verb == "REVOKE";
                }
            }
            perms.retain(|x| x != "CONNECT");
        }
        let state = match verb.as_str() {
            "DENY" => "DENY",
            _ if with_grant => "GRANT_WITH_GRANT_OPTION",
            _ => "GRANT",
        };
        for g in grantees {
            for perm in &perms {
                let mut x = p.clone();
                x.grantee = g.clone();
                x.permission = perm.clone();
                x.state = state.into();
                let same = |y: &Permission| {
                    y.class == x.class
                        && y.schema == x.schema
                        && y.object == x.object
                        && y.grantee.eq_ignore_ascii_case(&x.grantee)
                        && y.permission == x.permission
                };
                self.db.permissions.retain(|y| !same(y));
                if verb != "REVOKE" {
                    self.db.permissions.push(x);
                }
            }
        }
        Ok(())
    }

    /// EXEC of the system procedures that scripts use: extended properties, role members,
    /// trigger order.
    fn exec(&mut self, c: &mut Cur) -> R<()> {
        c.i = 1;
        // EXEC('...'), EXEC @sql and EXEC @rc = proc are not read.
        if c.is_punct("(") || c.peek().is_some_and(|t| t.kind == Tok::Str || t.text.starts_with('@')) {
            return c.fail(MessageKind::IgnoredStatement);
        }
        let proc = c.multipart()?.pop().unwrap_or_default().to_ascii_lowercase();
        let mut positional: Vec<Option<String>> = Vec::new();
        let mut named: HashMap<String, Option<String>> = HashMap::new();
        let end = c.t.len();
        for (x, y) in c.split_commas(c.i, end) {
            let mut s = c.sub(x, y);
            let mut key = None;
            if s.peek().is_some_and(|t| t.text.starts_with('@')) && s.t.get(1).is_some_and(|t| t.text == "=") {
                key = Some(s.t[0].text.to_ascii_lowercase());
                s.i = 2;
            }
            let value = if s.at("NULL") {
                None
            } else if s.peek().is_some_and(|t| t.kind == Tok::Str) {
                Some(string_value(&s.t[s.i].text))
            } else if s.done() {
                None
            } else {
                Some(s.text(s.i, s.t.len() - 1).to_string())
            };
            match key {
                Some(k) => {
                    named.insert(k, value);
                }
                None => positional.push(value),
            }
        }
        let arg = |name: &str, pos: usize| -> Option<String> {
            match named.get(name) {
                Some(v) => v.clone(),
                None => positional.get(pos).cloned().flatten(),
            }
        };
        match proc.as_str() {
            "sp_addextendedproperty" => {
                let Some(pname) = arg("@name", 0) else { return c.syntax() };
                let value = arg("@value", 1);
                let lv = |i: usize, t: &str, n: &str| (arg(t, 2 + i * 2).map(|s| s.to_ascii_uppercase()), arg(n, 3 + i * 2));
                let (t0, n0) = lv(0, "@level0type", "@level0name");
                let (t1, n1) = lv(1, "@level1type", "@level1name");
                let (t2, n2) = lv(2, "@level2type", "@level2name");
                let mut p = ExtendedProperty { name: pname, value, ..Default::default() };
                match (t0.as_deref(), t1) {
                    (None, _) => p.class = "DATABASE".into(),
                    (Some("SCHEMA"), None) => {
                        p.class = "SCHEMA".into();
                        p.schema = n0;
                    }
                    (Some("SCHEMA"), Some(t1)) => {
                        p.class = t1;
                        p.schema = n0;
                        p.object = n1;
                        if let (Some(t2), Some(n2)) = (t2, n2) {
                            p.minor_type = Some(t2);
                            p.minor_name = Some(n2);
                        }
                    }
                    _ => return c.fail(MessageKind::IgnoredStatement),
                }
                let same = |x: &ExtendedProperty| {
                    x.class == p.class
                        && x.schema == p.schema
                        && x.object == p.object
                        && x.minor_type == p.minor_type
                        && x.minor_name == p.minor_name
                        && x.name == p.name
                };
                self.db.extended_properties.retain(|x| !same(x));
                self.db.extended_properties.push(p);
                Ok(())
            }
            "sp_addrolemember" => {
                let (Some(role), Some(member)) = (arg("@rolename", 0), arg("@membername", 1)) else { return c.syntax() };
                self.add_member(&role, &member);
                Ok(())
            }
            "sp_settriggerorder" => {
                let (Some(name), Some(order), Some(event)) = (arg("@triggername", 0), arg("@order", 1), arg("@stmttype", 2)) else {
                    return c.syntax();
                };
                let mut sub = Cur::new(&name);
                let parts = sub.multipart().unwrap_or_else(|_| vec![name.clone()]);
                let tname = parts.last().cloned().unwrap_or_default();
                let order = order.to_ascii_uppercase();
                if order == "NONE" {
                    return Ok(());
                }
                for t in &mut self.db.tables {
                    for tr in t.triggers.iter_mut().filter(|tr| tr.name.eq_ignore_ascii_case(&tname)) {
                        tr.order.retain(|(e, _)| !e.eq_ignore_ascii_case(&event));
                        tr.order.push((event.to_ascii_uppercase(), order.clone()));
                        tr.order.sort();
                    }
                }
                Ok(())
            }
            "sp_bindefault" | "sp_bindrule" => {
                let rule = proc == "sp_bindrule";
                let (Some(what), Some(target)) = (arg(if rule { "@rulename" } else { "@defname" }, 0), arg("@objname", 1)) else {
                    return c.syntax();
                };
                let what = Cur::new(&what).object(&self.schema_default()).map(|n| n.quoted()).unwrap_or(what);
                let parts = Cur::new(&target).multipart().unwrap_or_default();
                // Bindings to alias types are not modelled.
                let [.., schema, table, column] = parts.as_slice() else { return c.fail(MessageKind::IgnoredStatement) };
                let Some(t) = self.table_index(&ObjectName::new(schema, table)) else { return c.fail(MessageKind::ReferenceNotFound) };
                let Some(col) = self.db.tables[t].columns.iter_mut().find(|x| x.name.eq_ignore_ascii_case(column)) else {
                    return c.fail(MessageKind::ReferenceNotFound);
                };
                if rule {
                    col.bound_rule = Some(what);
                } else {
                    col.bound_default = Some(what);
                }
                Ok(())
            }
            "sp_addtype" => c.fail(MessageKind::IgnoredStatement),
            _ => c.fail(MessageKind::IgnoredStatement),
        }
    }

    /// Ordering, defaults that depend on the whole model, and dependencies.
    fn finalize(&mut self) {
        let db = &mut self.db;
        for t in &mut db.tables {
            // The clustered index decides where and how the table is stored.
            if let Some(cx) = t.indexes.iter().find(|i| matches!(i.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore)) {
                if cx.storage.data_space.is_some() {
                    t.storage.data_space = cx.storage.data_space.clone();
                    t.storage.partition_column = cx.storage.partition_column.clone();
                }
                t.storage.compression = cx.storage.compression.clone();
            }
            let ds = t.storage.clone();
            for ix in &mut t.indexes {
                if ix.storage.data_space.is_none() {
                    ix.storage.data_space = ds.data_space.clone();
                    if ix.kind != IndexKind::NonclusteredColumnstore || ds.partition_column.is_some() {
                        ix.storage.partition_column = ds.partition_column.clone();
                    }
                }
            }
            if !t.columns.iter().any(is_lob) {
                t.text_filegroup = None;
            }
            if t.memory_optimized {
                t.storage = Storage::default();
                t.text_filegroup = None;
                for ix in &mut t.indexes {
                    ix.storage = Storage::default();
                }
            }
            for ck in &mut t.check_constraints {
                // SQL Server ties a check to a column when it references only that column.
                if ck.column.is_none() {
                    let refs: Vec<&Column> = t.columns.iter().filter(|c| mentions(&ck.definition, &c.name)).collect();
                    if refs.len() == 1 {
                        ck.column = Some(refs[0].name.clone());
                    }
                }
            }
            t.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            t.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
            t.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
            t.triggers.sort_by(|a, b| a.name.cmp(&b.name));
        }
        let history: Vec<ObjectName> = db.tables.iter().filter_map(|t| t.temporal.as_ref().map(|tv| tv.history_table.clone())).collect();
        for t in &mut db.tables {
            if history.iter().any(|h| h.schema.eq_ignore_ascii_case(&t.schema) && h.name.eq_ignore_ascii_case(&t.name)) {
                t.history_table = true;
            }
        }
        for v in &mut db.views {
            for ix in &mut v.indexes {
                if ix.storage.data_space.is_none() {
                    ix.storage.data_space = Some(self.defaults.filegroup.clone());
                }
            }
            v.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            v.triggers.sort_by(|a, b| a.name.cmp(&b.name));
        }
        for ty in &mut db.types {
            ty.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            ty.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }
        for p in &mut db.principals {
            p.member_of.sort();
        }
        db.schemas.sort_by(|a, b| a.name.cmp(&b.name));
        db.tables.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.views.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.routines.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.types.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.sequences.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.synonyms.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.principals.sort_by(|a, b| a.name.cmp(&b.name));
        db.permissions.sort();
        db.dependencies = dependencies(db);
    }
}

/// What a constraint clause produced.
enum Constraint {
    /// PK or UQ, with the CLUSTERED/NONCLUSTERED written (if any); the default depends on the table.
    KeyWithKind(Index, Option<IndexKind>),
    /// An inline INDEX.
    Key(Index),
    Foreign(ForeignKey),
    Check(CheckConstraint),
    Default(String, DefaultConstraint),
}

fn is_index_statement(c: &Cur) -> bool {
    (1..6).any(|k| c.word_at(k, "INDEX")) && !c.word_at(1, "TABLE")
}

fn is_lob(c: &Column) -> bool {
    let t = &c.data_type;
    matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml")
        || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1))
        || t.schema.is_some()
}

/// `(x)` stays, `x` becomes `(x)`: SQL Server stores defaults, filters and computed columns
/// in parentheses.
fn paren(s: &str) -> String {
    let s = s.trim();
    let t = tokenize(s);
    let sig: Vec<&crate::schema::text::Token> = t.iter().filter(|x| !matches!(x.kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).collect();
    if sig.first().is_some_and(|x| x.text == "(") && sig.last().is_some_and(|x| x.text == ")") {
        let mut depth = 0;
        let mut closes_at_end = true;
        for (k, x) in sig.iter().enumerate() {
            if x.kind == Tok::Punct && x.text == "(" {
                depth += 1;
            } else if x.kind == Tok::Punct && x.text == ")" {
                depth -= 1;
                if depth == 0 && k + 1 < sig.len() {
                    closes_at_end = false;
                }
            }
        }
        if closes_at_end {
            return s.to_string();
        }
    }
    format!("({s})")
}

/// Tokens of an expression starting at the cursor, up to a stop word or `,`/`)` at depth 0.
/// Returns an inclusive token range and leaves the cursor after it.
fn expression(c: &mut Cur, stop: &[&str]) -> (usize, usize) {
    let start = c.i;
    let mut depth = 0;
    while let Some(t) = c.peek() {
        if t.kind == Tok::Punct {
            match t.text.as_str() {
                "(" => depth += 1,
                ")" if depth == 0 => break,
                ")" => depth -= 1,
                "," if depth == 0 => break,
                _ => {}
            }
        } else if depth == 0 && t.kind == Tok::Word && c.i > start && stop.iter().any(|w| t.text.eq_ignore_ascii_case(w)) {
            // `NOT NULL` ends a default, but `IS NOT NULL` belongs to a filter.
            let prev = &c.t[c.i - 1].text;
            if !(t.text.eq_ignore_ascii_case("NOT") && prev.eq_ignore_ascii_case("IS"))
                && !(t.text.eq_ignore_ascii_case("NULL") && (prev.eq_ignore_ascii_case("IS") || prev.eq_ignore_ascii_case("NOT")))
            {
                break;
            }
        }
        c.i += 1;
    }
    (start, c.i.saturating_sub(1).max(start))
}

/// SQL Server stores `x IN (a, b)` in a check constraint as `x=b OR x=a`, and `x NOT IN (a, b)`
/// as `x<>b AND x<>a`.
fn expand_in_lists(expr: &str) -> String {
    let c = Cur::new(expr);
    let mut out = String::new();
    let mut pos = 0;
    let mut k = 0;
    while k < c.t.len() {
        let negated = c.word_at(k + 1, "NOT") && c.word_at(k + 2, "IN");
        let in_at = if negated { k + 2 } else { k + 1 };
        let is_operand = matches!(c.t[k].kind, Tok::Word | Tok::Quoted) && !c.t[k].text.chars().next().is_some_and(|ch| ch.is_ascii_digit());
        if is_operand
            && c.word_at(in_at, "IN")
            && c.t.get(in_at + 1).is_some_and(|t| t.text == "(")
            && let Some(close) = c.close_of(in_at + 1)
        {
            let items = c.split_commas(in_at + 2, close);
            let literal = |&(x, y): &(usize, usize)| y == x + 1 && matches!(c.t[x].kind, Tok::Str | Tok::Word) && !c.t[x].text.eq_ignore_ascii_case("SELECT");
            if !items.is_empty() && items.iter().all(literal) && !(k > 0 && c.t[k - 1].text == ".") {
                let operand = c.text(k, k);
                let (op, join) = if negated { ("<>", " AND ") } else { ("=", " OR ") };
                let parts: Vec<String> = items.iter().rev().map(|&(x, _)| format!("{operand}{op}{}", c.t[x].text)).collect();
                out.push_str(&expr[pos..c.t[k].start]);
                out.push_str(&parts.join(join));
                pos = c.t[close].end;
                k = close + 1;
                continue;
            }
        }
        k += 1;
    }
    out.push_str(&expr[pos..]);
    out
}

fn referential_action(c: &mut Cur) -> R<String> {
    if c.eat_seq(&["NO", "ACTION"]) {
        Ok("NO_ACTION".into())
    } else if c.eat("CASCADE") {
        Ok("CASCADE".into())
    } else if c.eat_seq(&["SET", "NULL"]) {
        Ok("SET_NULL".into())
    } else if c.eat_seq(&["SET", "DEFAULT"]) {
        Ok("SET_DEFAULT".into())
    } else {
        c.syntax()
    }
}

fn index_option(s: &mut Cur, ix: &mut Index) -> R<()> {
    let name = s.ident()?.to_ascii_uppercase();
    if !s.eat_punct("=") {
        // Old-style `WITH PAD_INDEX, FILLFACTOR = 90`.
        if name == "PAD_INDEX" {
            ix.pad_index = true;
        }
        return Ok(());
    }
    match name.as_str() {
        "PAD_INDEX" => ix.pad_index = s.on_off()?,
        "FILLFACTOR" => ix.fill_factor = s.number()?.parse().unwrap_or(0),
        "IGNORE_DUP_KEY" => ix.ignore_dup_key = s.on_off()?,
        "STATISTICS_NORECOMPUTE" => ix.no_recompute = s.on_off()?,
        "STATISTICS_INCREMENTAL" => ix.incremental = s.on_off()?,
        "ALLOW_ROW_LOCKS" => ix.allow_row_locks = s.on_off()?,
        "ALLOW_PAGE_LOCKS" => ix.allow_page_locks = s.on_off()?,
        "OPTIMIZE_FOR_SEQUENTIAL_KEY" => ix.optimize_for_sequential_key = s.on_off()?,
        "BUCKET_COUNT" => ix.bucket_count = s.number()?.parse().ok(),
        "DATA_COMPRESSION" => {
            let mut list = std::mem::take(&mut ix.storage.compression);
            list.extend(compression_option(s)?);
            ix.storage.compression = normalize_compression(list);
        }
        _ => s.i = s.t.len(),
    }
    Ok(())
}

/// `PAGE` or `ROW ON PARTITIONS (1, 3 TO 5)`.
fn compression_option(s: &mut Cur) -> R<Vec<PartitionCompression>> {
    let kind = s.ident()?.to_ascii_uppercase();
    if !s.eat_seq(&["ON", "PARTITIONS"]) {
        return Ok(vec![PartitionCompression { partition: 0, compression: kind }]);
    }
    let (a, b) = s.group()?;
    let mut out = Vec::new();
    for (x, y) in s.split_commas(a, b) {
        let mut p = s.sub(x, y);
        let from: u32 = p.number()?.parse().unwrap_or(0);
        let to: u32 = if p.eat("TO") { p.number()?.parse().unwrap_or(from) } else { from };
        for n in from..=to {
            out.push(PartitionCompression { partition: n, compression: kind.clone() });
        }
    }
    Ok(out)
}

fn normalize_compression(mut list: Vec<PartitionCompression>) -> Vec<PartitionCompression> {
    list.retain(|p| !(p.partition == 0 && p.compression == "NONE"));
    list.sort_by_key(|p| p.partition);
    list
}

fn routine_options(c: &mut Cur, r: &mut Routine) -> R<()> {
    loop {
        if c.eat("ENCRYPTION") {
            r.encrypted = true;
        } else if c.eat("SCHEMABINDING") {
            r.schema_bound = true;
        } else if c.eat("NATIVE_COMPILATION") {
            r.native_compilation = true;
        } else if c.eat("RECOMPILE")
            || c.eat("VIEW_METADATA")
            || c.eat_seq(&["RETURNS", "NULL", "ON", "NULL", "INPUT"])
            || c.eat_seq(&["CALLED", "ON", "NULL", "INPUT"])
        {
        } else if c.eat("INLINE") {
            c.expect_punct("=")?;
            c.on_off()?;
        } else if c.eat("EXECUTE") || c.eat("EXEC") {
            c.expect("AS")?;
            let who = c.ident_or_string()?;
            r.execute_as = match who.to_ascii_uppercase().as_str() {
                "CALLER" => None,
                "OWNER" => Some("OWNER".into()),
                // SELF is stored as the user who ran CREATE; scripts are run as dbo.
                "SELF" => Some("dbo".into()),
                _ => Some(who),
            };
        } else {
            return Ok(());
        }
        if !c.eat_punct(",") {
            return Ok(());
        }
    }
}

/// Minimum and maximum of a sequence's type, as SQL Server shows them.
fn type_range(t: &DataType) -> (String, String) {
    match t.name.as_str() {
        "tinyint" => ("0".into(), "255".into()),
        "smallint" => ("-32768".into(), "32767".into()),
        "int" => ("-2147483648".into(), "2147483647".into()),
        "decimal" | "numeric" => {
            let p = t.precision.unwrap_or(18) as usize;
            let nines = "9".repeat(p.saturating_sub(t.scale.unwrap_or(0) as usize).max(1));
            (format!("-{nines}"), nines)
        }
        _ => ("-9223372036854775808".into(), "9223372036854775807".into()),
    }
}

/// True when `name` appears as an identifier in an expression.
fn mentions(expr: &str, name: &str) -> bool {
    tokenize(expr).iter().any(|t| {
        matches!(t.kind, Tok::Word | Tok::Quoted)
            && unquote(&Tk { kind: t.kind, text: t.text.clone(), start: 0, end: 0, line: 0, col: 0 }).eq_ignore_ascii_case(name)
    })
}

/// References between objects, from foreign keys, typed columns, expressions and module text.
fn dependencies(db: &Database) -> Vec<Dependency> {
    let mut known: HashMap<(String, String), ObjectName> = HashMap::new();
    let mut add = |s: &str, n: &str| {
        known.insert((s.to_lowercase(), n.to_lowercase()), ObjectName::new(s, n));
    };
    for t in &db.tables {
        add(&t.schema, &t.name);
    }
    for v in &db.views {
        add(&v.schema, &v.name);
    }
    for r in &db.routines {
        add(&r.schema, &r.name);
    }
    for s in &db.sequences {
        add(&s.schema, &s.name);
    }
    for s in &db.synonyms {
        add(&s.schema, &s.name);
    }
    for t in &db.types {
        add(&t.schema, &t.name);
    }
    let mut deps: BTreeMap<(ObjectName, ObjectName), bool> = BTreeMap::new();
    let refs = |text: &str, schema: &str| -> Vec<ObjectName> {
        let toks: Vec<Tk> = Cur::new(text).t;
        let mut out = Vec::new();
        let mut k = 0;
        while k < toks.len() {
            if !matches!(toks[k].kind, Tok::Word | Tok::Quoted) || toks[k].text.starts_with('@') {
                k += 1;
                continue;
            }
            let mut parts = vec![unquote(&toks[k])];
            let mut j = k;
            while toks.get(j + 1).is_some_and(|t| t.text == ".") && toks.get(j + 2).is_some_and(|t| matches!(t.kind, Tok::Word | Tok::Quoted)) {
                parts.push(unquote(&toks[j + 2]));
                j += 2;
            }
            let candidates: Vec<(String, String)> = match parts.as_slice() {
                [n] => vec![(schema.to_lowercase(), n.to_lowercase()), ("dbo".into(), n.to_lowercase())],
                [s, n] | [_, s, n] => vec![(s.to_lowercase(), n.to_lowercase())],
                _ => Vec::new(),
            };
            if let Some(found) = candidates.iter().find_map(|c| known.get(c)) {
                out.push(found.clone());
            }
            k = j + 1;
        }
        out
    };
    for t in &db.tables {
        let from = t.object_name();
        for fk in &t.foreign_keys {
            if !(fk.referenced_table.schema.eq_ignore_ascii_case(&from.schema) && fk.referenced_table.name.eq_ignore_ascii_case(&from.name)) {
                let to = known.get(&(fk.referenced_table.schema.to_lowercase(), fk.referenced_table.name.to_lowercase())).cloned().unwrap_or(fk.referenced_table.clone());
                deps.entry((from.clone(), to)).or_insert(false);
            }
        }
        for c in &t.columns {
            if let Some(s) = &c.data_type.schema {
                let to = known.get(&(s.to_lowercase(), c.data_type.name.to_lowercase())).cloned().unwrap_or(ObjectName::new(s, &c.data_type.name));
                deps.insert((from.clone(), to), true);
            }
            let exprs = c.default.iter().map(|d| d.definition.as_str()).chain(c.computed.iter().map(|x| x.definition.as_str()));
            for e in exprs {
                for to in refs(e, &t.schema) {
                    if to != from {
                        deps.insert((from.clone(), to), true);
                    }
                }
            }
        }
        for ck in &t.check_constraints {
            for to in refs(&ck.definition, &t.schema) {
                if to != from {
                    deps.insert((from.clone(), to), true);
                }
            }
        }
    }
    let modules = db
        .views
        .iter()
        .map(|v| (ObjectName::new(&v.schema, &v.name), v.definition.as_str(), v.schema_bound))
        .chain(db.routines.iter().map(|r| (ObjectName::new(&r.schema, &r.name), r.definition.as_str(), r.schema_bound)));
    for (from, text, bound) in modules {
        // Skip the header's own name.
        let body = text.split_once(&from.name).map(|(_, b)| b).unwrap_or(text);
        for to in refs(body, &from.schema) {
            if to != from {
                let e = deps.entry((from.clone(), to)).or_insert(false);
                *e |= bound;
            }
        }
    }
    for s in &db.synonyms {
        let parts: Vec<String> = s.base_object.split('.').map(|p| p.trim_matches(['[', ']']).to_string()).collect();
        if let [.., schema, name] = parts.as_slice()
            && (parts.len() == 2 || (parts.len() == 3 && parts[0].eq_ignore_ascii_case(&db.name)))
            && let Some(to) = known.get(&(schema.to_lowercase(), name.to_lowercase()))
        {
            deps.entry((ObjectName::new(&s.schema, &s.name), to.clone())).or_insert(false);
        }
    }
    deps.into_iter().map(|((from, to), strong)| Dependency { from, to, strong }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(files: &[(&str, &str)]) -> ReadResult {
        let mut r = Reader::new(Defaults::default());
        for (name, text) in files {
            r.add_file(Path::new(name), text);
        }
        r.finish("Test")
    }

    #[test]
    fn splits_batches_on_go() {
        let b = batches("SELECT 1\r\nGO\r\n/* GO\r\n*/ SELECT 2\ngo 2\nSELECT 'x\nGO\n'\n");
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].1, 3);
        assert!(b[1].0.contains("GO\r\n*/"));
        assert!(b[2].0.contains("'x\nGO\n'"));
    }

    #[test]
    fn redgate_table_file() {
        let sql = "CREATE TABLE [dbo].[Customer]\r\n(\r\n[CustomerId] [int] NOT NULL IDENTITY(1, 1),\r\n\
[Name] [nvarchar] (100) COLLATE Latin1_General_CI_AS NOT NULL,\r\n[Phone] [dbo].[Phone] NULL,\r\n\
[Balance] [decimal] (18, 2) NOT NULL CONSTRAINT [DF_Customer_Balance] DEFAULT ((0)),\r\n\
[CreatedAt] [datetime2] (3) NOT NULL CONSTRAINT [DF__Customer__Create__3D5E1FD2] DEFAULT (sysutcdatetime()),\r\n\
[Notes] [nvarchar] (max) NULL,\r\n[NameLength] AS (len([Name])) PERSISTED\r\n)\r\nGO\r\n\
ALTER TABLE [dbo].[Customer] ADD CONSTRAINT [CK_Customer_Balance] CHECK (([Balance]>=(0)))\r\nGO\r\n\
ALTER TABLE [dbo].[Customer] ADD CONSTRAINT [PK_Customer] PRIMARY KEY CLUSTERED ([CustomerId])\r\nGO\r\n\
CREATE NONCLUSTERED INDEX [IX_Customer_Name] ON [dbo].[Customer] ([Name] DESC) INCLUDE ([Notes]) WHERE ([Name] IS NOT NULL) WITH (FILLFACTOR=90)\r\nGO\r\n\
DENY DELETE ON  [dbo].[Customer] TO [Reporting]\r\nGO\r\n\
EXEC sp_addextendedproperty N'MS_Description', N'People', 'SCHEMA', N'dbo', 'TABLE', N'Customer', 'COLUMN', N'Name'\r\nGO\r\n";
        let r = read(&[("Tables/dbo.Customer.sql", sql), ("Types/dbo.Phone.sql", "CREATE TYPE [dbo].[Phone] FROM varchar (20) NOT NULL\r\nGO\r\n")]);
        assert!(r.messages.is_empty(), "{:?}", r.messages);
        let t = r.database.table("dbo", "Customer").unwrap();
        assert_eq!(t.columns.len(), 7);
        assert_eq!(t.column("Name").unwrap().collation.as_deref(), Some("Latin1_General_CI_AS"));
        assert_eq!(t.column("Notes").unwrap().collation.as_deref(), Some("SQL_Latin1_General_CP1_CI_AS"));
        assert_eq!(t.column("Notes").unwrap().data_type.length, Some(-1));
        assert_eq!(t.column("Phone").unwrap().data_type.sql(), "[dbo].[Phone]");
        assert!(t.column("CreatedAt").unwrap().default.as_ref().unwrap().system_named);
        assert_eq!(t.column("Balance").unwrap().default.as_ref().unwrap().definition, "((0))");
        assert!(t.column("NameLength").unwrap().computed.as_ref().unwrap().persisted);
        let pk = t.primary_key().unwrap();
        assert_eq!((pk.kind.clone(), pk.unique), (IndexKind::Clustered, true));
        let ix = t.indexes.iter().find(|i| i.name == "IX_Customer_Name").unwrap();
        assert_eq!((ix.fill_factor, ix.columns[0].descending, ix.filter.as_deref()), (90, true, Some("([Name] IS NOT NULL)")));
        assert_eq!(t.check_constraints[0].column.as_deref(), Some("Balance"));
        assert_eq!(t.text_filegroup.as_deref(), Some("PRIMARY"));
        assert_eq!(r.database.permissions[0].state, "DENY");
        assert_eq!(r.database.extended_properties[0].minor_name.as_deref(), Some("Name"));
        assert_eq!(r.origins.get(&(FileKind::Object(ObjectType::Table), "dbo".into(), "Customer".into())).unwrap(), Path::new("Tables/dbo.Customer.sql"));
    }

    #[test]
    fn hand_written_script() {
        let sql = "CREATE TABLE Sales.[Order] (\n  OrderId bigint NOT NULL DEFAULT (NEXT VALUE FOR dbo.OrderNumbers),\n  \
Status char(1) NOT NULL CHECK (Status IN ('N', 'P')),\n  CONSTRAINT PK_Order PRIMARY KEY CLUSTERED (OrderId),\n  \
CustomerId int REFERENCES dbo.Customer (CustomerId) ON DELETE CASCADE\n) WITH (DATA_COMPRESSION = PAGE);\n\
ALTER TABLE Sales.[Order] NOCHECK CONSTRAINT ALL;\nGO\n\
CREATE SCHEMA Sales AUTHORIZATION dbo;\nGO\n\
CREATE TABLE dbo.Customer (CustomerId int PRIMARY KEY)\nGO\n";
        let r = read(&[("a.sql", sql)]);
        assert!(r.messages.is_empty(), "{:?}", r.messages);
        let t = r.database.table("Sales", "Order").unwrap();
        assert_eq!(t.storage.compression[0].compression, "PAGE");
        assert_eq!(t.primary_key().unwrap().storage.compression[0].compression, "PAGE");
        assert!(t.column("OrderId").unwrap().default.as_ref().unwrap().system_named);
        assert_eq!(t.foreign_keys[0].on_delete, "CASCADE");
        assert!(t.foreign_keys[0].disabled && t.foreign_keys[0].not_trusted);
        assert!(t.column("CustomerId").unwrap().nullable);
        assert_eq!(t.check_constraints[0].column.as_deref(), Some("Status"));
        let c = r.database.table("dbo", "Customer").unwrap();
        assert!(!c.columns[0].nullable && c.primary_key().unwrap().system_named);
        assert!(r.database.dependencies.iter().any(|d| d.from.name == "Order" && d.to.name == "Customer"));
    }

    #[test]
    fn modules_and_settings() {
        let d = read(&[("t.sql", "CREATE TRIGGER [Audit] ON DATABASE FOR CREATE_TABLE AS PRINT 1
GO
DISABLE TRIGGER [Audit] ON DATABASE
GO
")]);
        assert!(d.messages.is_empty(), "{:?}", d.messages);
        assert!(d.database.ddl_triggers[0].disabled && d.database.ddl_triggers[0].definition.starts_with("CREATE TRIGGER"));
        let b = read(&[("b.sql", "CREATE TABLE [dbo].[T] ([a] [int] NULL)\r\nGO\r\nEXEC sp_bindefault N'[dbo].[D]', N'[dbo].[T].[a]'\r\nGO\r\nEXEC sp_bindrule N'dbo.R', N'dbo.T.a'\r\nGO\r\n")]);
        assert!(b.messages.is_empty(), "{:?}", b.messages);
        let col = &b.database.tables[0].columns[0];
        assert_eq!((col.bound_default.as_deref(), col.bound_rule.as_deref()), (Some("[dbo].[D]"), Some("[dbo].[R]")));
        let r = read(&[(
            "p.sql",
            "SET QUOTED_IDENTIFIER OFF\nGO\nSET ANSI_NULLS ON\nGO\nCREATE PROCEDURE [Sales].[GetOrders] @CustomerId int\nWITH EXECUTE AS OWNER\nAS\nSELECT * FROM Sales.[Order];\n\nGO\n\
GRANT EXECUTE ON  [Sales].[GetOrders] TO [app]\nGO\n\
CREATE FUNCTION dbo.T (@a int) RETURNS TABLE AS RETURN SELECT 1 AS x\nGO\n\
CREATE FUNCTION dbo.M () RETURNS @t TABLE (a int) AS BEGIN RETURN END\nGO\n\
CREATE VIEW v WITH SCHEMABINDING AS SELECT 1 AS a\nGO\n\
CREATE TRIGGER [Sales].[Order_Audit] ON [Sales].[Order] AFTER UPDATE, INSERT AS\nBEGIN SET NOCOUNT ON; END;\nGO\n\
CREATE TABLE Sales.[Order] (a int)\nGO\n",
        )]);
        assert!(r.messages.is_empty(), "{:?}", r.messages);
        let p = r.database.routines.iter().find(|r| r.name == "GetOrders").unwrap();
        assert_eq!(p.execute_as.as_deref(), Some("OWNER"));
        assert!(!p.settings.quoted_identifier && p.settings.ansi_nulls);
        assert!(p.definition.starts_with("CREATE PROCEDURE") && p.definition.ends_with("[Order];\n"));
        assert_eq!(module_text("\r\nCREATE VIEW v AS SELECT 1\n\r\n"), "CREATE VIEW v AS SELECT 1\n");
        assert_eq!(module_text("CREATE VIEW v AS SELECT 1\r\n"), "CREATE VIEW v AS SELECT 1\r\n");
        let kinds: Vec<RoutineKind> = r.database.routines.iter().map(|r| r.kind.clone()).collect();
        assert!(kinds.contains(&RoutineKind::InlineTableFunction) && kinds.contains(&RoutineKind::TableFunction));
        assert!(r.database.views[0].schema_bound && r.database.views[0].schema == "dbo");
        let tr = &r.database.table("Sales", "Order").unwrap().triggers[0];
        assert_eq!(tr.events, ["INSERT", "UPDATE"]);
        assert_eq!(r.database.permissions[0].object.as_deref(), Some("GetOrders"));
    }

    #[test]
    fn security_and_sequences() {
        let r = read(&[
            ("r.sql", "CREATE ROLE [Reporting]\r\nAUTHORIZATION [dbo]\r\nGO\r\nALTER ROLE [Reporting] ADD MEMBER [app]\r\nGO\r\nGRANT VIEW DEFINITION TO [Reporting]"),
            ("u.sql", "CREATE USER [app] WITHOUT LOGIN WITH DEFAULT_SCHEMA=[Sales]\r\nGO\r\nEXEC sp_addrolemember N'db_datareader', N'app'\r\nGO\r\n"),
            ("s.sql", "CREATE SCHEMA [Sales]\r\nAUTHORIZATION [dbo]\r\nGO\r\nGRANT SELECT ON SCHEMA:: [Sales] TO [Reporting]\r\nGO\r\n"),
            ("q.sql", "CREATE SEQUENCE [dbo].[OrderNumbers]\r\nAS bigint\r\nSTART WITH 1000\r\nINCREMENT BY 1\r\nMINVALUE -9223372036854775808\r\nMAXVALUE 9223372036854775807\r\nNO CYCLE\r\nCACHE 50\r\nGO\r\nCREATE SEQUENCE dbo.S AS int"),
            ("y.sql", "CREATE SYNONYM [dbo].[Orders] FOR [Sales].[Order]\r\nGO\r\n"),
        ]);
        assert!(r.messages.is_empty(), "{:?}", r.messages);
        let app = r.database.principals.iter().find(|p| p.name == "app").unwrap();
        assert_eq!(app.member_of, ["Reporting", "db_datareader"]);
        assert_eq!((app.login.as_deref(), app.default_schema.as_deref()), (None, Some("Sales")));
        assert_eq!(r.database.permissions.len(), 2);
        let s = &r.database.sequences[0];
        assert_eq!((s.start.as_str(), s.cache_size), ("1000", Some(50)));
        let t = &r.database.sequences[1];
        assert_eq!((t.start.as_str(), t.maximum.as_str(), t.cache_size), ("-2147483648", "2147483647", None));
        assert_eq!(r.database.synonyms[0].base_object, "[Sales].[Order]");
    }

    #[test]
    fn reports_what_it_cannot_read() {
        let r = read(&[("b.sql", "CREATE TABLE [dbo].[X] ( [a] int NOT NULL,, )\r\nGO\r\nPRINT 'hi'\r\nGO\r\nIF 1=1 CREATE TABLE dbo.Y (a int)\r\nGO\r\nSELECT 1\r\n")]);
        let kinds: Vec<(MessageKind, usize)> = r.messages.iter().map(|m| (m.kind, m.line)).collect();
        assert_eq!(kinds, [(MessageKind::InvalidSyntax, 1), (MessageKind::IgnoredStatement, 3), (MessageKind::IfTakenAsTrue, 5), (MessageKind::IgnoredStatement, 7)]);
        assert!(r.database.table("dbo", "X").is_none());
        assert!(r.database.table("dbo", "Y").is_some());
        assert_eq!(r.messages[0].text(), "Invalid syntax found in file b.sql at line 1");
    }

    #[test]
    fn in_lists_become_comparisons() {
        assert_eq!(expand_in_lists("Status IN ('N', 'P', 'S')"), "Status='S' OR Status='P' OR Status='N'");
        assert_eq!(expand_in_lists("[a] NOT IN (1,2) AND b > 0"), "[a]<>2 AND [a]<>1 AND b > 0");
        assert_eq!(expand_in_lists("a IN (SELECT x FROM t)"), "a IN (SELECT x FROM t)");
    }

    #[test]
    fn system_names() {
        assert!(looks_system_named("DF__Customer__Create__3D5E1FD2"));
        assert!(looks_system_named("PK__TT_IdLis__3214EC07019F7773"));
        assert!(!looks_system_named("PK_Customer"));
    }

    #[test]
    fn connect_round_trips() {
        use crate::schema::{Opt, Options};
        let mut db = Database::default();
        for (name, no_connect) in [("Plain", false), ("NoConnect", true)] {
            db.principals.push(Principal { name: name.into(), kind: "SQL_USER".into(), no_connect, ..Default::default() });
        }
        for o in [Options::default_set(), Options::default_set().with(Opt::WriteAsDataToolsPermissions)] {
            let texts: Vec<String> = db.principals.iter().map(|p| super::super::script::user(&db, p, &o)).collect();
            let data_tools = o.has(Opt::WriteAsDataToolsPermissions);
            assert_eq!(texts[0].contains("GRANT CONNECT TO [Plain]"), data_tools);
            assert_eq!(texts[1].contains("REVOKE CONNECT TO [NoConnect]"), !data_tools);
            let files: Vec<(String, &str)> = texts.iter().enumerate().map(|(i, t)| (format!("u{i}.sql"), t.as_str())).collect();
            let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (n.as_str(), *t)).collect();
            let r = read(&files);
            assert!(r.messages.is_empty(), "{:?}", r.messages);
            let back: Vec<(&str, bool)> = r.database.principals.iter().map(|p| (p.name.as_str(), p.no_connect)).collect();
            // Read back, a user is connected unless revoked: Data Tools style loses NoConnect,
            // as in SQL Compare.
            assert_eq!(back, [("NoConnect", !data_tools), ("Plain", false)]);
            assert!(r.database.permissions.is_empty());
        }
    }
}
