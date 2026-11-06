//! Object scripts for the editor: `CREATE TABLE` from catalog details, and modules turned into
//! `ALTER` statements.

use serde::Deserialize;

use crate::lexer::{Kind, lex};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColumnDetail {
    pub name: String,
    /// The type as written, with its length or precision: `nvarchar(100)`.
    pub data_type: String,
    pub nullable: bool,
    pub identity_seed: Option<String>,
    pub identity_increment: Option<String>,
    pub computed: Option<String>,
    pub persisted: bool,
    pub default_name: Option<String>,
    pub default_definition: Option<String>,
    pub collation: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndexDetail {
    pub name: String,
    /// CLUSTERED, NONCLUSTERED, CLUSTERED COLUMNSTORE...
    pub kind: String,
    pub primary_key: bool,
    pub unique: bool,
    pub unique_constraint: bool,
    /// Key columns, already quoted: `[a], [b] DESC`.
    pub keys: String,
    pub includes: String,
    pub filter: Option<String>,
    /// Where the index is stored: a filegroup, or a partition scheme and its column.
    pub storage: Storage,
}

/// A filegroup (`[PRIMARY]`), or a partition scheme with its partitioning column.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Storage {
    pub filegroup: Option<String>,
    pub scheme: Option<String>,
    pub column: Option<String>,
}

impl Storage {
    fn clause(&self) -> String {
        match (&self.scheme, &self.column, &self.filegroup) {
            (Some(scheme), Some(column), _) => format!(" ON {} ({})", bracket(scheme), bracket(column)),
            (_, _, Some(fg)) if !fg.is_empty() => format!(" ON {}", bracket(fg)),
            _ => String::new(),
        }
    }
}

