//! Context-aware suggestions for the SSMS extension.
//!
//! The extension loads database metadata and sends it once per connection; each
//! request then carries the script text and caret position. Offsets in requests
//! and responses are UTF-16 code units, matching the editor.

use serde::{Deserialize, Serialize};

use crate::keywords::{is_reserved, DATA_TYPES};
use crate::lexer::{lex_partial, Kind, Token};
use crate::snippets::Snippet;
use crate::transform::{missing_terminators, upper_words};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Schema {
    pub database: String,
    pub default_schema: String,
    pub schemas: Vec<String>,
    pub databases: Vec<String>,
    /// Linked servers, for four-part names.
    pub linked_servers: Vec<String>,
    pub objects: Vec<DbObject>,
    pub foreign_keys: Vec<ForeignKey>,
}

/// `schema.table (columns)` references `ref_schema.ref_table (ref_columns)`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ForeignKey {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    /// table, view, procedure, scalar_function, table_function or synonym.
    pub kind: String,
    pub columns: Vec<Column>,
    pub params: Vec<Param>,
    /// The description people gave it (the MS_Description extended property).
    pub description: Option<String>,
    /// The definitions of its CHECK constraints, as stored: `([Status]='New' OR [Status]='Done')`.
    pub checks: Vec<String>,
    /// Another database's object, named with the database (`Other.dbo.t`); empty for this one's.
    #[serde(skip)]
    pub database: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub nullable: bool,
    /// Identity, computed and rowversion columns cannot be inserted or updated.
    pub identity: bool,
    pub computed: bool,
    /// The definition of the column's DEFAULT constraint, as stored: `(sysdatetime())`.
    pub default: Option<String>,
    /// The column's MS_Description.
    pub description: Option<String>,
    /// Part of the primary key.
    pub key: bool,
    /// The first column of an index.
    pub indexed: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub output: bool,
    /// The parameter's MS_Description.
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub label: String,
    pub insert: String,
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Completion {
    /// UTF-16 offset where the replaced text starts; it ends at the caret.
    pub start: usize,
    pub items: Vec<Item>,
    /// The item to select as the list opens: one that matches what was typed for sure (the whole
    /// word, its start, or the initials of its parts). None when the matches are only guesses,
    /// so Enter and Tab keep their usual meaning instead of inserting a guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<usize>,
}

/// How well a label matches what was typed: 0 the whole word, 1 its start, 2 the initials of
/// its parts (`cid` for CustomerId, `od` for OrderDetail), 3 somewhere inside; None for no match.
fn match_quality(label: &str, typed: &str) -> Option<u8> {
    let label = label.trim_start_matches('@');
    let lower = label.to_ascii_lowercase();
    let needle = typed.trim_start_matches('@').to_ascii_lowercase();
    if needle.is_empty() {
        return Some(1);
    }
    if lower == needle {
        return Some(0);
    }
    if lower.starts_with(&needle) {
        return Some(1);
    }
    if camel_match(&label_parts(label), &needle.chars().collect::<Vec<_>>()) {
        return Some(2);
    }
    if needle.len() >= 2 && lower.contains(&needle) {
        return Some(3);
    }
    if near(&lower, &needle) {
        return Some(4);
    }
    None
}

/// Whether the typed letters start the name but for a typo: one letter wrong, missing, extra or two
/// swapped (`cusotmer` for Customers), two in eight letters or more; four letters at least.
fn near(label: &str, typed: &str) -> bool {
    // Checked for every name that does not match, so most go at their first letters, without
    // counting: a typo there is rare.
    let mut l = label.chars();
    let (first, second) = (l.next(), l.next());
    let t = typed.chars().next();
    if typed.len() < 4 || label.len() + 1 < typed.len() || (first != t && second != t) {
        return false;
    }
    let typed: Vec<char> = typed.chars().collect();
    let label: Vec<char> = label.chars().collect();
    let n = typed.len();
    if n < 4 {
        return false;
    }
    let most = if n >= 8 { 2 } else { 1 };
    (n - 1..=n + 1).filter(|&len| len <= label.len()).any(|len| distance(&label[..len], &typed) <= most)
}

/// Edits between two words: letters changed, added, removed, or two neighbours swapped.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// The parts of a name, lowercased: words split at underscores, dots, spaces and case changes
/// (CustomerId: customer, id; HTTPStatus: http, status).
fn label_parts(label: &str) -> Vec<Vec<char>> {
    let chars: Vec<char> = label.chars().collect();
    let mut parts: Vec<Vec<char>> = Vec::new();
    let mut current = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous = if i > 0 { chars[i - 1] } else { ' ' };
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let new_part = c.is_uppercase() && (previous.is_lowercase() || previous.is_ascii_digit() || (previous.is_uppercase() && next_lower));
        if new_part && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Whether the typed letters start the first part and then, in order, the start of later parts:
/// each part used gives one or more of its first letters (`cid`: C-ustomer, I-d; `custid`).
fn camel_match(parts: &[Vec<char>], needle: &[char]) -> bool {
    fn rest(parts: &[Vec<char>], needle: &[char]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let Some((part, later)) = parts.split_first() else { return false };
        let most = part.len().min(needle.len());
        (1..=most).rev().any(|take| part[..take] == needle[..take] && (take == needle.len() || (0..later.len()).any(|skip| rest(&later[skip..], &needle[take..]))))
    }
    parts.len() > 1 && rest(parts, needle)
}
const MAX_ITEMS: usize = 300;

/// Keywords inside conditions, select lists and the clauses that follow them.
const EXPRESSION_KEYWORDS: &[&str] = &[
    "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "BETWEEN", "EXISTS", "CASE", "WHEN", "THEN", "ELSE", "END", "AS",
    "ASC", "DESC", "DISTINCT", "TOP", "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "JOIN",
    "INNER JOIN", "LEFT JOIN", "OVER", "PARTITION BY", "COLLATE", "OFFSET", "FETCH",
];

/// What may follow a table and its alias in FROM.
const CLAUSE_KEYWORDS: &[&str] = &[
    "WHERE", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "CROSS APPLY", "OUTER APPLY",
    "GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "EXCEPT", "INTERSECT", "OPTION",
];

/// What may begin a statement.
const STATEMENT_START: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "WITH", "DECLARE", "SET", "EXEC", "IF", "BEGIN", "END", "WHILE", "RETURN",
    "CREATE", "ALTER", "DROP", "TRUNCATE TABLE", "PRINT", "USE", "MERGE", "BEGIN TRAN", "COMMIT", "ROLLBACK", "RAISERROR",
    "THROW", "WAITFOR", "OPEN", "FETCH", "CLOSE", "DEALLOCATE", "GRANT", "REVOKE", "DENY", "DBCC", "BREAK", "CONTINUE",
];

/// What may follow a complete operand in a condition.
const CONDITION_NEXT: &[&str] = &["AND", "OR", "IS", "IN", "LIKE", "BETWEEN", "NOT"];

const CASE_NEXT: &[&str] = &["THEN", "WHEN", "ELSE", "END", "AND", "OR", "IS", "IN", "LIKE", "BETWEEN", "NOT"];

/// What may follow an operand that is not yet compared: `WHERE Id |`.
const OPERAND_NEXT: &[&str] = &["IN", "IS NULL", "IS NOT NULL", "LIKE", "BETWEEN", "NOT IN", "NOT LIKE", "NOT BETWEEN"];

/// How Tab or Enter writes a word that takes something after it; `$CURSOR$` is where the caret goes.
/// Other keys that insert a suggestion write the word alone.
fn shaped(label: &str, kind: &str, context: Context) -> Option<String> {
    let s = match (kind, label) {
        ("keyword", "IN" | "NOT IN" | "EXISTS" | "NOT EXISTS" | "VALUES") if context != Context::Keywords || label != "EXISTS" => format!("{label} ($CURSOR$)"),
        ("keyword", "LIKE" | "NOT LIKE") => format!("{label} '$CURSOR$'"),
        ("keyword", "BETWEEN" | "NOT BETWEEN") => format!("{label} $CURSOR$ AND "),
        ("keyword", "OVER") => "OVER ($CURSOR$)".to_string(),
        // Types that take a length or a precision: `NVARCHAR(|)`.
        ("type", "VARCHAR" | "NVARCHAR" | "CHAR" | "NCHAR" | "VARBINARY" | "BINARY" | "DECIMAL" | "NUMERIC") => format!("{label}($CURSOR$)"),
        _ => return None,
    };
    Some(s)
}

/// A literal of the type, the caret inside its quotes: N'' for Unicode text, '' for other text,
/// dates, times and GUIDs; none for numbers.
fn literal_for(data_type: &str) -> Option<&'static str> {
    let base = data_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "nvarchar" | "nchar" | "ntext" | "sysname" => Some("N'$CURSOR$'"),
        "varchar" | "char" | "text" | "date" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" | "time" | "uniqueidentifier" | "xml" => Some("'$CURSOR$'"),
        _ => None,
    }
}

/// What may follow a comparison: `WHERE Id = 1 |`.
const AFTER_COMPARISON: &[&str] = &["AND", "OR"];

