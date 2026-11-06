//! Which tables and columns of the two databases are compared, and by which key.

use serde::{Deserialize, Serialize};

use super::options::{DataOpt, DataOptions};
use crate::catalog::*;

/// A table (or view) of database 1 paired with one of database 2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePair {
    pub source: ObjectName,
    pub target: ObjectName,
    pub columns: Vec<ColumnPair>,
    /// Indexes into `columns` of the comparison key, in key order.
    pub key: Vec<usize>,
    /// Where the key came from: the primary key's or unique index's name, or "custom".
    pub key_name: Option<String>,
    pub where1: Option<String>,
    pub where2: Option<String>,
    /// Why the pair cannot be compared (no usable key, no common columns).
    pub problem: Option<String>,
    pub is_view: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPair {
    pub source: String,
    pub target: String,
    pub source_type: DataType,
    pub target_type: DataType,
    /// The target column cannot be written (computed, timestamp, generated always).
    pub read_only: bool,
    pub identity: bool,
    pub case_insensitive: bool,
    pub nullable: bool,
}

/// User choices that override the automatic mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DataMappings {
    pub tables: Vec<TableOverride>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TableOverride {
    pub source: ObjectName,
    /// A different table in database 2.
    pub target: Option<ObjectName>,
    /// Leave the table out.
    pub excluded: bool,
    /// Key columns (source names) to use instead of the primary key.
    pub key: Vec<String>,
    /// Columns (source names) left out of the comparison.
    pub excluded_columns: Vec<String>,
    pub where1: Option<String>,
    pub where2: Option<String>,
}

/// The name used for pairing: without spaces (and underscores), lower-case unless case-sensitive.
fn normalize(s: &str, o: &DataOptions) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !(o.has(DataOpt::IgnoreSpaces) && c.is_whitespace()) && !(o.has(DataOpt::IgnoreUnderscores) && *c == '_'))
        .collect();
    if !o.has(DataOpt::CaseSensitiveObjectDefinition) {
        out = out.to_lowercase();
    }
    out
}

fn is_case_insensitive(collation: Option<&str>) -> bool {
    collation.is_some_and(|c| {
        let c = c.to_ascii_uppercase();
        c.contains("_CI") && !c.contains("_BIN")
    })
}

/// Whether values of the two types can be compared and copied.
fn compatible(a: &DataType, b: &DataType) -> bool {
    fn family(t: &DataType) -> u8 {
        if t.schema.is_some() {
            return 100;
        }
        match t.name.as_str() {
            "tinyint" | "smallint" | "int" | "bigint" | "bit" | "decimal" | "numeric" | "money" | "smallmoney" | "float" | "real" => 1,
            "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname" => 2,
            "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => 3,
            "date" => 4,
            "time" => 5,
            "datetime" | "smalldatetime" | "datetime2" => 6,
            "datetimeoffset" => 7,
            "uniqueidentifier" => 8,
            "xml" => 9,
            "hierarchyid" | "geography" | "geometry" => 12,
            // sql_variant keeps its base type per value; not compared yet.
            _ => 11,
        }
    }
    family(a) == family(b) && (family(a) != 100 && family(a) != 12 || a.name.eq_ignore_ascii_case(&b.name)) && family(a) != 11
}

/// Types that cannot be key columns.
fn keyable(t: &DataType) -> bool {
    !(t.schema.is_some()
        || matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml" | "sql_variant" | "geography" | "geometry" | "hierarchyid")
        || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1)))
}