/// How a partitioned table is split.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Partitioning {
    pub function: String,
    pub scheme: String,
    pub column: String,
    /// The partitioning column's type: `date`, `int`...
    pub parameter_type: String,
    /// LEFT or RIGHT.
    pub range: String,
    /// Boundary values as T-SQL literals, in order.
    pub boundaries: Vec<String>,
    /// The filegroup of each partition, in order.
    pub filegroups: Vec<String>,
    /// Rows in each partition, in order.
    pub rows: Vec<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ForeignKeyDetail {
    pub name: String,
    pub columns: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: String,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CheckDetail {
    pub name: String,
    pub definition: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TableDetails {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColumnDetail>,
    pub indexes: Vec<IndexDetail>,
    pub foreign_keys: Vec<ForeignKeyDetail>,
    pub checks: Vec<CheckDetail>,
    /// Where the table (its heap or clustered index) is stored.
    pub storage: Storage,
    pub partitioning: Option<Partitioning>,
}

/// `[name]`, with `]` doubled.
pub fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn action(kind: &str, value: &str) -> String {
    match value.replace('_', " ").to_ascii_uppercase().as_str() {
        "" | "NO ACTION" => String::new(),
        v => format!(" ON {kind} {v}"),
    }
}

/// `CREATE PARTITION FUNCTION` and `SCHEME` statements, each only if it does not exist yet.
fn partition_script(p: &Partitioning, nl: &str) -> String {
    let filegroups = if p.filegroups.iter().all(|f| f == &p.filegroups[0]) && !p.filegroups.is_empty() {
        format!("ALL TO ({})", bracket(&p.filegroups[0]))
    } else {
        format!("TO ({})", p.filegroups.iter().map(|f| bracket(f)).collect::<Vec<_>>().join(", "))
    };
    format!(
        "-- {} partitions on {}{nl}IF NOT EXISTS (SELECT 1 FROM sys.partition_functions WHERE name = N'{}'){nl}    CREATE PARTITION FUNCTION {} ({}) AS RANGE {} FOR VALUES ({});{nl}GO{nl}IF NOT EXISTS (SELECT 1 FROM sys.partition_schemes WHERE name = N'{}'){nl}    CREATE PARTITION SCHEME {} AS PARTITION {} {filegroups};{nl}GO{nl}",
        p.boundaries.len() + 1,
        bracket(&p.column),
        p.function.replace('\'', "''"),
        bracket(&p.function),
        p.parameter_type,
        p.range,
        p.boundaries.join(", "),
        p.scheme.replace('\'', "''"),
        bracket(&p.scheme),
        bracket(&p.function),
    )
}

/// A `CREATE TABLE` script with its partitioning, constraints, foreign keys and indexes.
pub fn table_script(t: &TableDetails, nl: &str) -> String {
    let full = format!("{}.{}", bracket(&t.schema), bracket(&t.name));
    let prelude = t.partitioning.as_ref().map(|p| partition_script(p, nl)).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    for c in &t.columns {
        let mut line = format!("    {} ", bracket(&c.name));
        if let Some(expr) = &c.computed {
            line.push_str(&format!("AS {expr}{}", if c.persisted { " PERSISTED" } else { "" }));
            lines.push(line);
            continue;
        }
        line.push_str(&c.data_type);
        if let Some(collation) = c.collation.as_deref().filter(|s| !s.is_empty()) {
            line.push_str(&format!(" COLLATE {collation}"));
        }
        if let (Some(seed), Some(step)) = (&c.identity_seed, &c.identity_increment) {
            line.push_str(&format!(" IDENTITY({seed}, {step})"));
        }
        line.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
        if let Some(def) = &c.default_definition {
            match &c.default_name {
                Some(n) => line.push_str(&format!(" CONSTRAINT {} DEFAULT {def}", bracket(n))),
                None => line.push_str(&format!(" DEFAULT {def}")),
            }
        }
        lines.push(line);
    }
    for i in t.indexes.iter().filter(|i| i.primary_key || i.unique_constraint) {
        let kind = if i.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
        lines.push(format!("    CONSTRAINT {} {kind} {} ({}){}", bracket(&i.name), i.kind, i.keys, i.storage.clause()));
    }
    for c in &t.checks {
        lines.push(format!("    CONSTRAINT {} CHECK {}", bracket(&c.name), c.definition));
    }
    let mut out = format!("{prelude}CREATE TABLE {full}{nl}({nl}{}{nl}){};{nl}GO{nl}", lines.join(&format!(",{nl}")), t.storage.clause());
    for fk in &t.foreign_keys {
        out.push_str(&format!(
            "ALTER TABLE {full} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}.{} ({}){}{};{nl}GO{nl}",
            bracket(&fk.name),
            fk.columns,
            bracket(&fk.ref_schema),
            bracket(&fk.ref_table),
            fk.ref_columns,
            action("DELETE", &fk.on_delete),
            action("UPDATE", &fk.on_update)
        ));
    }
    for i in t.indexes.iter().filter(|i| !i.primary_key && !i.unique_constraint) {
        let mut line = format!("CREATE {}{} INDEX {} ON {full}", if i.unique { "UNIQUE " } else { "" }, i.kind, bracket(&i.name));
        if !i.keys.is_empty() {
            line.push_str(&format!(" ({})", i.keys));
        }
        if !i.includes.is_empty() {
            line.push_str(&format!(" INCLUDE ({})", i.includes));
        }
        if let Some(filter) = i.filter.as_deref().filter(|f| !f.is_empty()) {
            line.push_str(&format!(" WHERE {filter}"));
        }
        line.push_str(&i.storage.clause());
        out.push_str(&format!("{line};{nl}GO{nl}"));
    }
    out
}

/// The module definition with its first `CREATE` (or `CREATE OR ALTER`) turned into `ALTER`.
pub fn to_alter(definition: &str) -> Result<String, String> {
    let lexed = lex(definition).map_err(|e| e.to_string())?;
    let tokens = &lexed.tokens;
    let create = tokens.iter().position(|t| t.kind == Kind::Word && t.text.eq_ignore_ascii_case("CREATE")).ok_or("the definition has no CREATE")?;
    let mut start = 0;
    for t in &tokens[..create] {
        start += t.ws.len() + t.text.len();
    }
    start += tokens[create].ws.len();
    let mut end = start + tokens[create].text.len();
    // `CREATE OR ALTER` becomes a single `ALTER`.
    let next: Vec<usize> = (create + 1..tokens.len()).filter(|&j| !tokens[j].is_comment()).take(2).collect();
    if let [or, alter] = next[..]
        && tokens[or].text.eq_ignore_ascii_case("OR") && tokens[alter].text.eq_ignore_ascii_case("ALTER") {
            end = tokens[..=alter].iter().map(|t| t.ws.len() + t.text.len()).sum();
        }
    let mut out = definition.to_string();
    out.replace_range(start..end, "ALTER");
    Ok(out)
}