/// What may follow a condition in WHERE, ON or HAVING, besides more of it.
const AFTER_CONDITION: &[&str] = &["GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "EXCEPT", "INTERSECT", "OPTION"];

const AFTER_JOIN_CONDITION: &[&str] = &[
    "WHERE", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "CROSS APPLY", "OUTER APPLY",
];

const CREATE_WHAT: &[&str] = &[
    "TABLE", "PROCEDURE", "VIEW", "FUNCTION", "INDEX", "NONCLUSTERED INDEX", "CLUSTERED INDEX", "UNIQUE INDEX",
    "TRIGGER", "OR ALTER", "SCHEMA", "TYPE", "SYNONYM", "SEQUENCE", "STATISTICS", "DATABASE", "LOGIN", "USER", "ROLE",
];

const ALTER_WHAT: &[&str] = &[
    "TABLE", "PROCEDURE", "VIEW", "FUNCTION", "INDEX", "TRIGGER", "DATABASE", "SCHEMA", "SEQUENCE", "LOGIN", "USER", "ROLE",
];

const DROP_WHAT: &[&str] = &[
    "TABLE", "PROCEDURE", "VIEW", "FUNCTION", "INDEX", "TRIGGER", "SCHEMA", "TYPE", "SYNONYM", "SEQUENCE", "STATISTICS",
    "DATABASE", "LOGIN", "USER", "ROLE",
];

const BEGIN_NEXT: &[&str] = &["TRAN", "TRANSACTION", "TRY", "CATCH", "DISTRIBUTED TRANSACTION"];

const SET_OPTIONS: &[&str] = &[
    "NOCOUNT ON", "XACT_ABORT ON", "TRANSACTION ISOLATION LEVEL", "STATISTICS IO ON", "STATISTICS TIME ON",
    "IDENTITY_INSERT", "ANSI_NULLS ON", "QUOTED_IDENTIFIER ON", "ROWCOUNT", "LOCK_TIMEOUT", "DEADLOCK_PRIORITY",
    "DATEFORMAT", "DATEFIRST", "LANGUAGE", "ARITHABORT ON", "SHOWPLAN_XML ON", "STATISTICS XML ON",
];

const TABLE_HINTS: &[&str] = &[
    "NOLOCK", "READUNCOMMITTED", "UPDLOCK", "ROWLOCK", "HOLDLOCK", "READPAST", "TABLOCK", "TABLOCKX", "PAGLOCK",
    "XLOCK", "NOWAIT", "SERIALIZABLE", "REPEATABLEREAD", "READCOMMITTED", "READCOMMITTEDLOCK", "INDEX", "FORCESEEK",
    "FORCESCAN", "NOEXPAND", "SNAPSHOT",
];

const QUERY_HINTS: &[&str] = &[
    "RECOMPILE", "MAXDOP", "OPTIMIZE FOR UNKNOWN", "OPTIMIZE FOR", "FORCE ORDER", "HASH JOIN", "LOOP JOIN",
    "MERGE JOIN", "HASH GROUP", "ORDER GROUP", "MAXRECURSION", "KEEPFIXED PLAN", "FAST", "USE HINT", "QUERYTRACEON",
];

/// Data types listed first, the ones most written.
const COMMON_TYPES: &[&str] = &[
    "INT", "NVARCHAR", "VARCHAR", "DATETIME", "DATETIME2", "DATE", "BIT", "DECIMAL", "BIGINT", "UNIQUEIDENTIFIER",
    "MONEY", "NUMERIC", "FLOAT", "CHAR", "NCHAR", "TINYINT", "SMALLINT", "TIME", "VARBINARY", "XML",
];

/// Built-in functions listed first among those matching, the ones most called.
const COMMON_FUNCTIONS: &[&str] = &[
    "COUNT", "SUM", "MAX", "MIN", "AVG", "ISNULL", "COALESCE", "CAST", "CONVERT", "GETDATE", "DATEADD", "DATEDIFF",
    "LEN", "SUBSTRING", "REPLACE", "LTRIM", "RTRIM", "TRIM", "UPPER", "LOWER", "ROW_NUMBER", "CONCAT", "FORMAT", "IIF",
    "NULLIF", "ROUND", "ABS", "YEAR", "MONTH", "DAY", "DATEPART", "DATENAME", "EOMONTH", "SYSDATETIME", "GETUTCDATE",
    "NEWID", "STRING_AGG", "STRING_SPLIT", "CHARINDEX", "LEFT", "RIGHT", "STUFF", "RANK", "DENSE_RANK", "LAG", "LEAD",
    "TRY_CAST", "TRY_CONVERT", "OBJECT_ID", "OBJECT_NAME", "DB_NAME", "SCOPE_IDENTITY", "COUNT_BIG",
];

/// Functions that aggregate the rows of a group.
const AGGREGATES: &[&str] = &[
    "COUNT", "COUNT_BIG", "SUM", "AVG", "MIN", "MAX", "STRING_AGG", "STDEV", "STDEVP", "VAR", "VARP", "GROUPING",
    "GROUPING_ID", "CHECKSUM_AGG", "APPROX_COUNT_DISTINCT", "APPROX_PERCENTILE_CONT", "APPROX_PERCENTILE_DISC",
];

/// What comes next at the caret when only certain words can: keywords, or a data type.
enum Next {
    /// Keywords, most likely first; `true` adds the declared variables (after `SET`).
    Words(Vec<&'static str>, bool),
    /// Data types, then these words.
    Types(Vec<&'static str>),
    /// The columns of the table named by tokens `[from, to)`.
    Columns(usize, usize),
    /// Tables and views.
    Tables,
    /// Any object a permission is granted on.
    Securables,
}

const ALTER_TABLE_NEXT: &[&str] = &[
    "ADD", "ALTER COLUMN", "DROP COLUMN", "ADD CONSTRAINT", "DROP CONSTRAINT", "WITH CHECK ADD CONSTRAINT", "NOCHECK CONSTRAINT",
    "CHECK CONSTRAINT", "ENABLE TRIGGER", "DISABLE TRIGGER", "REBUILD", "SWITCH TO", "SET",
];

const CONSTRAINT_KINDS: &[&str] = &["PRIMARY KEY", "FOREIGN KEY", "UNIQUE", "CHECK", "DEFAULT"];

/// What may follow a column's data type in `CREATE TABLE`.
const COLUMN_OPTIONS: &[&str] = &[
    "NOT NULL", "NULL", "PRIMARY KEY", "IDENTITY(1,1)", "DEFAULT", "UNIQUE", "CONSTRAINT", "REFERENCES", "CHECK", "COLLATE",
];

const PERMISSIONS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "EXECUTE", "ALTER", "CONTROL", "VIEW DEFINITION", "REFERENCES", "TAKE OWNERSHIP",
];

const MERGE_WHEN: &[&str] = &["MATCHED", "NOT MATCHED", "NOT MATCHED BY SOURCE", "NOT MATCHED BY TARGET"];

/// The tokens `[at, end)` of the (multipart) name starting at `at`.
fn name_range(head: &[Token], at: usize, limit: usize) -> (usize, usize) {
    let mut j = at;
    if j < limit && (is_name(&head[j]) || head[j].kind == Kind::Variable) {
        j += 1;
        while j + 1 < limit && head[j].kind == Kind::Dot && is_name(&head[j + 1]) {
            j += 2;
        }
    }
    (at, j)
}

/// Suggestions inside DDL, permissions and MERGE: what `ALTER TABLE t`, `CREATE INDEX ... ON t (`,
/// `GRANT ... ON`, a column of `CREATE TABLE` or `WHEN` in MERGE go on with.
fn ddl_words(head: &[Token], k: usize, lo: usize, p: usize) -> Option<Next> {
    let prev = |j: usize| (lo..j).rev().find(|&i| !head[i].is_comment());
    let next = |j: usize| (j + 1..k).find(|&i| !head[i].is_comment());
    let word = |i: Option<usize>| i.filter(|&i| head[i].kind == Kind::Word).map(|i| head[i].text.to_ascii_uppercase()).unwrap_or_default();
    let words = |w: &[&'static str]| Some(Next::Words(w.to_vec(), false));
    let first_at = (lo..k).find(|&i| !head[i].is_comment())?;
    let first = word(Some(first_at));
    let second = next(first_at);
    let (pw, qw) = (word(Some(p)), word(prev(p)));
    let open = opener(head, lo, p + 1);
    let before_open = open.and_then(prev);
    // The name before `(` at `o`, as a token range.
    let name_before = |o: usize| -> Option<(usize, usize)> {
        let mut j = prev(o)?;
        if !is_name(&head[j]) {
            return None;
        }
        while j >= 2 && head[j - 1].kind == Kind::Dot && is_name(&head[j - 2]) {
            j -= 2;
        }
        Some((j, o))
    };

    match first.as_str() {
        "ALTER" if word(second) == "TABLE" => {
            let (ts, te) = name_range(head, next(second?)?, k);
            if te == ts {
                return None;
            }
            if p + 1 == te {
                return words(ALTER_TABLE_NEXT);
            }
            match pw.as_str() {
                "COLUMN" if matches!(qw.as_str(), "ALTER" | "DROP") => return Some(Next::Columns(ts, te)),
                "ADD" => return words(&["CONSTRAINT", "PRIMARY KEY", "FOREIGN KEY", "UNIQUE", "CHECK", "DEFAULT", "INDEX"]),
                "CONSTRAINT" => return words(&[]),
                "ENABLE" | "DISABLE" => return words(&["TRIGGER"]),
                "REFERENCES" => return Some(Next::Tables),
                "PRIMARY" | "FOREIGN" => return words(&["KEY"]),
                _ => {}
            }
            if is_name(&head[p]) && qw == "CONSTRAINT" && word(prev(p).and_then(prev)) == "ADD" {
                return words(CONSTRAINT_KINDS);
            }
            if matches!(head[p].kind, Kind::LParen | Kind::Comma)
                && let Some(o) = open
            {
                if matches!(word(before_open).as_str(), "KEY" | "UNIQUE" | "CLUSTERED" | "NONCLUSTERED") {
                    return Some(Next::Columns(ts, te));
                }
                // `REFERENCES other (`: the columns of the other table.
                let (s, e) = name_before(o)?;
                return (word(prev(s)) == "REFERENCES").then_some(Next::Columns(s, e));
            }
            let key_list = |o: Option<usize>| o.and_then(prev).is_some_and(|b| word(Some(b)) == "KEY") && word(o.and_then(prev).and_then(prev)) == "FOREIGN";
            if head[p].kind == Kind::RParen && key_list(opener(head, lo, p)) {
                return words(&["REFERENCES"]);
            }
            None
        }
        "CREATE" if (first_at..p).any(|i| word(Some(i)) == "INDEX") => {
            let on = (first_at..=p).find(|&i| word(Some(i)) == "ON")?;
            if p == on {
                return Some(Next::Tables);
            }
            let (ts, te) = name_range(head, next(on)?, k);
            if matches!(head[p].kind, Kind::LParen | Kind::Comma)
                && let Some(o) = open
                && (o == te || word(before_open) == "INCLUDE")
            {
                return Some(Next::Columns(ts, te));
            }
            if head[p].kind == Kind::RParen {
                return words(&["INCLUDE", "WHERE", "WITH"]);
            }
            None
        }
        "CREATE" | "DECLARE" if open.is_some() => {
            let o = open?;
            // Inside the column list of `CREATE TABLE t (` or `DECLARE @t TABLE (`.
            let b = before_open?;
            let table = word(Some(b)) == "TABLE" || (first == "CREATE" && word(name_before(o).and_then(|(s, _)| prev(s))) == "TABLE");
            if !table {
                return None;
            }
            let element = (o..p).rev().find(|&i| head[i].kind == Kind::Comma && opener(head, lo, i + 1) == Some(o)).unwrap_or(o);
            if p == element {
                return words(&["CONSTRAINT", "PRIMARY KEY", "UNIQUE", "INDEX", "FOREIGN KEY", "CHECK"]);
            }
            let starts = word(next(element));
            if matches!(starts.as_str(), "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "INDEX" | "FOREIGN" | "CHECK") {
                return match pw.as_str() {
                    "PRIMARY" | "FOREIGN" => words(&["KEY"]),
                    "REFERENCES" => Some(Next::Tables),
                    _ if qw == "CONSTRAINT" => words(CONSTRAINT_KINDS),
                    _ => None,
                };
            }
            // `Id int |`: the column's options; the type itself comes from next_words.
            if next(element).and_then(next).is_some_and(|ty| p >= ty) {
                return match pw.as_str() {
                    "NOT" => words(&["NULL"]),
                    "PRIMARY" => words(&["KEY"]),
                    "REFERENCES" => Some(Next::Tables),
                    "DEFAULT" | "IDENTITY" | "COLLATE" | "CHECK" => words(&[]),
                    _ if head[p].kind == Kind::LParen => None,
                    _ => words(COLUMN_OPTIONS),
                };
            }
            None
        }
        "GRANT" | "REVOKE" | "DENY" => {
            let on = (first_at..p).find(|&i| word(Some(i)) == "ON");
            let to = (first_at..p).any(|i| matches!(word(Some(i)).as_str(), "TO" | "FROM"));
            match pw.as_str() {
                "ON" => Some(Next::Securables),
                "TO" | "FROM" => words(&[]),
                _ if on.is_none() && (p == first_at || head[p].kind == Kind::Comma) => words(PERMISSIONS),
                _ if on.is_none() => words(&["ON", "TO"]),
                _ if !to && head[p].kind != Kind::Dot => words(&["TO"]),
                _ => None,
            }
        }
        "MERGE" => match pw.as_str() {
            "WHEN" => words(MERGE_WHEN),
            "NOT" if qw == "WHEN" => words(&["MATCHED"]),
            "MATCHED" => words(&["THEN", "AND", "BY SOURCE", "BY TARGET"]),
            "SOURCE" | "TARGET" if qw == "BY" => words(&["THEN", "AND"]),
            "THEN" => words(&["UPDATE SET", "DELETE", "INSERT"]),
            _ => None,
        },
        "UPDATE" if pw == "STATISTICS" => Some(Next::Tables),
        "DROP" if pw == "EXISTS" && qw == "IF" && matches!(word(second).as_str(), "TABLE" | "VIEW") => Some(Next::Tables),
        _ => None,
    }
}

/// The `(` still open at `to`, searching back to `from`.
fn opener(head: &[Token], from: usize, to: usize) -> Option<usize> {
    let mut depth = 0;
    for j in (from..to).rev() {
        match head[j].kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => return Some(j),
            Kind::LParen => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether token `i` ends a value: a name, literal, variable or closing parenthesis.
pub(crate) fn ends_operand(head: &[Token], i: usize) -> bool {
    match head[i].kind {
        Kind::Number | Kind::Str | Kind::Variable | Kind::RParen | Kind::Bracket | Kind::Quoted => true,
        // `SELECT *` and `t.*`: a `*` with no operand before it is every column, not a product.
        Kind::Op if head[i].text == "*" => {
            let before = (0..i).rev().find(|&j| !head[j].is_comment());
            before.is_none_or(|j| head[j].kind == Kind::Dot || !ends_operand(head, j))
        }
        Kind::Word => {
            let w = head[i].text.to_ascii_uppercase();
            !is_reserved(&w) || matches!(w.as_str(), "NULL" | "ASC" | "DESC" | "CURRENT_TIMESTAMP" | "CURRENT_USER" | "SESSION_USER" | "SYSTEM_USER")
        }
        _ => false,
    }
}

/// The words that can come at the caret when the syntax allows only some: after `INNER`, `IS`,
/// `ORDER`, `CREATE`, inside `WITH (`, at the start of a statement, or after a complete operand
/// (`c.Name |` goes on with an operator or a clause, not with another column). None leaves the
/// usual suggestions.
fn next_words(head: &[Token], k: usize, stmt_start: usize, clause: &str) -> Option<Next> {
    let lo = stmt_start.min(k);
    let prev = |j: usize| (lo..j).rev().find(|&i| !head[i].is_comment());
    let word = |i: Option<usize>| i.filter(|&i| head[i].kind == Kind::Word).map(|i| head[i].text.to_ascii_uppercase()).unwrap_or_default();
    let words = |w: &[&'static str]| Some(Next::Words(w.to_vec(), false));
    let Some(p) = prev(k) else { return words(STATEMENT_START) };
    let q = prev(p);
    let first_at = (lo..k).find(|&i| !head[i].is_comment()).unwrap_or(lo);
    let first = word(Some(first_at));
    let (pw, qw) = (word(Some(p)), word(q));
    let in_from = matches!(clause, "FROM" | "JOIN");
    let ups: Vec<String> = (lo..p).map(|i| if head[i].kind == Kind::Word { head[i].text.to_ascii_uppercase() } else { String::new() }).collect();
    let case_open = ups.iter().filter(|w| *w == "CASE").count() > ups.iter().filter(|w| *w == "END").count();
    let routine = matches!(first.as_str(), "CREATE" | "ALTER") && ups.iter().any(|w| matches!(w.as_str(), "PROC" | "PROCEDURE" | "FUNCTION" | "TRIGGER"));
    let routine_header = routine && !(lo..p).any(|i| word(Some(i)) == "AS" && opener(head, lo, i).is_none());
    let with_statements = |w: &[&'static str]| Some(Next::Words(w.iter().chain(STATEMENT_START).copied().collect(), false));

    if let Some(ddl) = ddl_words(head, k, lo, p) {
        return Some(ddl);
    }

    // `UPDATE t |` and `UPDATE TOP (10) t |` go on with SET.
    if is_name(&head[p]) && !(head[p].kind == Kind::Word && is_reserved(&pw)) {
        let mut j = p;
        while j >= lo + 2 && head[j - 1].kind == Kind::Dot && is_name(&head[j - 2]) {
            j -= 2;
        }
        let mut b = prev(j);
        if let Some(r) = b.filter(|&r| head[r].kind == Kind::RParen)
            && let Some(o) = opener(head, lo, r)
            && word(prev(o)) == "TOP"
        {
            b = prev(o).and_then(prev);
        }
        if word(b) == "UPDATE" {
            return words(&["SET", "WITH"]);
        }
    }

    match head[p].kind {
        Kind::Word => match pw.as_str() {
            "INNER" if in_from => return words(&["JOIN"]),
            "LEFT" | "RIGHT" | "FULL" if in_from => return words(&["JOIN", "OUTER JOIN"]),
            "OUTER" | "CROSS" if in_from => return words(&["JOIN", "APPLY"]),
            "ORDER" | "GROUP" | "PARTITION" => return words(&["BY"]),
            // INTO is optional: `INSERT dbo.t`; INTO is offered with the tables.
            "INSERT" if p == first_at => return Some(Next::Tables),
            "INSERT" => return words(&["INTO"]),
            "CREATE" => return words(CREATE_WHAT),
            "OR" if qw == "CREATE" => return words(&["ALTER"]),
            "ALTER" if p == first_at => return words(ALTER_WHAT),
            "DROP" if p == first_at => return words(DROP_WHAT),
            "DROP" if first == "ALTER" => return words(&["COLUMN", "CONSTRAINT"]),
            "IS" => return words(&["NULL", "NOT NULL"]),
            "NOT" if qw == "IS" => return words(&["NULL"]),
            "NOT" if q.is_some_and(|q| ends_operand(head, q)) => return words(&["IN", "LIKE", "BETWEEN"]),
            "BEGIN" => return with_statements(BEGIN_NEXT),
            "COMMIT" | "ROLLBACK" => return words(&["TRAN", "TRANSACTION"]),
            "TRAN" | "TRANSACTION" | "TRY" | "CATCH" | "ELSE" if !case_open => return words(STATEMENT_START),
            "END" if !case_open => return with_statements(&["TRY", "CATCH", "ELSE"]),
            "TOP" => return words(&[]),
            "UNION" => return words(&["ALL", "SELECT"]),
            "ALL" if qw == "UNION" => return words(&["SELECT"]),
            "EXCEPT" | "INTERSECT" => return words(&["SELECT"]),
            "SET" if p == first_at => return Some(Next::Words(SET_OPTIONS.to_vec(), true)),
            "AS" if opener(head, lo, p).is_some_and(|o| matches!(word(prev(o)).as_str(), "CAST" | "TRY_CAST")) => return Some(Next::Types(Vec::new())),
            "AS" if routine && opener(head, lo, p).is_none() => return words(STATEMENT_START),
            "AS" if matches!(first.as_str(), "CREATE" | "ALTER") && ups.iter().any(|w| w == "VIEW") && opener(head, lo, p).is_none() => return words(&["SELECT", "WITH"]),
            "FOR" if qw == "CURSOR" => return words(&["SELECT", "WITH"]),
            _ => {}
        },
        Kind::LParen => match word(prev(p)).as_str() {
            "WITH" => return words(TABLE_HINTS),
            "OPTION" => return words(QUERY_HINTS),
            "OVER" => return words(&["PARTITION BY", "ORDER BY"]),
            "EXISTS" => return words(&["SELECT"]),
            "IN" => return Some(Next::Words(vec!["SELECT"], true)),
            "CONVERT" | "TRY_CONVERT" => return Some(Next::Types(Vec::new())),
            _ => {}
        },
        Kind::Comma => {
            if let Some(o) = opener(head, lo, p) {
                match word(prev(o)).as_str() {
                    "WITH" => return words(TABLE_HINTS),
                    "OPTION" => return words(QUERY_HINTS),
                    _ => {}
                }
            }
        }
        Kind::Variable if first == "DECLARE" && (qw == "DECLARE" || q.is_some_and(|q| head[q].kind == Kind::Comma)) => {
            return Some(Next::Types(vec!["TABLE", "CURSOR FOR"]));
        }
        Kind::Variable if routine_header => return Some(Next::Types(Vec::new())),
        _ => {}
    }

    // A column being defined: `CREATE TABLE t (Id |` or `DECLARE @t TABLE (Id |`.
    if is_name(&head[p])
        && !(head[p].kind == Kind::Word && is_reserved(&pw))
        && q.is_some_and(|q| matches!(head[q].kind, Kind::LParen | Kind::Comma))
        && let Some(o) = opener(head, lo, p)
    {
        let before = prev(o);
        let table = word(before) == "TABLE"
            || (first == "CREATE" && before.and_then(prev).is_some_and(|b| word(Some(b)) == "TABLE" || head[b].kind == Kind::Dot));
        if table && matches!(first.as_str(), "CREATE" | "DECLARE") {
            return Some(Next::Types(Vec::new()));
        }
    }

    if !ends_operand(head, p) {
        return None;
    }
    // `TOP 10 |` and `TOP (10) |` go on with the select list (for a `)`, the `(` open before it is its own).
    if qw == "TOP" || (head[p].kind == Kind::RParen && opener(head, lo, p).is_some_and(|o| word(prev(o)) == "TOP")) {
        return None;
    }
    let mut out: Vec<&'static str> = Vec::new();
    if case_open {
        out.extend(CASE_NEXT);
    }
    if clause.is_empty()
        && let Some(o) = opener(head, lo, p + 1)
    {
        // Inside parentheses: a condition in brackets goes on as one; a function's arguments do not.
        let b = prev(o);
        let condition = b.is_none_or(|b| head[b].kind == Kind::LParen)
            || matches!(word(b).as_str(), "WHERE" | "AND" | "OR" | "NOT" | "ON" | "HAVING" | "WHEN" | "IF" | "WHILE");
        if condition && !case_open && !compares(head, lo, p) {
            return words(OPERAND_NEXT);
        }
        if condition {
            out.extend(if case_open { CONDITION_NEXT } else { AFTER_COMPARISON });
        }
        return if out.is_empty() { None } else { Some(Next::Words(out, false)) };
    }
    match clause {
        "WHERE" | "ON" | "HAVING" => {
            if !case_open && !compares(head, lo, p) {
                return words(OPERAND_NEXT);
            }
            if first == "MERGE" {
                out.extend(["WHEN MATCHED THEN", "WHEN NOT MATCHED THEN", "WHEN NOT MATCHED BY SOURCE THEN"]);
            }
            out.extend(if case_open { CONDITION_NEXT } else { AFTER_COMPARISON });
            if clause == "ON" {
                out.extend(AFTER_JOIN_CONDITION);
            }
            out.extend(AFTER_CONDITION);
        }
        "SELECT" => out.extend(["AS", "FROM", "INTO", "UNION", "UNION ALL", "ORDER BY"]),
        "ORDER" => out.extend(["ASC", "DESC", "OFFSET", "OPTION", "ROWS", "RANGE"]),
        "GROUP" => out.extend(["HAVING", "ORDER BY", "WITH ROLLUP", "UNION", "UNION ALL", "OPTION"]),
        "SET" if first == "UPDATE" => out.extend(["FROM", "WHERE", "OUTPUT"]),
        "INSERT" | "INTO" if head[p].kind == Kind::RParen => return words(&["VALUES", "SELECT", "OUTPUT", "EXEC", "DEFAULT VALUES"]),
        "SET" | "VALUES" | "" => {
            if matches!(first.as_str(), "IF" | "WHILE") {
                if !case_open && !compares(head, lo, p) {
                    return words(OPERAND_NEXT);
                }
                out.extend(if case_open { CONDITION_NEXT } else { AFTER_COMPARISON });
                out.push("BEGIN");
            }
        }
        _ => return None,
    }
    out.extend(STATEMENT_START);
    Some(Next::Words(out, false))
}

/// Whether the condition ending at token `p` compares something (`a = 1`, `a IN (...)`, `a IS NULL`,
/// `EXISTS (...)`, `(a = 1)`); `a` alone still needs its operator.
fn compares(head: &[Token], lo: usize, p: usize) -> bool {
    let word = |i: usize| if head[i].kind == Kind::Word { head[i].text.to_ascii_uppercase() } else { String::new() };
    let comparison = |i: usize| head[i].kind == Kind::Op && !head[i].text.is_empty() && head[i].text.chars().all(|c| matches!(c, '=' | '<' | '>' | '!'));
    let tests = |w: &str| matches!(w, "IN" | "IS" | "LIKE" | "BETWEEN" | "EXISTS");
    let starts = |w: &str| matches!(w, "WHERE" | "ON" | "HAVING" | "AND" | "OR" | "NOT" | "WHEN" | "IF" | "WHILE");
    let mut depth = 0;
    // A condition in parentheses: `(a = 1) |`.
    let mut grouped = false;
    for j in (lo..=p).rev() {
        match head[j].kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => break,
            Kind::LParen => depth -= 1,
            _ if head[j].is_comment() => {}
            _ if depth > 0 => grouped |= comparison(j) || tests(&word(j)),
            _ if comparison(j) || tests(&word(j)) => return true,
            _ => {
                let w = word(j);
                // `BETWEEN 1 AND 2`: the AND is the BETWEEN's.
                if w == "AND" {
                    let mut d = 0;
                    for i in (lo..j).rev() {
                        match head[i].kind {
                            Kind::RParen => d += 1,
                            Kind::LParen if d == 0 => break,
                            Kind::LParen => d -= 1,
                            _ if d > 0 => {}
                            _ if word(i) == "BETWEEN" => return true,
                            _ if starts(&word(i)) || comparison(i) => break,
                            _ => {}
                        }
                    }
                }
                if starts(&w) {
                    break;
                }
            }
        }
    }
    grouped
}

/// What a value at the caret is written for: the operand before a comparison (`c.Name = |`,
/// `@Id = |`) or before `IN (|`.
struct Compared {
    qualifier: Option<String>,
    name: String,
    variable: bool,
    list: bool,
}

fn compared_operand(head: &[Token], k: usize) -> Option<Compared> {
    let prev = |j: usize| (0..j).rev().find(|&i| !head[i].is_comment());
    let word = |i: usize, w: &str| head[i].kind == Kind::Word && head[i].text.eq_ignore_ascii_case(w);
    let op = prev(k)?;
    let list = head[op].kind == Kind::LParen;
    let mut l = if list {
        prev(prev(op).filter(|&i| word(i, "IN"))?)?
    } else if head[op].kind == Kind::Op && !head[op].text.is_empty() && head[op].text.chars().all(|c| matches!(c, '=' | '<' | '>' | '!')) {
        prev(op)?
    } else {
        return None;
    };
    if list && word(l, "NOT") {
        l = prev(l)?;
    }
    if head[l].kind == Kind::Variable {
        return Some(Compared { qualifier: None, name: head[l].text.clone(), variable: true, list });
    }
    let (qualifier, name) = operand_name(head, l)?;
    Some(Compared { qualifier, name, variable: false, list })
}

/// The column named at token `l`, with its qualifier: `c.Name`.
fn operand_name(head: &[Token], l: usize) -> Option<(Option<String>, String)> {
    if !is_name(&head[l]) || (head[l].kind == Kind::Word && is_reserved(&head[l].text.to_ascii_uppercase())) {
        return None;
    }
    let qualifier = (l >= 2 && head[l - 1].kind == Kind::Dot && is_name(&head[l - 2])).then(|| bare(&head[l - 2]));
    Some((qualifier, bare(&head[l])))
}

/// A column of the statement's sources, `c.Name` or `Name`, with its table.
fn column_in<'s>(sources: &[Source], qualifier: Option<&str>, name: &str, schema: &'s Schema) -> Option<(&'s DbObject, &'s Column)> {
    sources
        .iter()
        .filter(|s| qualifier.is_none_or(|q| eq(q, &s.reference) || s.parts.last().is_some_and(|p| eq(p, q))))
        .filter_map(|s| resolve(s, schema))
        .find_map(|o| o.columns.iter().find(|c| eq(&c.name, name)).map(|c| (o, c)))
}

/// The values the table's CHECK constraints allow the column: `([Status]='New' OR [Status]='Done')`
/// or `Status IN ('New', 'Done')` give `'New'` and `'Done'`.
fn check_values(o: &DbObject, column: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let literal = |t: &Token| matches!(t.kind, Kind::Str | Kind::Number);
    for definition in &o.checks {
        let (tokens, _) = lex_partial(definition);
        let t: Vec<&Token> = tokens.iter().filter(|t| !t.is_comment()).collect();
        let named = |i: usize| is_name(t[i]) && eq(&bare(t[i]), column);
        let mut found: Vec<&str> = Vec::new();
        for i in 0..t.len() {
            if i + 2 < t.len() && t[i + 1].text == "=" && ((named(i) && literal(t[i + 2])) || (literal(t[i]) && named(i + 2))) {
                found.push(if named(i) { &t[i + 2].text } else { &t[i].text });
            }
            if i + 2 < t.len() && named(i) && t[i + 1].text.eq_ignore_ascii_case("IN") && t[i + 2].kind == Kind::LParen {
                found.extend(t[i + 3..].iter().take_while(|t| t.kind != Kind::RParen).filter(|t| literal(t)).map(|t| t.text.as_str()));
            }
        }
        for v in found {
            if !out.iter().any(|o| o == v) {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// The data type's name without its length: `nvarchar(100)` is nvarchar.
fn base_type(data_type: &str) -> String {
    data_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// Whether the name before token `k` is an alias already written after a table: `t x` or `t AS x`.
fn alias_written(head: &[Token], k: usize) -> bool {
    let mut words = (0..k).rev().filter(|&j| !head[j].is_comment());
    let (Some(alias), Some(before)) = (words.next(), words.next()) else { return false };
    let named = |t: &Token| matches!(t.kind, Kind::Bracket | Kind::Quoted) || (t.kind == Kind::Word && !is_reserved(&t.text.to_ascii_uppercase()));
    named(&head[alias]) && (named(&head[before]) || head[before].kind == Kind::RParen || head[before].text.eq_ignore_ascii_case("AS"))
}

const GLOBAL_VARIABLES: &[&str] = &[
    "@@ERROR", "@@FETCH_STATUS", "@@IDENTITY", "@@ROWCOUNT", "@@SERVERNAME", "@@SPID",
    "@@TRANCOUNT", "@@VERSION",
];

pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

pub(crate) fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}

/// Identifier text without brackets or quotes.
pub(crate) fn bare(t: &Token) -> String {
    match t.kind {
        Kind::Bracket => t.text[1..t.text.len() - 1].replace("]]", "]"),
        Kind::Quoted => t.text[1..t.text.len() - 1].replace("\"\"", "\""),
        _ => t.text.clone(),
    }
}

pub(crate) fn quote(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '#' || c == '@')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '#' | '@' | '$'));
    if plain && !is_reserved(&name.to_ascii_uppercase()) {
        name.to_string()
    } else {
        format!("[{}]", name.replace(']', "]]"))
    }
}

pub(crate) fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub(crate) fn is_name(t: &Token) -> bool {
    matches!(t.kind, Kind::Word | Kind::Bracket | Kind::Quoted)
}

/// A table-like source visible in the statement around the caret.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    /// The name the query can refer to it by: its alias, or else its own name.
    pub reference: String,
    /// Multipart object name as written, or empty for a derived table.
    pub parts: Vec<String>,
    /// Columns inferred from a derived table or CTE.
    pub columns: Vec<String>,
    /// Token index of the source's first token.
    pub at: usize,
    /// Token index of the alias, if one is written.
    pub alias_at: Option<usize>,
}

pub(crate) struct Analysis {
    pub tokens: Vec<Token>,
    pub up: Vec<String>,
    /// Byte offset of each token's text.
    pub starts: Vec<usize>,
}

impl Analysis {
    pub fn new(text: &str) -> Self {
        let (tokens, _) = lex_partial(text);
        let up = upper_words(&tokens);
        let mut starts = Vec::with_capacity(tokens.len());
        let mut at = 0;
        for t in &tokens {
            at += t.ws.len();
            starts.push(at);
            at += t.text.len();
        }
        Analysis { tokens, up, starts }
    }

    pub fn matching(&self, open: usize, end: usize) -> usize {
        let mut depth = 0usize;
        for j in open..end {
            match self.tokens[j].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        end
    }

    /// Token range `[start, end)` of the statement containing byte offset `at`.
    pub fn statement_at(&self, at: usize) -> (usize, usize) {
        let explicit = |i: usize| matches!(self.tokens[i].kind, Kind::Semi | Kind::Go | Kind::Raw);
        let mut bounds: Vec<usize> = missing_terminators(&self.tokens);
        bounds.extend((0..self.tokens.len()).filter(|&i| explicit(i)));
        bounds.sort_unstable();
        bounds.dedup();
        let mut start = 0;
        for b in bounds {
            // An inferred end only lies before the caret once the next statement has begun;
            // until then the caret is still extending this one.
            let before = if explicit(b) {
                self.starts[b] + self.tokens[b].text.len() <= at
            } else {
                self.starts.get(b + 1).is_some_and(|&next| next < at)
            };
            if before {
                start = b + 1;
            } else {
                return (start, b + 1);
            }
        }
        (start, self.tokens.len())
    }

    /// The clause keyword governing token `k`, within its own parentheses.
    pub fn clause_before(&self, stmt_start: usize, k: usize) -> String {
        let mut depth = 0i32;
        for j in (stmt_start..k.min(self.tokens.len())).rev() {
            match self.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen => depth -= 1,
                Kind::Word if depth <= 0 && matches!(self.up[j].as_str(), "SELECT" | "FROM" | "WHERE" | "GROUP" | "ORDER" | "HAVING" | "SET" | "ON" | "VALUES" | "JOIN" | "INTO" | "INSERT") => {
                    return self.up[j].clone();
                }
                _ => {}
            }
            if depth < 0 {
                break;
            }
        }
        String::new()
    }

    /// Token ranges `[start, end)` of every statement, including their terminators.
    pub fn statements(&self) -> Vec<(usize, usize)> {
        let mut bounds: Vec<usize> = missing_terminators(&self.tokens);
        bounds.extend((0..self.tokens.len()).filter(|&i| matches!(self.tokens[i].kind, Kind::Semi | Kind::Go | Kind::Raw)));
        bounds.sort_unstable();
        bounds.dedup();
        let mut out = Vec::new();
        let mut start = 0;
        for b in bounds {
            if b >= start {
                out.push((start, b + 1));
                start = b + 1;
            }
        }
        if start < self.tokens.len() {
            out.push((start, self.tokens.len()));
        }
        out
    }

    /// Output column names of a SELECT list beginning at token `select`.
    pub fn select_columns(&self, select: usize, end: usize) -> Vec<String> {
        let mut columns = Vec::new();
        let mut depth = 0usize;
        let mut last_name: Option<String> = None;
        let mut j = select + 1;
        while j < end {
            let t = &self.tokens[j];
            match t.kind {
                Kind::LParen => depth += 1,
                Kind::RParen if depth == 0 => break,
                Kind::RParen => depth -= 1,
                Kind::Comma if depth == 0 => {
                    columns.extend(last_name.take());
                }
                Kind::Word if depth == 0 && matches!(self.up[j].as_str(), "FROM" | "INTO" | "WHERE" | "UNION" | "GROUP" | "ORDER") => break,
                _ if depth == 0 && is_name(t) && !(t.kind == Kind::Word && is_reserved(&self.up[j])) => {
                    last_name = Some(bare(t));
                }
                _ if depth == 0 && t.kind == Kind::Op && t.text == "=" => {
                    // `alias = expression` names the column before the `=`.
                    if let Some(name) = last_name.clone() {
                        columns.push(name);
                        last_name = None;
                        while j + 1 < end && !matches!(self.tokens[j + 1].kind, Kind::Comma) && !(self.tokens[j + 1].kind == Kind::Word && self.up[j + 1] == "FROM") {
                            j += 1;
                        }
                    }
                }
                _ => {}
            }
            j += 1;
        }
        columns.extend(last_name);
        columns.retain(|c| c != "*");
        columns
    }

    /// The SELECT of the query that token `at` belongs to, before it at the same depth.
    pub fn select_before(&self, start: usize, at: usize) -> Option<usize> {
        let mut depth = 0i32;
        for j in (start..at).rev() {
            match self.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen if depth == 0 => return None,
                Kind::LParen => depth -= 1,
                Kind::Word if depth == 0 && self.up[j] == "SELECT" => return Some(j),
                _ => {}
            }
        }
        None
    }

    /// What GROUP BY must list for the select list beginning at token `select`: each expression
    /// with a column and without an aggregate, a window or a subquery, as written but without its
    /// alias (`c.Name AS n` is `c.Name`).
    pub fn grouping_expressions(&self, select: usize, end: usize) -> Vec<String> {
        let mut items: Vec<Vec<usize>> = vec![Vec::new()];
        let mut j = select + 1;
        while j < end && matches!(self.up[j].as_str(), "DISTINCT" | "ALL") {
            j += 1;
        }
        if j < end && self.up[j] == "TOP" {
            j += 1;
            j = if j < end && self.tokens[j].kind == Kind::LParen { self.matching(j, end) + 1 } else { j + 1 };
            while j < end && matches!(self.up[j].as_str(), "PERCENT" | "WITH" | "TIES") {
                j += 1;
            }
        }
        let mut depth = 0usize;
        while j < end {
            let t = &self.tokens[j];
            match t.kind {
                _ if t.is_comment() => {
                    j += 1;
                    continue;
                }
                Kind::LParen => depth += 1,
                Kind::RParen if depth == 0 => break,
                Kind::RParen => depth -= 1,
                Kind::Semi | Kind::Go => break,
                Kind::Comma if depth == 0 => {
                    items.push(Vec::new());
                    j += 1;
                    continue;
                }
                Kind::Word if depth == 0 && matches!(self.up[j].as_str(), "FROM" | "INTO" | "WHERE" | "GROUP" | "ORDER" | "HAVING" | "UNION" | "EXCEPT" | "INTERSECT" | "OPTION") => break,
                _ => {}
            }
            items.last_mut().expect("one at least").push(j);
            j += 1;
        }
        let reserved = |i: usize| self.tokens[i].kind == Kind::Word && is_reserved(&self.up[i]);
        let called = |item: &[usize], n: usize| item.get(n + 1).is_some_and(|&x| self.tokens[x].kind == Kind::LParen);
        let mut out: Vec<String> = Vec::new();
        for mut item in items {
            // `n = expression`, `expression AS n` and `expression n`.
            if item.len() > 2 && is_name(&self.tokens[item[0]]) && !reserved(item[0]) && self.tokens[item[1]].text == "=" {
                item.drain(..2);
            }
            let mut d = 0i32;
            let as_at = item.iter().position(|&i| {
                d += match self.tokens[i].kind { Kind::LParen => 1, Kind::RParen => -1, _ => 0 };
                d == 0 && self.up[i] == "AS"
            });
            if let Some(p) = as_at {
                item.truncate(p);
            } else if item.len() >= 2 {
                let (last, before) = (item[item.len() - 1], item[item.len() - 2]);
                let alias = is_name(&self.tokens[last])
                    && !reserved(last)
                    && !matches!(self.tokens[before].kind, Kind::Dot | Kind::Op | Kind::LParen)
                    && (!reserved(before) || self.up[before] == "END");
                if alias {
                    item.pop();
                }
            }
            let aggregate = (0..item.len()).any(|n| AGGREGATES.contains(&self.up[item[n]].as_str()) && called(&item, n));
            let nested = item.iter().any(|&i| matches!(self.up[i].as_str(), "OVER" | "SELECT"));
            let column = (0..item.len()).any(|n| is_name(&self.tokens[item[n]]) && !reserved(item[n]) && !called(&item, n));
            let star = item.last().is_some_and(|&i| self.tokens[i].text == "*");
            if aggregate || nested || !column || star {
                continue;
            }
            let mut text = String::new();
            for (n, &i) in item.iter().enumerate() {
                if n > 0 && !self.tokens[i].ws.is_empty() {
                    text.push(' ');
                }
                text.push_str(&self.tokens[i].text);
            }
            if !out.iter().any(|o| eq(o, &text)) {
                out.push(text);
            }
        }
        out
    }

    pub fn ctes(&self, start: usize, end: usize) -> Vec<Source> {
        let mut out = Vec::new();
        if start >= end || self.up[start] != "WITH" {
            return out;
        }
        let mut j = start + 1;
        while j < end {
            if !is_name(&self.tokens[j]) {
                break;
            }
            let name_at = j;
            let name = bare(&self.tokens[j]);
            j += 1;
            let mut columns = Vec::new();
            if j < end && self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                columns = (j + 1..close).filter(|&k| is_name(&self.tokens[k])).map(|k| bare(&self.tokens[k])).collect();
                j = close + 1;
            }
            if j < end && self.up[j] == "AS" {
                j += 1;
            }
            if j < end && self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                if columns.is_empty() && j + 1 < end && self.up[j + 1] == "SELECT" {
                    columns = self.select_columns(j + 1, close);
                }
                j = close + 1;
            }
            out.push(Source { reference: name.clone(), parts: vec![name], columns, at: name_at, alias_at: None });
            if j < end && self.tokens[j].kind == Kind::Comma {
                j += 1;
            } else {
                break;
            }
        }
        out
    }

    /// Tables, views, functions, CTEs and derived tables referenced in `[start, end)`.
    pub fn sources(&self, start: usize, end: usize) -> Vec<Source> {
        let mut out = self.ctes(start, end);
        let mut in_from = false;
        let mut j = start;
        while j < end {
            let w = self.up[j].as_str();
            let introduces = self.tokens[j].kind == Kind::Word
                && (matches!(w, "FROM" | "JOIN" | "APPLY" | "USING" | "INTO" | "UPDATE" | "DELETE")
                    || (w == "MERGE" && self.up.get(j + 1).is_some_and(|n| n != "JOIN")));
            let comma_in_from = in_from && self.tokens[j].kind == Kind::Comma;
            if self.tokens[j].kind == Kind::Word && matches!(w, "WHERE" | "GROUP" | "ORDER" | "HAVING" | "ON" | "SET" | "SELECT" | "UNION") {
                in_from = false;
            }
            if !(introduces || comma_in_from) {
                j += 1;
                continue;
            }
            in_from = matches!(w, "FROM" | "JOIN" | "APPLY") || comma_in_from;
            j += 1;
            if j < end && self.up[j] == "INTO" {
                j += 1;
            }
            if j >= end {
                break;
            }
            let mut source = Source { reference: String::new(), parts: Vec::new(), columns: Vec::new(), at: j, alias_at: None };
            if self.tokens[j].kind == Kind::LParen {
                let close = self.matching(j, end);
                if j + 1 < end && self.up[j + 1] == "SELECT" {
                    source.columns = self.select_columns(j + 1, close);
                }
                j = close + 1;
            } else if (is_name(&self.tokens[j]) && !(self.tokens[j].kind == Kind::Word && is_reserved(&self.up[j]))) || self.tokens[j].kind == Kind::Variable {
                source.parts.push(bare(&self.tokens[j]));
                j += 1;
                while j + 1 < end && self.tokens[j].kind == Kind::Dot {
                    // `db..table` leaves an empty schema part.
                    if self.tokens[j + 1].kind == Kind::Dot {
                        source.parts.push(String::new());
                        j += 1;
                        continue;
                    }
                    if !is_name(&self.tokens[j + 1]) {
                        break;
                    }
                    source.parts.push(bare(&self.tokens[j + 1]));
                    j += 2;
                }
                if j < end && self.tokens[j].kind == Kind::LParen {
                    j = self.matching(j, end) + 1;
                }
            } else {
                continue;
            }
            if j < end && self.up[j] == "AS" {
                j += 1;
            }
            if j < end && is_name(&self.tokens[j]) && !(self.tokens[j].kind == Kind::Word && (is_reserved(&self.up[j]) || matches!(self.up[j].as_str(), "WINDOW" | "APPLY"))) {
                source.reference = bare(&self.tokens[j]);
                source.alias_at = Some(j);
                j += 1;
            } else {
                source.reference = source.parts.last().cloned().unwrap_or_default();
            }
            if !source.reference.is_empty() {
                out.push(source);
            }
        }
        let ctes: Vec<Source> = out.iter().filter(|s| s.parts.len() == 1 && !s.columns.is_empty()).cloned().collect();
        for source in out.iter_mut().filter(|s| s.columns.is_empty() && s.parts.len() == 1) {
            if let Some(cte) = ctes.iter().find(|c| eq(&c.reference, &source.parts[0])) {
                source.columns = cte.columns.clone();
            }
        }
        out
    }

    /// True when token `k` sits directly inside the column list of `INSERT [INTO] name (`.
    fn in_insert_column_list(&self, k: usize) -> bool {
        let mut depth = 0usize;
        let mut open = None;
        for j in (0..k).rev() {
            match self.tokens[j].kind {
                Kind::RParen => depth += 1,
                Kind::LParen if depth == 0 => {
                    open = Some(j);
                    break;
                }
                Kind::LParen => depth -= 1,
                _ => {}
            }
        }
        let Some(mut j) = open else { return false };
        // Walk back over the multipart table name.
        let mut saw_name = false;
        while j > 0 {
            j -= 1;
            match self.tokens[j].kind {
                Kind::Word | Kind::Bracket | Kind::Quoted if !is_reserved(&self.up[j]) => saw_name = true,
                Kind::Dot => {}
                Kind::Word => return saw_name && matches!(self.up[j].as_str(), "INTO" | "INSERT"),
                _ => return false,
            }
        }
        false
    }

    /// Temporary tables and table variables the script defines: `CREATE TABLE #t (...)`,
    /// `DECLARE @t TABLE (...)` and `SELECT ... INTO #t FROM ...`.
    pub fn local_tables(&self, schema: Option<&Schema>) -> Vec<DbObject> {
        let n = self.tokens.len();
        let mut out: Vec<DbObject> = Vec::new();
        for j in 0..n {
            let temp = |i: usize| i < n && is_name(&self.tokens[i]) && self.tokens[i].text.starts_with('#');
            let defined = if self.up[j] == "TABLE" && j > 0 && self.up[j - 1] == "CREATE" && temp(j + 1) {
                Some((j + 1, j + 2))
            } else if self.tokens[j].kind == Kind::Variable && j > 0 && (self.up[j - 1] == "DECLARE" || self.tokens[j - 1].kind == Kind::Comma) && self.up.get(j + 1).is_some_and(|w| w == "TABLE") {
                Some((j, j + 2))
            } else {
                None
            };
            if let Some((name, open)) = defined {
                if open < n && self.tokens[open].kind == Kind::LParen {
                    let close = self.matching(open, n);
                    out.push(DbObject { schema: String::new(), name: self.tokens[name].text.clone(), kind: "table".into(), columns: self.column_definitions(open, close), ..DbObject::default() });
                }
                continue;
            }
            if self.up[j] == "INTO" && temp(j + 1) {
                let (start, end) = self.statement_at(self.starts[j]);
                let Some(select) = (start..j).rev().find(|&i| self.up[i] == "SELECT") else { continue };
                let names = self.select_columns(select, end);
                let mut columns: Vec<Column> = names.iter().map(|c| Column { name: c.clone(), ..Column::default() }).collect();
                // `SELECT *` (or `t.*`) takes the columns of the sources.
                let star = (select..j).any(|i| self.tokens[i].kind == Kind::Op && self.tokens[i].text == "*");
                if star && let Some(schema) = schema {
                    for source in self.sources(start, end).iter().filter(|s| s.at > j) {
                        for (name, ty) in source_columns(source, Some(schema)) {
                            if !columns.iter().any(|c| eq(&c.name, &name)) {
                                columns.push(Column { name, data_type: ty, ..Column::default() });
                            }
                        }
                    }
                }
                out.push(DbObject { schema: String::new(), name: self.tokens[j + 1].text.clone(), kind: "table".into(), columns, ..DbObject::default() });
            }
        }
        out
    }

    /// The columns defined between `(` at `open` and `)` at `close`: `Id int NOT NULL, Code varchar(10)`.
    fn column_definitions(&self, open: usize, close: usize) -> Vec<Column> {
        // While the list is still being typed there is no `)`: it ends with the text.
        let close = close.min(self.tokens.len());
        let mut columns = Vec::new();
        let mut element = open + 1;
        let mut j = open + 1;
        while j <= close {
            if j == close || self.tokens[j].kind == Kind::Comma {
                if element < j && is_name(&self.tokens[element]) && !matches!(self.up[element].as_str(), "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "INDEX" | "CHECK" | "FOREIGN" | "PERIOD") {
                    let mut ty = self.tokens.get(element + 1).filter(|_| element + 1 < j).map(|t| t.text.to_ascii_lowercase()).unwrap_or_default();
                    if element + 2 < j && self.tokens[element + 2].kind == Kind::LParen {
                        let end = self.matching(element + 2, close);
                        ty.push_str(&self.tokens[element + 2..=end.min(j - 1)].iter().map(|t| t.text.as_str()).collect::<String>());
                    }
                    columns.push(Column { name: bare(&self.tokens[element]), data_type: ty, ..Column::default() });
                }
                element = j + 1;
            } else if self.tokens[j].kind == Kind::LParen {
                j = self.matching(j, close);
                continue;
            }
            j += 1;
        }
        columns
    }

    pub fn declared_variables(&self, before: usize) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for j in 0..self.tokens.len() {
            if self.starts[j] >= before {
                break;
            }
            if self.tokens[j].kind == Kind::Variable && !self.tokens[j].text.starts_with("@@") {
                let ty = self.up.get(j + 1).filter(|_| self.tokens[j + 1].kind == Kind::Word).cloned().unwrap_or_default();
                let name = self.tokens[j].text.clone();
                if !out.iter().any(|(n, _)| eq(n, &name)) {
                    out.push((name, ty.to_lowercase()));
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Context {
    TableSource,
    Procedure,
    Expression,
    InsertColumns,
    /// After `INSERT INTO name `: offer the column list and VALUES.
    InsertTarget,
    /// After `EXEC name `: offer the parameters.
    ExecArgs,
    /// After `ALTER VIEW` or `DROP VIEW`.
    Views,
    /// After `ALTER FUNCTION` or `DROP FUNCTION`.
    Functions,
    /// After a table and its alias: the next clause, or a new statement.
    AfterSource,
    /// Only certain keywords can come next.
    Keywords,
    /// A data type comes next.
    Types,
    /// The columns of one table, named in the statement (`ALTER TABLE t ALTER COLUMN`, `ON t (`).
    TableColumns,
    /// Any object, for `GRANT ... ON`.
    Securables,
    Nothing,
}

/// A literal of the right type to fill in a generated parameter or value list.
pub(crate) fn sample_value(data_type: &str) -> &'static str {
    let base = data_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "bigint" | "int" | "smallint" | "tinyint" | "bit" | "money" | "smallmoney" => "0",
        "decimal" | "numeric" | "float" | "real" => "0.0",
        "char" | "varchar" | "text" => "''",
        "nchar" | "nvarchar" | "ntext" | "sysname" => "N''",
        "date" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" | "time" => "'1900-01-01'",
        "uniqueidentifier" => "'00000000-0000-0000-0000-000000000000'",
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => "0x",
        _ => "NULL",
    }
}

/// The column list and a VALUES row of an INSERT, one column a line: the column's DEFAULT where it
/// has one, NULL where it may be null, else a value of its type; a comment names each column.
pub(crate) fn insert_body(cols: &[&Column], indent: &str, nl: &str) -> String {
    let mut body = format!("{nl}{indent}({nl}");
    for (i, c) in cols.iter().enumerate() {
        let comma = if i + 1 < cols.len() { "," } else { "" };
        body.push_str(&format!("{indent}    {}{comma}{nl}", quote(&c.name)));
    }
    body.push_str(&format!("{indent}){nl}{indent}VALUES{nl}{indent}({nl}"));
    for (i, c) in cols.iter().enumerate() {
        let comma = if i + 1 < cols.len() { "," } else { "" };
        let (value, note) = match &c.default {
            Some(d) => ("DEFAULT", format!(" (default {})", unwrap_parens(d))),
            None if c.nullable => ("NULL", String::new()),
            None => (sample_value(&c.data_type), String::new()),
        };
        body.push_str(&format!("{indent}    {value}{comma} -- {} - {}{note}{nl}", c.name, c.data_type));
    }
    body.push_str(&format!("{indent})"));
    body
}

/// `((0))` and `(getdate())` as `0` and `getdate()`.
fn unwrap_parens(definition: &str) -> &str {
    let mut d = definition.trim();
    while d.len() >= 2 && d.starts_with('(') && d.ends_with(')') {
        // Only parentheses that wrap the whole text: `(a) + (b)` keeps its own.
        let inner = &d[1..d.len() - 1];
        let mut depth = 0i32;
        if inner.chars().any(|ch| {
            depth += match ch { '(' => 1, ')' => -1, _ => 0 };
            depth < 0
        }) {
            break;
        }
        d = inner.trim();
    }
    d
}

/// A short alias from a name's initials (`OrderLines` -> `ol`), unique among `taken`.
/// The alias for a table picked in a suggestion: the user's own for that name, or else one made
/// from the name without its prefix.
fn alias_with(name: &str, taken: &[String], options: &CompleteOptions) -> String {
    let free = |a: &str| !is_reserved(&a.to_ascii_uppercase()) && !taken.iter().any(|t| eq(t, a));
    if let Some(own) = options.aliases.iter().find(|(n, _)| eq(n, name)).map(|(_, a)| a.as_str()).filter(|a| !a.is_empty()) {
        if free(own) {
            return own.to_string();
        }
        return (2..).map(|n| format!("{own}{n}")).find(|a| free(a)).expect("unbounded");
    }
    let bare = name.trim_start_matches('#');
    let stripped = options
        .ignored_prefixes
        .iter()
        .filter(|p| !p.is_empty() && bare.len() > p.len() && bare.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
        .map(|p| &bare[p.len()..])
        .min_by_key(|rest| rest.len())
        .unwrap_or(bare);
    alias_for(stripped, taken)
}

pub(crate) fn alias_for(name: &str, taken: &[String]) -> String {
    let mut base = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let starts_word = match prev {
            None => true,
            Some(p) => (c.is_uppercase() && p.is_lowercase()) || (!p.is_alphanumeric() && c.is_alphanumeric()),
        };
        if starts_word && c.is_alphanumeric() {
            base.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    if base.is_empty() || !base.chars().next().is_some_and(char::is_alphabetic) {
        base.insert(0, 't');
    }
    let free = |a: &str| !is_reserved(&a.to_ascii_uppercase()) && !taken.iter().any(|t| eq(t, a));
    if free(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}{n}")).find(|a| free(a)).expect("unbounded")
}

/// Join conditions between a newly joined source and an earlier one, from foreign keys,
/// or else from identically named `...Id` columns. The flag is true for foreign keys.
fn join_conditions(new: (&str, &DbObject), old: (&str, &DbObject), schema: &Schema) -> Vec<(String, bool)> {
    let (nr, no) = new;
    let (or, oo) = old;
    let same = |s: &str, t: &str, o: &DbObject| eq(s, &o.schema) && eq(t, &o.name);
    let mut out = Vec::new();
    for fk in &schema.foreign_keys {
        let pairs: Vec<(String, String)> = if same(&fk.schema, &fk.table, no) && same(&fk.ref_schema, &fk.ref_table, oo) {
            fk.columns.iter().zip(&fk.ref_columns).map(|(a, b)| (a.clone(), b.clone())).collect()
        } else if same(&fk.schema, &fk.table, oo) && same(&fk.ref_schema, &fk.ref_table, no) {
            fk.ref_columns.iter().zip(&fk.columns).map(|(a, b)| (a.clone(), b.clone())).collect()
        } else {
            continue;
        };
        let cond = pairs.iter().map(|(a, b)| format!("{}.{} = {}.{}", quote(nr), quote(a), quote(or), quote(b))).collect::<Vec<_>>().join(" AND ");
        if !cond.is_empty() && !out.iter().any(|(c, _)| *c == cond) {
            out.push((cond, true));
        }
    }
    if out.is_empty() {
        // Not each table's own key: `Id = Id` joins nothing.
        let own_key = |o: &DbObject, name: &str| eq(name, "Id") || key_column(o).is_some_and(|k| eq(&k, name));
        for c in no.columns.iter().filter(|c| c.name.to_ascii_lowercase().ends_with("id") && !(own_key(no, &c.name) && own_key(oo, &c.name))) {
            if oo.columns.iter().any(|o| eq(&o.name, &c.name)) {
                out.push((format!("{}.{} = {}.{}", quote(nr), quote(&c.name), quote(or), quote(&c.name)), false));
            }
        }
        // A column named for the other table and that table's key: `o.CustomerId = c.Id`.
        let mut guess = |to: &DbObject, from: &DbObject, flip: bool| {
            let Some(key) = key_column(to) else { return };
            for c in from.columns.iter().filter(|c| !eq(&c.name, &key) && names_for(&to.name).iter().any(|n| eq(&c.name, &format!("{n}Id")) || eq(&c.name, &format!("{n}_Id")))) {
                let cond = if flip {
                    format!("{}.{} = {}.{}", quote(nr), quote(&key), quote(or), quote(&c.name))
                } else {
                    format!("{}.{} = {}.{}", quote(nr), quote(&c.name), quote(or), quote(&key))
                };
                if !out.iter().any(|(x, _)| *x == cond) {
                    out.push((cond, false));
                }
            }
        };
        guess(oo, no, false);
        guess(no, oo, true);
    }
    out
}

/// The column a reference to the table names: its one-column primary key, or else `Id`.
fn key_column(o: &DbObject) -> Option<String> {
    let keys: Vec<&Column> = o.columns.iter().filter(|c| c.key).collect();
    match keys.as_slice() {
        [k] => Some(k.name.clone()),
        [] => o.columns.iter().find(|c| eq(&c.name, "Id")).map(|c| c.name.clone()),
        _ => None,
    }
}

/// What a column referring to the table starts with: its name without a prefix, and its singular
/// (`tblCustomers`: Customers, Customer; `Categories`: Category).
fn names_for(table: &str) -> Vec<String> {
    let lower = table.to_ascii_lowercase();
    let bare = ["tbl", "tb_", "t_"].iter().find(|p| lower.starts_with(*p) && lower.len() > p.len()).map_or(table, |p| &table[p.len()..]);
    let lower = bare.to_ascii_lowercase();
    let mut out = vec![bare.to_string()];
    if let Some(stem) = lower.strip_suffix("ies") {
        out.push(format!("{}y", &bare[..stem.len()]));
    }
    if lower.ends_with("es") {
        out.push(bare[..bare.len() - 2].to_string());
    }
    if lower.ends_with('s') {
        out.push(bare[..bare.len() - 1].to_string());
    }
    out
}

/// `@a = 0, -- int` lines for a procedure call, aligned under the first parameter.
fn parameter_list(params: &[Param], align: &str, nl: &str) -> String {
    let mut out = String::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(nl);
            out.push_str(align);
        }
        let value = if p.output { p.name.clone() } else { sample_value(&p.data_type).to_string() };
        out.push_str(&format!("{} = {}{}", p.name, value, if p.output { " OUTPUT" } else { "" }));
        out.push_str(if i + 1 < params.len() { ", " } else { " " });
        out.push_str(&format!("-- {}", p.data_type));
    }
    out
}

fn writable(o: &DbObject) -> Vec<&Column> {
    o.columns
        .iter()
        .filter(|c| !c.identity && !c.computed && !matches!(c.data_type.as_str(), "timestamp" | "rowversion"))
        .collect()
}

pub(crate) fn object_matches(o: &DbObject, parts: &[String], schema: &Schema) -> bool {
    // `db.schema.name` must be in that database; a shorter name, in this one.
    let database = if parts.len() >= 3 { parts[parts.len() - 3].as_str() } else { "" };
    let in_database = if database.is_empty() || eq(database, &schema.database) { o.database.is_empty() } else { eq(&o.database, database) };
    if !in_database {
        return false;
    }
    match parts {
        // System procedures (sp_who2) are called without their schema, from any database.
        [name] => eq(&o.name, name) && (eq(&o.schema, &schema.default_schema) || schema.default_schema.is_empty() || (o.kind == "procedure" && eq(&o.schema, "sys"))),
        [.., s, name] if s.is_empty() => eq(&o.name, name),
        [.., s, name] => eq(&o.name, name) && eq(&o.schema, s),
        [] => false,
    }
}

pub(crate) fn resolve<'s>(source: &Source, schema: &'s Schema) -> Option<&'s DbObject> {
    let exact = schema.objects.iter().find(|o| object_matches(o, &source.parts, schema));
    // An unqualified name falls back to any schema, as SQL Server resolves it via the default schema first.
    exact.or_else(|| match source.parts.as_slice() {
        [name] => schema.objects.iter().find(|o| eq(&o.name, name) && o.database.is_empty()),
        _ => None,
    })
}

pub(crate) fn source_columns(source: &Source, schema: Option<&Schema>) -> Vec<(String, String)> {
    if !source.columns.is_empty() {
        return source.columns.iter().map(|c| (c.clone(), String::new())).collect();
    }
    schema
        .and_then(|s| resolve(source, s))
        .map(|o| o.columns.iter().map(|c| (c.name.clone(), c.data_type.clone())).collect())
        .unwrap_or_default()
}

fn object_kind(kind: &str) -> &'static str {
    match kind {
        "table" => "table",
        "view" => "view",
        "procedure" => "procedure",
        "scalar_function" => "function",
        "table_function" => "table_function",
        "synonym" => "synonym",
        _ => "object",
    }
}

fn object_item(o: &DbObject, schema: &Schema, qualified: bool) -> Item {
    let name = if qualified || eq(&o.schema, &schema.default_schema) || o.schema.is_empty() {
        quote(&o.name)
    } else {
        format!("{}.{}", quote(&o.schema), quote(&o.name))
    };
    let detail = match o.kind.as_str() {
        "procedure" | "scalar_function" | "table_function" if !o.params.is_empty() => format!(
            "{}.{} ({})",
            o.schema,
            o.name,
            o.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect::<Vec<_>>().join(", ")
        ),
        _ => format!("{}.{}", o.schema, o.name),
    };
    Item { label: o.name.clone(), insert: name, kind: object_kind(&o.kind), detail }
}

fn wants(context: Context, kind: &str) -> bool {
    match context {
        Context::TableSource => matches!(kind, "table" | "view" | "table_function" | "synonym"),
        Context::Procedure => matches!(kind, "procedure" | "synonym"),
        Context::Expression => matches!(kind, "scalar_function" | "table" | "view"),
        Context::Views => kind == "view",
        Context::Functions => matches!(kind, "scalar_function" | "table_function"),
        Context::Securables => kind != "synonym",
        Context::InsertColumns | Context::InsertTarget | Context::ExecArgs | Context::AfterSource | Context::Keywords | Context::Types | Context::TableColumns | Context::Nothing => false,
    }
}

/// Lower ranks sort first.
fn rank(kind: &str, context: Context) -> u8 {
    match (context, kind) {
        (_, "join" | "body" | "picker" | "value") => 0,
        (Context::Expression, "column") => 1,
        (Context::Expression, "alias") => 2,
        (Context::Expression, "variable") => 3,
        (Context::Expression, "function") => 4,
        (_, "parameter") => 1,
        (_, "join_guess") => 3,
        (_, "snippet") => 5,
        (_, "global_variable") => 8,
        (_, "keyword") => 9,
        (_, "schema" | "database" | "server") => 6,
        _ => 4,
    }
}

fn object_at(tokens: &[Token], from: usize, to: usize, schema: &Schema) -> Option<DbObject> {
    let parts: Vec<String> = tokens[from..to].iter().filter(|t| is_name(t)).map(bare).collect();
    let source = Source { reference: String::new(), parts, columns: Vec::new(), at: from, alias_at: None };
    resolve(&source, schema).cloned()
}

/// Choices the user can change in the options.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompleteOptions {
    /// Give a table picked after FROM or JOIN an alias: `Customers` becomes `Customers AS c`.
    pub assign_aliases: bool,
    /// Write `AS` before generated aliases.
    pub alias_with_as: bool,
    /// Insert objects with their schema (`dbo.Customers`) even in the default schema.
    pub qualify_object_names: bool,
    /// List sys and INFORMATION_SCHEMA views without typing their schema first.
    pub show_system_objects: bool,
    /// Prefixes left out of generated aliases: `tblCustomer` gets `c`, not `tc`.
    pub ignored_prefixes: Vec<String>,
    /// The alias to give these tables, by name: `Customers` → `cu`.
    pub aliases: std::collections::HashMap<String, String>,
    /// Put the suggestions picked often and lately first.
    pub learn_from_picks: bool,
    /// How often and lately each suggestion was picked, by [`crate::usage::item_key`]; set by the
    /// engine, not the options.
    #[serde(skip)]
    pub usage: std::collections::HashMap<String, u32>,
}