/// Pairs the tables (and, with Include views, indexed views) of the two databases.
pub fn map_tables<'a>(db1: &'a Database, db2: &'a Database, o: &DataOptions, user: &DataMappings) -> (Vec<TablePair>, Vec<ObjectName>, Vec<ObjectName>) {
    struct Obj<'a> {
        name: ObjectName,
        columns: Vec<Column>,
        indexes: &'a [Index],
        view: bool,
    }
    // Alias types are compared as their base type.
    let resolve = |db: &Database, cols: &[Column]| -> Vec<Column> {
        cols.iter()
            .map(|c| {
                let mut c = c.clone();
                if let Some(schema) = &c.data_type.schema
                    && let Some(base) = db
                        .types
                        .iter()
                        .find(|t| &t.schema == schema && t.name == c.data_type.name && t.kind == UserTypeKind::Alias)
                        .and_then(|t| t.base_type.clone())
                {
                    c.data_type = base;
                }
                c
            })
            .collect()
    };
    let objects = |db: &'a Database| -> Vec<Obj<'a>> {
        let mut v: Vec<Obj> = db
            .tables
            .iter()
            .filter(|t| !t.history_table)
            .map(|t| Obj { name: t.object_name(), columns: resolve(db, &t.columns), indexes: &t.indexes, view: false })
            .collect();
        if o.has(DataOpt::IncludeViews) {
            v.extend(db.views.iter().filter(|w| !w.indexes.is_empty()).map(|w| Obj {
                name: ObjectName::new(&w.schema, &w.name),
                columns: resolve(db, &w.columns),
                indexes: &w.indexes,
                view: true,
            }));
        }
        v
    };
    let (left, right) = (objects(db1), objects(db2));
    let key = |n: &ObjectName| format!("{}.{}", normalize(&n.schema, o), normalize(&n.name, o));
    let mut used = vec![false; right.len()];
    let mut pairs = Vec::new();
    let mut only1 = Vec::new();
    for l in &left {
        let over = user.tables.iter().find(|t| t.source == l.name);
        if over.is_some_and(|t| t.excluded) {
            continue;
        }
        let wanted = over.and_then(|t| t.target.clone());
        let found = right.iter().enumerate().position(|(j, r)| {
            !used[j] && r.view == l.view && match &wanted {
                Some(w) => r.name == *w,
                None => key(&r.name) == key(&l.name),
            }
        });
        let Some(j) = found else {
            only1.push(l.name.clone());
            continue;
        };
        used[j] = true;
        let r = &right[j];
        pairs.push(pair(l.name.clone(), &l.columns, l.indexes, r.name.clone(), &r.columns, r.indexes, l.view, o, over));
    }
    let only2 = right.iter().enumerate().filter(|(j, _)| !used[*j]).map(|(_, r)| r.name.clone()).collect();
    (pairs, only1, only2)
}

#[allow(clippy::too_many_arguments)]
fn pair(
    n1: ObjectName,
    c1: &[Column],
    i1: &[Index],
    n2: ObjectName,
    c2: &[Column],
    i2: &[Index],
    view: bool,
    o: &DataOptions,
    over: Option<&TableOverride>,
) -> TablePair {
    let excluded = |c: &Column| {
        over.is_some_and(|t| t.excluded_columns.iter().any(|x| x.eq_ignore_ascii_case(&c.name)))
            || (c.identity.is_some() && !o.has(DataOpt::IncludeIdentities))
            || (matches!(c.data_type.name.as_str(), "timestamp" | "rowversion") && !o.has(DataOpt::IncludeTimestamps))
            || (c.generated_always.is_some() && !o.has(DataOpt::IncludeGeneratedAlwaysColumns))
            || c.name.starts_with("graph_id_")
            || c.column_set
    };
    let mut columns = Vec::new();
    let mut skipped = Vec::new();
    for a in c1.iter().filter(|c| !excluded(c)) {
        let Some(b) = c2.iter().find(|b| normalize(&b.name, o) == normalize(&a.name, o) && !excluded(b)) else { continue };
        if !compatible(&a.data_type, &b.data_type) {
            skipped.push(a.name.clone());
            continue;
        }
        columns.push(ColumnPair {
            source: a.name.clone(),
            target: b.name.clone(),
            source_type: a.data_type.clone(),
            target_type: b.data_type.clone(),
            read_only: b.computed.is_some() || matches!(b.data_type.name.as_str(), "timestamp" | "rowversion") || b.generated_always.is_some(),
            identity: b.identity.is_some(),
            case_insensitive: is_case_insensitive(a.collation.as_deref()) || is_case_insensitive(b.collation.as_deref()),
            nullable: b.nullable,
        });
    }
    let mut p = TablePair {
        source: n1,
        target: n2,
        columns,
        key: Vec::new(),
        key_name: None,
        where1: over.and_then(|t| t.where1.clone()),
        where2: over.and_then(|t| t.where2.clone()),
        problem: None,
        is_view: view,
    };
    let index_of = |name: &str, p: &TablePair| p.columns.iter().position(|c| c.source.eq_ignore_ascii_case(name));
    // `index:<name>` picks an index of database 1 by name.
    let by_index: Option<Vec<String>> = over
        .and_then(|t| t.key.first())
        .and_then(|k| k.strip_prefix("index:"))
        .and_then(|name| i1.iter().find(|i| i.name.eq_ignore_ascii_case(name)))
        .map(|ix| ix.columns.iter().map(|c| c.name.clone()).collect());
    let custom_key = by_index.as_ref().or(over.map(|t| &t.key)).filter(|k| !k.is_empty());
    if let Some(custom) = custom_key {
        p.key = custom.iter().filter_map(|k| index_of(k, &p)).collect();
        p.key_name = Some("custom".into());
        if p.key.len() != custom.len() {
            p.problem = Some("a custom key column is not compared in both databases".into());
        }
    } else {
        // The primary key on both sides, else the first unique, unfiltered index both share.
        let usable = |ix: &Index| (ix.primary_key || ix.unique) && ix.filter.is_none() && !ix.columns.is_empty() && !ix.disabled;
        let mut candidates: Vec<&Index> = i1.iter().filter(|i| usable(i)).collect();
        candidates.sort_by(|a, b| b.primary_key.cmp(&a.primary_key).then(a.name.cmp(&b.name)));
        for ix in candidates {
            let cols: Vec<String> = ix.columns.iter().map(|c| normalize(&c.name, o)).collect();
            let other = i2.iter().filter(|i| usable(i)).any(|j| j.columns.iter().map(|c| normalize(&c.name, o)).collect::<Vec<_>>() == cols);
            let key: Option<Vec<usize>> = ix.columns.iter().map(|c| index_of(&c.name, &p)).collect();
            if other && let Some(key) = key {
                p.key = key;
                p.key_name = Some(ix.name.clone());
                break;
            }
        }
        if p.key.is_empty() {
            p.problem = Some("no primary key or unique index on the same columns in both databases; choose key columns".into());
        }
    }
    if let Some(bad) = p.key.iter().map(|&k| &p.columns[k]).find(|c| !keyable(&c.source_type) || !keyable(&c.target_type)) {
        p.problem = Some(format!("column {} cannot be part of the comparison key", bad.source));
    }
    if p.columns.len() <= p.key.len() && p.problem.is_none() && p.columns.is_empty() {
        p.problem = Some("no columns are compared".into());
    }
    let _ = skipped;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, t: &str) -> Column {
        Column { name: name.into(), data_type: DataType { name: t.into(), ..Default::default() }, ..Default::default() }
    }

    fn table(name: &str, cols: Vec<Column>, pk: &[&str]) -> Table {
        Table {
            schema: "dbo".into(),
            name: name.into(),
            columns: cols,
            indexes: vec![Index {
                name: format!("PK_{name}"),
                primary_key: true,
                unique: true,
                columns: pk.iter().map(|c| IndexColumn { name: c.to_string(), descending: false }).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn pairs_tables_columns_and_keys() {
        let db1 = Database { tables: vec![table("Order Line", vec![col("Id", "int"), col("Qty", "int"), col("Note", "nvarchar")], &["Id"])], ..Default::default() };
        let db2 = Database { tables: vec![table("orderline", vec![col("ID", "bigint"), col("Qty", "int"), col("Note", "datetime")], &["ID"])], ..Default::default() };
        let (pairs, only1, only2) = map_tables(&db1, &db2, &DataOptions::default_set(), &DataMappings::default());
        assert!(only1.is_empty() && only2.is_empty());
        let p = &pairs[0];
        assert_eq!(p.columns.iter().map(|c| c.source.as_str()).collect::<Vec<_>>(), ["Id", "Qty"], "Note's types don't match");
        assert_eq!(p.key, [0]);
        assert_eq!(p.key_name.as_deref(), Some("PK_Order Line"));
        assert!(p.problem.is_none());
    }

    #[test]
    fn no_key_is_a_problem_unless_custom() {
        let mut t = table("T", vec![col("A", "int"), col("B", "int")], &[]);
        t.indexes.clear();
        let db = Database { tables: vec![t], ..Default::default() };
        let (pairs, _, _) = map_tables(&db, &db, &DataOptions::default_set(), &DataMappings::default());
        assert!(pairs[0].problem.is_some());
        let user = DataMappings {
            tables: vec![TableOverride { source: ObjectName::new("dbo", "T"), key: vec!["B".into()], ..Default::default() }],
        };
        let (pairs, _, _) = map_tables(&db, &db, &DataOptions::default_set(), &user);
        assert!(pairs[0].problem.is_none());
        assert_eq!(pairs[0].key, [1]);
    }
}