impl Default for CompleteOptions {
    fn default() -> Self {
        CompleteOptions {
            assign_aliases: true,
            alias_with_as: true,
            qualify_object_names: true,
            show_system_objects: false,
            ignored_prefixes: ["tbl", "vw", "tb_", "t_", "v_"].map(String::from).to_vec(),
            aliases: Default::default(),
            learn_from_picks: true,
            usage: Default::default(),
        }
    }
}

fn is_system(o: &DbObject) -> bool {
    o.schema.eq_ignore_ascii_case("sys") || o.schema.eq_ignore_ascii_case("INFORMATION_SCHEMA")
}

/// Built-in functions called without parentheses.
const NILADIC: &[&str] = &["CURRENT_TIMESTAMP", "CURRENT_USER", "SESSION_USER", "SYSTEM_USER", "USER", "CURRENT_DATE"];

pub fn complete(text: &str, caret_utf16: usize, schema: Option<&Schema>) -> Completion {
    complete_with(text, caret_utf16, schema, &[])
}

pub fn complete_with(text: &str, caret_utf16: usize, schema: Option<&Schema>, snippets: &[Snippet]) -> Completion {
    complete_full(text, caret_utf16, schema, snippets, CompleteOptions::default())
}

pub fn complete_full(text: &str, caret_utf16: usize, schema: Option<&Schema>, snippets: &[Snippet], options: CompleteOptions) -> Completion {
    complete_across(text, caret_utf16, schema, &[], snippets, options)
}

/// Like `complete_full`, with the metadata of other databases the script names (`Other.dbo.t`).
pub fn complete_across(text: &str, caret_utf16: usize, schema: Option<&Schema>, others: &[&Schema], snippets: &[Snippet], options: CompleteOptions) -> Completion {
    let caret = utf16_to_byte(text, caret_utf16);
    let none = |at: usize| Completion { start: byte_to_utf16(text, at), items: Vec::new(), selected: None };
    let before = &text[..caret];

    // Never suggest inside a string or comment.
    let (head, error) = lex_partial(before);
    let mut bracket_prefix: Option<(usize, String)> = None;
    if let Some((e, offset)) = &error {
        if e.what == "bracketed identifier" {
            bracket_prefix = Some((*offset, before[*offset + 1..].to_string()));
        } else {
            return none(caret);
        }
    }
    let trailing = before.len() - head.iter().map(|t| t.ws.len() + t.text.len()).sum::<usize>();
    let touching = trailing == 0 && bracket_prefix.is_none();
    if touching && head.last().is_some_and(|t| t.kind == Kind::LineComment) {
        return none(caret);
    }

    let a = Analysis::new(text);
    // Index of the last token before the caret in the full-text analysis.
    let mut k = head.len();
    let (prefix, prefix_start, variable) = match (&bracket_prefix, head.last()) {
        (Some((offset, p)), _) => (p.clone(), *offset, false),
        (None, Some(t)) if touching && matches!(t.kind, Kind::Word | Kind::Variable | Kind::Bracket) => {
            k -= 1;
            (bare(t), caret - t.text.len(), t.kind == Kind::Variable)
        }
        (None, Some(t)) if touching && t.kind == Kind::Number => return none(caret),
        _ => (String::new(), caret, false),
    };
    let variable = variable || (prefix.is_empty() && before.ends_with('@'));

    // `GO` alone on its line ends a batch (the lexer reads it as one): offering GOTO or GROUPING
    // right after it would let Enter replace it.
    if touching && head.last().is_some_and(|t| t.kind == Kind::Go) {
        return none(caret);
    }

    // A qualifier such as `t.` or `dbo.` right before the word.
    let mut qualifier: Vec<String> = Vec::new();
    while k >= 2 && head[k - 1].kind == Kind::Dot && is_name(&head[k - 2]) {
        qualifier.insert(0, bare(&head[k - 2]));
        k -= 2;
    }
    let prev_word = (0..k).rev().find(|&j| !head[j].is_comment()).map(|j| (head[j].kind, head[j].text.to_ascii_uppercase()));

    let (stmt_start, stmt_end) = a.statement_at(prefix_start);
    let sources = a.sources(stmt_start, stmt_end);

    let clause = a.clause_before(stmt_start, k);

    // `INSERT [INTO] name ` or `EXEC name ` with the caret after a space.
    let after_target = |intro: &[&str]| -> Option<usize> {
        if touching || k == 0 || !is_name(&head[k - 1]) || (head[k - 1].kind == Kind::Word && is_reserved(&head[k - 1].text.to_ascii_uppercase())) {
            return None;
        }
        let mut j = k - 1;
        while j >= 2 && head[j - 1].kind == Kind::Dot && is_name(&head[j - 2]) {
            j -= 2;
        }
        let w = head.get(j.checked_sub(1)?)?.text.to_ascii_uppercase();
        intro.contains(&w.as_str()).then_some(j)
    };
    let insert_target = after_target(&["INTO", "INSERT"]).filter(|_| prefix.is_empty() && qualifier.is_empty());
    // `EXEC name a, |` also goes on with the parameters not yet written.
    let exec_more = (prefix.is_empty() && qualifier.is_empty() && matches!(prev_word, Some((Kind::Comma, _))))
        .then(|| (stmt_start..k).find(|&i| !head[i].is_comment()))
        .flatten()
        .filter(|&i| matches!(head[i].text.to_ascii_uppercase().as_str(), "EXEC" | "EXECUTE") && opener(&head, i, k).is_none())
        .map(|i| i + 1);
    let exec_target = after_target(&["EXEC", "EXECUTE"]).filter(|_| prefix.is_empty() && qualifier.is_empty()).or(exec_more);

    let mut next = if variable || !qualifier.is_empty() || insert_target.is_some() || exec_target.is_some() {
        None
    } else {
        next_words(&head, k, stmt_start, &clause)
    };

    let context = match &prev_word {
        _ if variable => Context::Expression,
        _ if insert_target.is_some() => Context::InsertTarget,
        _ if exec_target.is_some() => Context::ExecArgs,
        _ if matches!(next, Some(Next::Words(..))) => Context::Keywords,
        _ if matches!(next, Some(Next::Types(_))) => Context::Types,
        _ if matches!(next, Some(Next::Columns(..))) => Context::TableColumns,
        _ if matches!(next, Some(Next::Tables)) => Context::TableSource,
        _ if matches!(next, Some(Next::Securables)) => Context::Securables,
        Some((Kind::Word, w)) if matches!(w.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO" | "INSERT" | "TABLE" | "MERGE" | "USING" | "APPLY") => Context::TableSource,
        Some((Kind::Word, w)) if matches!(w.as_str(), "EXEC" | "EXECUTE" | "PROC" | "PROCEDURE") => Context::Procedure,
        Some((Kind::Word, w)) if w == "VIEW" => Context::Views,
        Some((Kind::Word, w)) if w == "FUNCTION" => Context::Functions,
        Some((Kind::Comma, _)) if clause == "FROM" || clause == "JOIN" => Context::TableSource,
        Some((Kind::LParen | Kind::Comma, _)) if a.in_insert_column_list(k) => Context::InsertColumns,
        Some((Kind::Word, w)) if matches!(w.as_str(), "AS") && clause == "FROM" => Context::Nothing,
        Some((Kind::Word | Kind::Bracket | Kind::Quoted, w)) if !qualifier.is_empty() || is_reserved(w) => Context::Expression,
        // After a table and its alias comes the next clause or statement.
        Some((Kind::Word | Kind::Bracket | Kind::Quoted, _)) if (clause == "FROM" || clause == "JOIN") && alias_written(&head, k) => Context::AfterSource,
        // A name directly after a table name is an alias being typed.
        Some((Kind::Word | Kind::Bracket | Kind::Quoted | Kind::RParen, _)) if clause == "FROM" || clause == "JOIN" => Context::Nothing,
        _ => Context::Expression,
    };

    let mut items: Vec<Item> = Vec::new();
    let default = Schema::default();
    // Temporary tables and table variables of the script count as tables.
    let local = a.local_tables(schema);
    let merged: Schema;
    let meta = if local.is_empty() && others.is_empty() {
        schema.unwrap_or(&default)
    } else {
        let mut m = schema.cloned().unwrap_or_default();
        m.objects.extend(local);
        // Other databases' objects resolve names such as `Other.dbo.t` in FROM.
        for other in others {
            m.objects.extend(other.objects.iter().map(|o| DbObject { database: other.database.clone(), ..o.clone() }));
        }
        merged = m;
        &merged
    };
    let schema = Some(meta);
    // After a column, the tests its type and nullability make sense of come first: IS NULL goes last
    // for a column that cannot be null (unless an outer join can make it so), LIKE for one not text.
    if let Some(Next::Words(words, _)) = &mut next
        && words.as_slice() == OPERAND_NEXT
        && let Some(p) = (0..k).rev().find(|&j| !head[j].is_comment())
        && let Some((q, name)) = operand_name(&head, p)
        && let Some((_, c)) = column_in(&sources, q.as_deref(), &name, meta)
    {
        let outer = (stmt_start..stmt_end.min(a.up.len())).any(|j| matches!(a.up[j].as_str(), "LEFT" | "RIGHT" | "FULL" | "OUTER"));
        let text = matches!(base_type(&c.data_type).as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname");
        words.sort_by_key(|w| (!c.nullable && !outer && w.starts_with("IS ")) || (!text && w.contains("LIKE")));
    }
    // A value written for a column or a parameter (`c.CustomerId = |`, `EXEC p @CustomerId = |`): the
    // variables of that name come first.
    let in_exec = a.up.get(stmt_start).is_some_and(|w| matches!(w.as_str(), "EXEC" | "EXECUTE"));
    let compared = compared_operand(&head, k).filter(|c| !c.variable || in_exec);
    let favoured: Vec<(String, String)> = match &compared {
        // A parameter's name is a variable of the call, not one declared before it.
        Some(c) => a
            .declared_variables(if c.variable { a.starts.get(stmt_start).copied().unwrap_or(prefix_start) } else { prefix_start })
            .into_iter()
            .filter(|(n, _)| eq(n.trim_start_matches('@'), c.name.trim_start_matches('@')))
            .collect(),
        None => Vec::new(),
    };
    // Items listed first, before the written ones: these variables, the GROUP BY list, the columns
    // of an INSERT ... SELECT.
    let mut pinned: Vec<String> = favoured.iter().map(|(n, _)| n.clone()).collect();
    // In conditions, the columns of keys and indexes first.
    let mut roles: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let conditions = matches!(clause.as_str(), "WHERE" | "ON" | "HAVING");
    // Each source's table, found once rather than for each of its columns.
    let resolved: Vec<(&str, &DbObject)> = if conditions { sources.iter().filter_map(|s| resolve(s, meta).map(|o| (s.reference.as_str(), o))).collect() } else { Vec::new() };
    let role = |reference: &str, name: &str| -> Option<usize> {
        let o = resolved.iter().find(|(r, _)| eq(r, reference))?.1;
        let c = o.columns.iter().find(|c| eq(&c.name, name))?;
        let foreign = meta.foreign_keys.iter().any(|f| eq(&f.schema, &o.schema) && eq(&f.table, &o.name) && f.columns.iter().any(|x| eq(x, name)));
        if c.key {
            Some(0)
        } else if foreign {
            Some(1)
        } else if c.indexed {
            Some(2)
        } else {
            None
        }
    };
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = before[..prefix_start].rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..prefix_start].chars().count();
    let indent: String = before[line_start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let prev_upper = prev_word.as_ref().filter(|(kind, _)| *kind == Kind::Word).map(|(_, w)| w.as_str()).unwrap_or("");
    let earlier: Vec<&Source> = sources.iter().filter(|s| s.at < k).collect();
    // An alias is added after FROM, JOIN or a comma in FROM, unless one is already written.
    let after = (0..a.tokens.len()).find(|&j| !a.tokens[j].is_comment() && a.starts[j] >= caret);
    let has_alias = after.is_some_and(|j| {
        let t = &a.tokens[j];
        matches!(t.kind, Kind::Bracket | Kind::Quoted | Kind::Dot | Kind::LParen) || (t.kind == Kind::Word && (!is_reserved(&a.up[j]) || a.up[j] == "AS"))
    });
    let aliasing = options.assign_aliases
        && context == Context::TableSource
        && !has_alias
        && (matches!(prev_upper, "FROM" | "JOIN") || (matches!(prev_word, Some((Kind::Comma, _))) && clause == "FROM"));
    let taken: Vec<String> = sources.iter().filter(|s| s.at < k || a.starts[s.at] > caret).map(|s| s.reference.clone()).collect();
    let with_alias = |item: &mut Item, name: &str| {
        let alias = alias_with(name, &taken, &options);
        if !eq(&alias, name) {
            item.insert = format!("{}{}{alias}", item.insert, if options.alias_with_as { " AS " } else { " " });
        }
    };
    let qualify = options.qualify_object_names;
    // In a select list, before anything is typed: check several columns at once.
    let picking = clause == "SELECT" && prefix.is_empty() && !variable;
    let picker = || Item { label: "(choose columns…)".into(), insert: String::new(), kind: "picker", detail: "check the columns to insert".into() };

    if variable {
        for (name, ty) in a.declared_variables(prefix_start) {
            items.push(Item { label: name.clone(), insert: name, kind: "variable", detail: ty });
        }
        if stmt_start < a.tokens.len() && matches!(a.up[stmt_start].as_str(), "EXEC" | "EXECUTE") {
            let mut to = stmt_start + 1;
            while to < a.tokens.len() && (is_name(&a.tokens[to]) || a.tokens[to].kind == Kind::Dot) && a.starts[to] < prefix_start {
                to += 1;
            }
            // `@Name =` in the call names a parameter already given; it is not a variable either,
            // unless the script uses it before the call.
            let named: Vec<&str> = (to..a.tokens.len())
                .take_while(|&j| a.starts[j] < prefix_start)
                .filter(|&j| a.tokens[j].kind == Kind::Variable && a.tokens.get(j + 1).is_some_and(|t| t.text == "="))
                .map(|j| a.tokens[j].text.as_str())
                .collect();
            let used_before = |name: &str| (0..stmt_start).any(|j| a.tokens[j].kind == Kind::Variable && eq(&a.tokens[j].text, name));
            items.retain(|it| it.kind != "variable" || !named.iter().any(|n| eq(n, &it.label)) || used_before(&it.label));
            if let Some(proc) = object_at(&a.tokens, stmt_start + 1, to, meta) {
                for p in proc.params.iter().filter(|p| !named.iter().any(|n| eq(n, &p.name))) {
                    items.push(Item { label: p.name.clone(), insert: format!("{} = ", p.name), kind: "parameter", detail: p.data_type.clone() });
                }
            }
        }
        for g in GLOBAL_VARIABLES {
            items.push(Item { label: g.to_string(), insert: g.to_string(), kind: "global_variable", detail: "global".into() });
        }
    } else if !qualifier.is_empty() {
        let q = qualifier.last().expect("non-empty");
        // The qualifier being typed is itself parsed as a source, so only a source with
        // known columns counts as a match.
        let aliased = sources
            .iter()
            .filter(|s| qualifier.len() == 1 && eq(&s.reference, q))
            .map(|s| source_columns(s, schema))
            .find(|c| !c.is_empty());
        if let Some(columns) = aliased {
            if picking && columns.len() > 1 {
                items.push(picker());
            }
            for (name, ty) in columns {
                if conditions && let Some(r) = role(q, &name) {
                    roles.insert(quote(&name), r);
                }
                items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
            }
        } else if let Some(other) = others.iter().find(|o| qualifier.len() == 1 && eq(&o.database, q)) {
            // `Other.` goes on with a schema of that database.
            for s in &other.schemas {
                items.push(Item { label: s.clone(), insert: quote(s), kind: "schema", detail: format!("{} schema", other.database) });
            }
        } else if let Some(other) = others.iter().find(|o| qualifier.len() == 2 && eq(&o.database, &qualifier[0])) {
            let wanted = |kind: &str| match context {
                Context::Expression => kind != "procedure",
                c => wants(c, kind),
            };
            for o in other.objects.iter().filter(|o| eq(&o.schema, q) && wanted(&o.kind)) {
                let mut item = object_item(o, other, true);
                item.detail = format!("{}.{}", other.database, item.detail);
                if aliasing && matches!(o.kind.as_str(), "table" | "view" | "synonym") {
                    with_alias(&mut item, &o.name);
                }
                items.push(item);
            }
        } else if meta.schemas.iter().any(|s| eq(s, q)) || meta.objects.iter().any(|o| eq(&o.schema, q) && o.database.is_empty()) {
            let o_kind_in_expression = |kind: &str| kind != "procedure";
            let wanted = |kind: &str| match context {
                Context::Expression => o_kind_in_expression(kind),
                c => wants(c, kind),
            };
            for o in meta.objects.iter().filter(|o| eq(&o.schema, q) && o.database.is_empty() && wanted(&o.kind)) {
                let mut item = object_item(o, meta, true);
                if aliasing && matches!(o.kind.as_str(), "table" | "view" | "synonym") {
                    with_alias(&mut item, &o.name);
                }
                items.push(item);
            }
        } else if qualifier.len() == 1 && eq(q, &meta.database) {
            // `Shop.` goes on with a schema of the database.
            for s in &meta.schemas {
                items.push(Item { label: s.clone(), insert: quote(s), kind: "schema", detail: "schema".into() });
            }
        } else if let Some(src) = sources.iter().find(|s| s.parts.last().is_some_and(|p| eq(p, q))) {
            for (name, ty) in source_columns(src, schema) {
                items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
            }
        }
    } else {
        match context {
            Context::TableSource | Context::Procedure | Context::Views | Context::Functions | Context::Securables => {
                // After EXEC, the system procedures too (sp_who2, sp_helptext): they are called
                // without their schema, and sort after the database's own.
                let system_procedure = |o: &DbObject| context == Context::Procedure && o.kind == "procedure" && eq(&o.schema, "sys");
                for o in meta.objects.iter().filter(|o| wants(context, &o.kind) && o.database.is_empty() && (options.show_system_objects || !is_system(o) || system_procedure(o))) {
                    let mut item = object_item(o, meta, false);
                    if system_procedure(o) {
                        item.insert = quote(&o.name);
                    } else if qualify && !o.schema.is_empty() {
                        item.insert = format!("{}.{}", quote(&o.schema), quote(&o.name));
                    }
                    if aliasing && matches!(o.kind.as_str(), "table" | "view" | "synonym") {
                        with_alias(&mut item, &o.name);
                    }
                    if matches!(prev_upper, "EXEC" | "EXECUTE") && o.kind == "procedure" && !o.params.is_empty() {
                        let align = " ".repeat(column + item.insert.chars().count() + 1);
                        item.insert = format!("{} {}", item.insert, parameter_list(&o.params, &align, nl));
                    }
                    items.push(item);
                }
                if prev_upper == "JOIN" {
                    let taken: Vec<String> = sources.iter().map(|s| s.reference.clone()).collect();
                    for o in meta.objects.iter().filter(|o| matches!(o.kind.as_str(), "table" | "view") && o.database.is_empty()) {
                        let alias = alias_with(&o.name, &taken, &options);
                        for e in &earlier {
                            let Some(eo) = resolve(e, meta) else { continue };
                            if let Some((cond, fk)) = join_conditions((&alias, o), (&e.reference, eo), meta).into_iter().next() {
                                let name = if qualify && !o.schema.is_empty() { format!("{}.{}", quote(&o.schema), quote(&o.name)) } else { object_item(o, meta, false).insert };
                                let kind = if fk { "join" } else { "join_guess" };
                                let as_ = if options.alias_with_as { " AS " } else { " " };
                                items.push(Item { label: o.name.clone(), insert: format!("{name}{as_}{alias} ON {cond}"), kind, detail: format!("ON {cond}") });
                            }
                        }
                    }
                }
                if prev_upper == "INSERT" {
                    items.push(Item { label: "INTO".into(), insert: "INTO".into(), kind: "keyword", detail: String::new() });
                }
                if prev_upper == "TABLE" && a.up.get(stmt_start).is_some_and(|w| w == "DROP") {
                    items.push(Item { label: "IF EXISTS".into(), insert: "IF EXISTS".into(), kind: "keyword", detail: String::new() });
                }
                if context == Context::TableSource {
                    for cte in a.ctes(stmt_start, stmt_end) {
                        let mut item = Item { label: cte.reference.clone(), insert: quote(&cte.reference), kind: "cte", detail: "common table expression".into() };
                        if aliasing {
                            with_alias(&mut item, &cte.reference);
                        }
                        items.push(item);
                    }
                }
                for s in &meta.schemas {
                    items.push(Item { label: s.clone(), insert: quote(s), kind: "schema", detail: "schema".into() });
                }
                if matches!(context, Context::TableSource) {
                    for d in meta.databases.iter().filter(|d| !eq(d, &meta.database)) {
                        items.push(Item { label: d.clone(), insert: quote(d), kind: "database", detail: "database".into() });
                    }
                    for l in &meta.linked_servers {
                        items.push(Item { label: l.clone(), insert: quote(l), kind: "server", detail: "linked server".into() });
                    }
                }
            }
            Context::InsertTarget => {
                let from = insert_target.expect("context");
                if let Some(o) = object_at(&head, from, k, meta) {
                    let cols = writable(&o);
                    if !cols.is_empty() {
                        items.push(Item { label: "(columns) VALUES (...)".into(), insert: insert_body(&cols, &indent, nl), kind: "body", detail: format!("{} columns of {}.{}", cols.len(), o.schema, o.name) });
                    }
                }
            }
            Context::ExecArgs => {
                let from = exec_target.expect("context");
                let (_, to) = name_range(&head, from, k);
                // Parameters already given by name are not offered again.
                let written: Vec<&str> = head[to..k].iter().filter(|t| t.kind == Kind::Variable).map(|t| t.text.as_str()).collect();
                if let Some(o) = object_at(&head, from, to, meta) {
                    if !o.params.is_empty() && exec_more.is_none() {
                        let align = " ".repeat(column);
                        items.push(Item { label: "(all parameters)".into(), insert: parameter_list(&o.params, &align, nl), kind: "body", detail: format!("{}.{}", o.schema, o.name) });
                    }
                    for p in o.params.iter().filter(|p| !written.iter().any(|w| eq(w, &p.name))) {
                        items.push(Item { label: p.name.clone(), insert: format!("{} = ", p.name), kind: "parameter", detail: p.data_type.clone() });
                    }
                }
            }
            Context::InsertColumns => {
                if let Some(target) = sources.iter().find(|s| !s.parts.is_empty()) {
                    for (name, ty) in source_columns(target, schema) {
                        items.push(Item { label: name.clone(), insert: quote(&name), kind: "column", detail: ty });
                    }
                }
            }
            Context::Expression => {
                if prev_upper == "ON" && clause == "ON"
                    && let Some((new, older)) = earlier.split_last()
                        && let Some(no) = resolve(new, meta) {
                            for e in older {
                                let Some(eo) = resolve(e, meta) else { continue };
                                for (cond, fk) in join_conditions((&new.reference, no), (&e.reference, eo), meta) {
                                    let kind = if fk { "join" } else { "join_guess" };
                                    items.push(Item { label: cond.clone(), insert: cond, kind, detail: "join condition".into() });
                                }
                            }
                        }
                if prev_upper == "SET" && a.up.get(stmt_start).is_some_and(|w| w == "UPDATE")
                    && let Some(o) = sources.iter().find(|s| !s.parts.is_empty()).and_then(|s| resolve(s, meta)) {
                        let cols = writable(o);
                        let align = " ".repeat(column);
                        let body = cols
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{} = {}{} -- {}", quote(&c.name), sample_value(&c.data_type), if i + 1 < cols.len() { "," } else { "" }, c.data_type))
                            .collect::<Vec<_>>()
                            .join(&format!("{nl}{align}"));
                        if !body.is_empty() {
                            items.push(Item { label: "(all columns)".into(), insert: body, kind: "body", detail: format!("{}.{}", o.schema, o.name) });
                        }
                    }
                // `c.Name = |`: the values its CHECK constraints allow, or else a literal of its type, the
                // caret inside its quotes; for a bit 1 and 0, for a date the current one.
                if prefix.is_empty()
                    && let Some(c) = compared.as_ref().filter(|c| !c.variable && !c.list)
                    && let Some((o, col)) = column_in(&sources, c.qualifier.as_deref(), &c.name, meta)
                {
                    let mut values: Vec<(String, String)> = check_values(o, &col.name).into_iter().map(|v| (v.clone(), v)).collect();
                    if values.is_empty() {
                        values.extend(literal_for(&col.data_type).map(|l| (l.replace("$CURSOR$", ""), l.to_string())));
                        let now = match base_type(&col.data_type).as_str() {
                            "bit" => vec!["1", "0"],
                            "date" => vec!["CAST(GETDATE() AS date)"],
                            "datetime" | "smalldatetime" => vec!["GETDATE()"],
                            "datetime2" => vec!["SYSDATETIME()"],
                            "datetimeoffset" => vec!["SYSDATETIMEOFFSET()"],
                            _ => Vec::new(),
                        };
                        values.extend(now.into_iter().map(|v| (v.to_string(), v.to_string())));
                    }
                    for (label, insert) in values {
                        items.push(Item { label, insert, kind: "value", detail: format!("{} · {}", col.name, col.data_type) });
                    }
                }
                for (name, ty) in &favoured {
                    items.push(Item { label: name.clone(), insert: name.clone(), kind: "variable", detail: ty.clone() });
                }
                // `GROUP BY |`: the select list's expressions that are not aggregates.
                if prefix.is_empty()
                    && clause == "GROUP"
                    && prev_upper == "BY"
                    && let Some(by) = (0..k).rev().find(|&j| !head[j].is_comment())
                    && let Some(group) = (0..by).rev().find(|&j| !head[j].is_comment()).filter(|&g| a.up[g] == "GROUP")
                    && let Some(select) = a.select_before(stmt_start, group)
                {
                    let list = a.grouping_expressions(select, group).join(", ");
                    if !list.is_empty() {
                        pinned.push(list.clone());
                        items.push(Item { label: list.clone(), insert: list, kind: "body", detail: "the select list's columns".into() });
                    }
                }
                // `INSERT INTO t (a, b) SELECT |`: the same columns, in their order, from the sources.
                if picking
                    && prev_upper == "SELECT"
                    && a.up.get(stmt_start).is_some_and(|w| w == "INSERT")
                    && let Some(select) = (0..k).rev().find(|&j| !head[j].is_comment())
                    && let Some(open) = (stmt_start..select).find(|&j| a.tokens[j].kind == Kind::LParen)
                    && a.in_insert_column_list(open + 1)
                {
                    let close = a.matching(open, select);
                    let from: Vec<&Source> = sources.iter().filter(|s| s.at > select).collect();
                    let columns: Vec<String> = (open + 1..close)
                        .filter(|&j| is_name(&a.tokens[j]))
                        .map(|j| {
                            let name = bare(&a.tokens[j]);
                            let owners: Vec<&&Source> = from.iter().filter(|s| source_columns(s, schema).iter().any(|(c, _)| eq(c, &name))).collect();
                            match owners.as_slice() {
                                [s] if from.len() > 1 || s.parts.last().is_none_or(|p| !eq(p, &s.reference)) => format!("{}.{}", quote(&s.reference), quote(&name)),
                                _ => quote(&name),
                            }
                        })
                        .collect();
                    if !columns.is_empty() {
                        let list = columns.join(", ");
                        pinned.push(list.clone());
                        items.push(Item { label: list.clone(), insert: list, kind: "body", detail: "the INSERT's columns, in order".into() });
                    }
                }
                // Columns of every source. A column of a source with an alias (`dbo.JitBin AS jb`) is
                // written with it (`jb.Name`), and so is a name in more than one source.
                let all: Vec<(String, String, String, bool)> = sources
                    .iter()
                    .flat_map(|s| {
                        let aliased = s.parts.last().is_none_or(|p| !eq(p, &s.reference));
                        source_columns(s, schema).into_iter().map(move |(c, t)| (s.reference.clone(), c, t, aliased))
                    })
                    .collect();
                if picking && all.len() > 1 {
                    items.push(picker());
                }
                for (reference, name, ty, aliased) in &all {
                    let ambiguous = all.iter().filter(|(_, n, _, _)| eq(n, name)).count() > 1;
                    let insert = if ambiguous || *aliased { format!("{}.{}", quote(reference), quote(name)) } else { quote(name) };
                    let detail = if ty.is_empty() { reference.clone() } else { format!("{reference} · {ty}") };
                    if conditions && let Some(r) = role(reference, name) {
                        roles.insert(insert.clone(), r);
                    }
                    items.push(Item { label: name.clone(), insert, kind: "column", detail });
                }
                for s in &sources {
                    items.push(Item { label: s.reference.clone(), insert: quote(&s.reference), kind: "alias", detail: s.parts.join(".") });
                }
                for o in meta.objects.iter().filter(|o| o.kind == "scalar_function" && o.database.is_empty()) {
                    let mut item = object_item(o, meta, false);
                    // Scalar functions must be called with their schema.
                    item.insert = format!("{}.{}()", quote(&o.schema), quote(&o.name));
                    items.push(item);
                }
                // Reserved names (LEFT, CONVERT, COALESCE...) come with the keywords.
                for f in crate::functions::all().iter().filter(|f| !is_reserved(&f.name)) {
                    // The editor puts the caret between the parentheses.
                    let insert = if NILADIC.contains(&f.name.as_str()) { f.name.clone() } else { format!("{}()", f.name) };
                    items.push(Item { label: f.name.clone(), insert, kind: "function", detail: "built-in function".into() });
                }
                // Where a value goes no statement begins, but a subquery may: `(SELECT`.
                let statements: &[&str] = if prev_word.as_ref().is_some_and(|(kind, _)| *kind == Kind::LParen) { &["SELECT", "WITH"] } else { &[] };
                for kw in statements.iter().chain(EXPRESSION_KEYWORDS) {
                    items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                }
            }
            Context::AfterSource => {
                for kw in CLAUSE_KEYWORDS.iter().chain(STATEMENT_START) {
                    items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                }
            }
            Context::Keywords => {
                if let Some(Next::Words(words, variables)) = &next {
                    for kw in words {
                        items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                    }
                    // Where a statement starts, a procedure can too, without EXEC (`sp_who2`, as the
                    // first statement of a batch may): the database's own and the system ones.
                    if !prefix.is_empty() && words.contains(&"EXEC") {
                        for o in meta.objects.iter().filter(|o| o.kind == "procedure" && o.database.is_empty()) {
                            let mut item = object_item(o, meta, false);
                            if eq(&o.schema, "sys") {
                                item.insert = quote(&o.name);
                            } else if qualify && !o.schema.is_empty() {
                                item.insert = format!("{}.{}", quote(&o.schema), quote(&o.name));
                            }
                            // Its parameters, as after EXEC.
                            if !o.params.is_empty() {
                                let align = " ".repeat(column + item.insert.chars().count() + 1);
                                item.insert = format!("{} {}", item.insert, parameter_list(&o.params, &align, nl));
                            }
                            items.push(item);
                        }
                    }
                    // `Status IN (|`: the values its CHECK constraints allow.
                    if prefix.is_empty()
                        && let Some(c) = compared.as_ref().filter(|c| c.list && !c.variable)
                        && let Some((o, col)) = column_in(&sources, c.qualifier.as_deref(), &c.name, meta)
                    {
                        for v in check_values(o, &col.name) {
                            items.push(Item { label: v.clone(), insert: v, kind: "value", detail: format!("{} · {}", col.name, col.data_type) });
                        }
                    }
                    if *variables {
                        for (name, ty) in a.declared_variables(prefix_start) {
                            items.push(Item { label: name.clone(), insert: name, kind: "variable", detail: ty });
                        }
                    }
                }
            }
            Context::TableColumns => {
                if let Some(Next::Columns(from, to)) = next
                    && let Some(o) = object_at(&head, from, to, meta)
                {
                    for c in &o.columns {
                        items.push(Item { label: c.name.clone(), insert: quote(&c.name), kind: "column", detail: c.data_type.clone() });
                    }
                }
            }
            Context::Types => {
                if let Some(Next::Types(words)) = &next {
                    // `DECLARE @CustomerId |` or `CREATE TABLE t (CustomerId |`: the type the database's
                    // columns of that name have most.
                    if let Some(v) = (0..k).rev().find(|&j| !head[j].is_comment()).filter(|&j| head[j].kind == Kind::Variable || operand_name(&head, j).is_some()) {
                        let name = bare(&head[v]);
                        let name = name.trim_start_matches('@');
                        let mut types: Vec<(&str, usize, &DbObject)> = Vec::new();
                        for o in meta.objects.iter().filter(|o| o.database.is_empty() && !is_system(o)) {
                            for c in o.columns.iter().filter(|c| !c.data_type.is_empty() && eq(&c.name, name)) {
                                match types.iter_mut().find(|t| eq(t.0, &c.data_type)) {
                                    Some(t) => t.1 += 1,
                                    None => types.push((&c.data_type, 1, o)),
                                }
                            }
                        }
                        if let Some((ty, _, o)) = types.into_iter().rev().max_by_key(|t| t.1) {
                            let ty = ty.to_ascii_uppercase();
                            items.push(Item { label: ty.clone(), insert: ty, kind: "value", detail: format!("as {}.{}.{name}", o.schema, o.name) });
                        }
                    }
                    for t in DATA_TYPES {
                        items.push(Item { label: t.to_string(), insert: t.to_string(), kind: "type", detail: "data type".into() });
                    }
                    for kw in words {
                        items.push(Item { label: kw.to_string(), insert: kw.to_string(), kind: "keyword", detail: String::new() });
                    }
                }
            }
            Context::Nothing => {}
        }
    }

    // Snippets write statements: only where one may begin, or a subquery after `(`; `WHERE Id be`
    // is BETWEEN, not a BEGIN...END block.
    let statement_here = match (context, &next) {
        (Context::Keywords, Some(Next::Words(words, _))) => words.contains(&"BEGIN") || words.contains(&"SELECT"),
        (Context::AfterSource, _) => true,
        (Context::Expression, _) => prev_word.as_ref().is_some_and(|(kind, _)| *kind == Kind::LParen),
        _ => false,
    };
    if prefix.chars().count() >= 2 && qualifier.is_empty() && !variable && statement_here {
        for sn in snippets {
            items.push(Item { label: sn.prefix.clone(), insert: sn.prefix.clone(), kind: "snippet", detail: sn.description.clone() });
        }
    }

    // Words in the order they are most likely: the keyword lists are written that way.
    let likely: Vec<&str> = match (context, &next) {
        (Context::Keywords, Some(Next::Words(words, _))) => words.clone(),
        (Context::Types, _) => COMMON_TYPES.to_vec(),
        (Context::AfterSource, _) => CLAUSE_KEYWORDS.iter().chain(STATEMENT_START).copied().collect(),
        (Context::Expression, _) => COMMON_FUNCTIONS.to_vec(),
        _ => Vec::new(),
    };
    // `INSERT INTO |`: picking a table or view writes the whole statement, its columns and a VALUES
    // row, unless the statement already goes on after the name.
    let insert_into = matches!(context, Context::TableSource)
        && matches!(prev_upper, "INTO" | "INSERT")
        && a.up.get(stmt_start).is_some_and(|w| w == "INSERT")
        && after.is_none_or(|j| j >= stmt_end);
    if insert_into {
        for it in items.iter_mut().filter(|it| matches!(it.kind, "table" | "view")) {
            let parts: Vec<String> = it.detail.split('.').map(str::to_string).collect();
            if let Some(o) = meta.objects.iter().find(|o| object_matches(o, &parts, meta)) {
                let cols = writable(o);
                if !cols.is_empty() {
                    it.insert = format!("{}{}", it.insert, insert_body(&cols, &indent, nl));
                }
            }
        }
    }
    let likely = |it: &Item| if it.kind == "column" { roles.get(&it.insert).copied().unwrap_or(usize::MAX) } else { likely.iter().position(|w| w.eq_ignore_ascii_case(&it.label)).unwrap_or(usize::MAX) };

    // How often the user picked it: first among matches of the same quality and kind, after the
    // whole word and the statement-writing items; a word picked often never passes a column.
    let used = |it: &Item| std::cmp::Reverse(options.usage.get(&crate::usage::item_key(it.kind, &it.label)).copied().unwrap_or(0));

    let pin = |it: &Item| matches!(it.kind, "variable" | "body") && pinned.contains(&it.label);
    // Where a condition starts, `ex` is EXISTS before EXP().
    let exists = context == Context::Expression && matches!(prev_upper, "NOT" | "WHERE" | "AND" | "OR" | "HAVING" | "WHEN" | "IF" | "WHILE");

    // Rank: prefix matches, then substring matches; then by kind, how likely, and name.
    // (pinned, then written items or the whole word, then the rest; kind, how often picked)
    type Rank = (u8, u8, std::cmp::Reverse<u32>);
    let mut scored: Vec<(u8, Rank, bool, usize, Item)> = items
        .into_iter()
        .filter_map(|it| {
            let exact_or_prefix = match_quality(&it.label, &prefix)?;
            // A keyword is found by its start or initials: `an` is AND, not the end of GRANT.
            if exact_or_prefix == 3 && it.kind == "keyword" {
                return None;
            }
            // The whole word ranks with its start; both come before initials and inner matches.
            let quality = if exact_or_prefix == 0 { 1 } else { exact_or_prefix };
            let label = it.label.to_ascii_lowercase();
            let label = label.trim_start_matches('@');
            let needle = prefix.to_ascii_lowercase();
            let needle = needle.trim_start_matches('@');
            // System catalog views sort after the database's own objects.
            let system = it.detail.starts_with("sys.") || it.detail.starts_with("INFORMATION_SCHEMA.");
            // A whole word typed in another case (`join`, `customerid`) comes first, so Tab or Enter
            // gives it its proper case instead of a longer name that starts with it.
            let exact = !needle.is_empty() && label == needle;
            let kind = match it.kind {
                "keyword" if exists && it.label == "EXISTS" => 1,
                // After ON, a condition guessed from the names still comes before the columns.
                "join_guess" if context == Context::Expression => 0,
                k => rank(k, context),
            };
            let rank = if pin(&it) {
                (0, 0, std::cmp::Reverse(0))
            } else if exact || kind == 0 {
                (1, 0, std::cmp::Reverse(0))
            } else {
                (2, kind, used(&it))
            };
            Some((quality, rank, system, likely(&it), it))
        })
        .collect();
    // A name typed with a typo is offered only when nothing matches as typed.
    if scored.iter().any(|x| x.0 < 4) {
        scored.retain(|x| x.0 < 4);
    }
    // Among names that start with what was typed, the shortest is the likeliest: `tabl` is tables
    // before table_types.
    let length = |it: &Item| if prefix.is_empty() { 0 } else { it.label.len() };
    scored.sort_by_key(|x| (x.0, x.1, x.2, x.3, length(&x.4), x.4.label.to_ascii_lowercase()));
    scored.dedup_by(|x, y| x.4.insert == y.4.insert && x.4.kind == y.4.kind);
    let mut items: Vec<Item> = scored.into_iter().take(MAX_ITEMS).map(|(_, _, _, _, it)| it).collect();
    for it in items.iter_mut() {
        if let Some(s) = shaped(&it.label, it.kind, context) {
            it.insert = s;
        }
    }
    // Select the first item only when something was typed and it matches for sure; a list of guesses,
    // or one opened by a space before anything is typed, starts with nothing selected.
    // Three letters inside a built-in function's name count too (`add` for DATEADD): their names
    // are known words, unlike the database's names, where they may be the start of a new one.
    let sure = |it: &Item, q: u8| q <= 2 || (q == 3 && it.kind == "function" && prefix.chars().count() >= 3);
    let selected = items
        .first()
        .filter(|it| (prefix.is_empty() && (it.kind == "value" || pin(it))) || (!prefix.is_empty() && match_quality(&it.label, &prefix).is_some_and(|q| sure(it, q))))
        .map(|_| 0);
    Completion { start: byte_to_utf16(text, prefix_start), items, selected }
}
