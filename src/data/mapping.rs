//! Which tables and columns of the two databases are compared, and by which key.
//!
//! Schemas pair first, then tables (and views) within a schema pair, then columns, all by
//! normalised name; a name that normalises the same for two objects on one side pairs neither.
//! User mappings (owners, tables, columns) override the automatic pairing.

use serde::{Deserialize, Serialize};

use super::options::{DataOpt, DataOptions};
use crate::catalog::*;
use crate::schema::deploy::Severity;

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
    /// The database 2 index paired with `key_name`.
    #[serde(default)]
    pub target_key_name: Option<String>,
    pub where1: Option<String>,
    pub where2: Option<String>,
    /// Why the pair cannot be compared (no usable key, no common columns).
    pub problem: Option<String>,
    pub is_view: bool,
    /// Columns left out of the comparison and other mapping warnings.
    #[serde(default)]
    pub notes: Vec<Note>,
    /// Why the pair is left out of the comparison altogether (a user mapping that no longer
    /// applies, or something the data sources cannot compare), as the classic data compare tool words it.
    #[serde(default)]
    pub excluded: Option<String>,
    /// MissingFrom2AsInclude: the table is only in database 1 and is paired with itself; database
    /// 2 has no rows for it.
    #[serde(default)]
    pub target_missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    /// `type`, `missing`, `exclude`, `collation`, `truncate`, `rounding`, `overflow`,
    /// `xmlconversion`, `xmlschema`, `guidcomparisonkey`, `clr`.
    pub code: String,
    pub severity: Severity,
    pub text: String,
}

impl Note {
    /// The column is not compared (rather than a warning about a compared one).
    pub fn not_compared(&self) -> bool {
        matches!(self.code.as_str(), "type" | "missing" | "exclude" | "computed")
    }
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
    #[serde(default)]
    pub source_collation: Option<String>,
    #[serde(default)]
    pub target_collation: Option<String>,
    /// The database 1 column is an identity column too.
    #[serde(default)]
    pub source_identity: bool,
    /// Graph pseudo-column kind: `$node_id`, `$edge_id`, `$from_id` or `$to_id`.
    #[serde(default)]
    pub graph: Option<String>,
    /// The database 1 column has a data mask.
    #[serde(default)]
    pub masked: bool,
}

impl ColumnPair {
    /// The collation values are compared by: database 1's, else database 2's.
    pub fn collation(&self) -> Option<&str> {
        self.source_collation.as_deref().or(self.target_collation.as_deref())
    }
}

/// User choices that override the automatic mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DataMappings {
    pub tables: Vec<TableOverride>,
    /// Owner (schema) mappings: database 1 schema → database 2 schema.
    pub schemas: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TableOverride {
    pub source: ObjectName,
    /// A different table in database 2.
    pub target: Option<ObjectName>,
    /// Leave the table out.
    pub excluded: bool,
    /// Key columns (source names) to use instead of the primary key, or `index:<name>`.
    pub key: Vec<String>,
    /// Columns (source names) left out of the comparison.
    pub excluded_columns: Vec<String>,
    /// Column mappings: database 1 column → database 2 column.
    pub columns: Vec<(String, String)>,
    pub where1: Option<String>,
    pub where2: Option<String>,
}

const SPACES: &[char] = &[
    ' ', '\u{a0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}',
    '\u{200a}', '\u{200b}', '\u{3000}',
];
const UNDERSCORES: &[char] = &['_', '\u{203f}', '\u{2040}', '\u{fe33}', '\u{fe34}', '\u{fe4d}', '\u{fe4e}', '\u{fe4f}', '\u{ff3f}'];

/// The name used for pairing: without spaces (and underscores), lower-case unless case-sensitive.
pub fn normalize(s: &str, o: &DataOptions) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !(o.has(DataOpt::IgnoreSpaces) && SPACES.contains(c)) && !(o.has(DataOpt::IgnoreUnderscores) && UNDERSCORES.contains(c)))
        .collect();
    if !o.has(DataOpt::CaseSensitiveObjectDefinition) {
        out = out.to_lowercase();
    }
    out
}

/// Whether a user mapping's object name means this object: ignoring case unless object
/// definitions are case-sensitive.
pub fn same_object(a: &ObjectName, b: &ObjectName, o: &DataOptions) -> bool {
    if o.has(DataOpt::CaseSensitiveObjectDefinition) {
        a == b
    } else {
        a.schema.to_lowercase() == b.schema.to_lowercase() && a.name.to_lowercase() == b.name.to_lowercase()
    }
}

/// The user mapping of a database 1 object.
pub fn override_of<'a>(user: &'a DataMappings, source: &ObjectName, o: &DataOptions) -> Option<&'a TableOverride> {
    user.tables.iter().find(|t| same_object(&t.source, source, o))
}

fn is_case_insensitive(collation: Option<&str>) -> bool {
    collation.is_some_and(|c| {
        let c = c.to_ascii_uppercase();
        c.contains("_CI") && !c.contains("_BIN")
    })
}

const GRAPH_INTERNAL: &[&str] = &["graph_id_", "from_obj_id_", "from_id_", "to_obj_id_", "to_id_"];
const GRAPH_COMPUTED: &[&str] = &["$node_id", "$edge_id", "$from_id", "$to_id"];

/// `$node_id_<32 hex>` → `$node_id`.
pub fn graph_kind(column: &str) -> Option<&'static str> {
    GRAPH_COMPUTED.iter().copied().find(|k| column.strip_prefix(k).is_some_and(|rest| rest.starts_with('_') && rest.len() == 33))
}

fn graph_internal(c: &Column) -> bool {
    c.hidden && GRAPH_INTERNAL.iter().any(|p| c.name.starts_with(p))
}

/// The type group of the compatibility matrix.
fn group(t: &DataType) -> &str {
    if t.schema.is_some() {
        return "clr";
    }
    match t.name.as_str() {
        "hierarchyid" | "geometry" | "geography" => "clr",
        "rowversion" => "timestamp",
        "sysname" => "nvarchar",
        other => other,
    }
}

/// For each database 1 type, the database 2 types it compares with and the checks applied
/// (DS size, P precision, S scale, Col collation, OL/OR overflow, TL/TR truncation, CL/CR CLR,
/// G guid key, XL/XR XML conversion, XS XML schema, TC type conversion). From
/// specs/data-compare/matrix.md.
const MATRIX: &[(&str, &str)] = &[
    ("binary", "binary;image;varbinary DS"),
    ("bit", "bit"),
    ("tinyint", "tinyint;smallint OL;int OL;bigint OL;decimal P S;float OL TR;money OL TR;numeric OL TR;real OL TR;smallmoney OL TR;nvarchar DS;varchar DS;nchar DS;char DS"),
    ("char", "char DS Col;clr TL NUL CR;uniqueidentifier DS G;nchar DS Col NUL;ntext Col TL NUL;nvarchar DS Col NUL;text Col TL;varchar DS Col;xml TL NUL XR"),
    ("clr", "clr CL CR;char TR NUR CL;uniqueidentifier TR NUR G CL;nchar TR CL;ntext CL;nvarchar TR CL;text NUR CL;varchar TR NUR CL;xml XR CL"),
    ("datetime", "datetime;smalldatetime TR;date TR;time TR;datetime2 P S;datetimeoffset P S TL"),
    ("decimal", "decimal P S;numeric P S;money OR;smallmoney OR;tinyint P S;smallint P S;int P S;bigint P S;float P S;real P S"),
    ("float", "float;decimal P S;numeric P S;money OR;smallmoney OR;tinyint TL OR;smallint TL OR;int TL OR;bigint TL OR;real TL OR"),
    ("uniqueidentifier", "char DS G;uniqueidentifier;clr TL NUL G CR;nchar DS NUL G;ntext NUL G;nvarchar DS NUL G;text TL G;varchar DS G"),
    ("image", "image;binary;varbinary DS"),
    ("smallint", "smallint;tinyint OR;int OL;bigint OL;decimal P S;float OL TR;money P S;numeric P S;real OL TR;smallmoney P S;nvarchar DS;varchar DS;nchar DS;char DS"),
    ("int", "int;tinyint OR;smallint OR;bigint OL;decimal P S;float OL TR;money P S;numeric P S;real OL TR;smallmoney P S;nvarchar DS;varchar DS;nchar DS;char DS"),
    ("bigint", "bigint;tinyint OR;smallint OR;int OR;decimal P S;float OL TR;money P S;numeric P S;real OL TR;smallmoney P S;nvarchar DS;varchar DS;nchar DS;char DS"),
    ("money", "money;decimal P S;numeric P S;smallmoney OR;tinyint P S;float P S;smallint P S;int P S;bigint P S;real P S"),
    ("nchar", "nchar DS Col;char DS Col NUR;clr TL CR;uniqueidentifier DS NUR G;ntext Col TL;nvarchar DS Col;text Col TL NUR;varchar DS Col NUR;xml TL XR"),
    ("ntext", "ntext Col;char Col TR NUR;clr CR;uniqueidentifier TR NUR G;nchar Col TR;nvarchar DS Col;text Col NUR;varchar DS Col NUR;xml XR"),
    ("numeric", "numeric P S;decimal P S;money OR;smallmoney OR;float P S;real P S;tinyint P S;smallint P S;int P S;bigint P S"),
    ("nvarchar", "nvarchar DS Col;char DS Col NUR;clr DS CR;uniqueidentifier DS NUR G;nchar DS Col;ntext DS Col;text DS Col NUR;varchar DS Col NUR;xml DS XR;int TC"),
    ("real", "real;numeric P S;decimal P S;money P S;smallmoney P S;tinyint TR OR;smallint TR OR;int S OR;bigint S OR;float OR"),
    ("smalldatetime", "smalldatetime;datetime TL;date TR;time TR;datetime2 TL;datetimeoffset TL"),
    ("smallmoney", "smallmoney;money OL;decimal P S;numeric P S;real P S;tinyint P S;smallint P S;int P S;bigint P S;float P S"),
    ("text", "text Col;char Col TR;clr NUL CR;uniqueidentifier TR G;nchar Col NUL TR;nvarchar DS Col NUL;ntext Col NUL;varchar DS Col;xml NUL XR"),
    ("timestamp", "timestamp"),
    ("varbinary", "varbinary DS;image DS;binary DS"),
    ("varchar", "varchar DS Col;char DS Col;clr NUL TR CR;uniqueidentifier TR G;nchar DS Col NUL;nvarchar DS Col NUL;ntext DS Col NUL;text DS Col;xml DS NUL XR"),
    ("sql_variant", "sql_variant"),
    ("xml", "xml XS;char XL TR NUR;clr XL CR;nchar XL TR;nvarchar DS XL;ntext XL;text XL NUR;varchar DS XL NUR"),
    ("date", "date;datetime TL;smalldatetime TL;datetime2 TL;datetimeoffset TL"),
    ("time", "time P S;datetime TL;smalldatetime TL;datetime2 TL;datetimeoffset TL"),
    ("datetime2", "datetime2 P S;date TR;time TR;datetime P S;smalldatetime TR;datetimeoffset P S TL"),
    ("datetimeoffset", "datetimeoffset P S;date TR;time TR;datetime TR;smalldatetime TR;datetime2 P S TR"),
];

/// The result of checking a column pair's types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeCheck {
    pub flags: Vec<&'static str>,
}

impl TypeCheck {
    pub fn has(&self, f: &str) -> bool {
        self.flags.contains(&f)
    }
}

fn str_size(t: &DataType) -> Option<i32> {
    match t.name.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => t.length,
        "text" | "ntext" | "image" | "xml" => Some(-1),
        _ if group(t) == "clr" => Some(-1),
        "sysname" => Some(128),
        _ => None,
    }
}

/// Whether a database 1 column of type `a` compares with a database 2 column of type `b`, and
/// the warnings that apply (`None` = not comparable).
pub fn type_check(a: &DataType, b: &DataType, c1: Option<&str>, c2: Option<&str>) -> Option<TypeCheck> {
    let (ga, gb) = (group(a), group(b));
    let row = MATRIX.iter().find(|(s, _)| *s == ga)?.1;
    let entry = row.split(';').find(|e| e.split(' ').next() == Some(gb))?;
    let statics: Vec<&str> = entry.split(' ').skip(1).collect();
    if ga == "clr" && gb == "clr" && !a.name.eq_ignore_ascii_case(&b.name) {
        return None;
    }
    let mut out = TypeCheck::default();
    let add = |f: &'static str, out: &mut TypeCheck| {
        if !out.flags.contains(&f) {
            out.flags.push(f);
        }
    };
    for s in &statics {
        match *s {
            "OL" => add("OverflowLeft", &mut out),
            "OR" => add("OverflowRight", &mut out),
            "TL" => add("TruncateLeft", &mut out),
            "TR" => add("TruncateRight", &mut out),
            "CL" | "CR" => add("Clr", &mut out),
            "G" => add("GuidComparisonKey", &mut out),
            "XL" => add("XmlConversionLeft", &mut out),
            "XR" => add("XmlConversionRight", &mut out),
            "TC" => add("TypeConversion", &mut out),
            "Col" if c1 != c2 => add("Collation", &mut out),
            "P" => match (a.precision, b.precision) {
                (Some(p1), Some(p2)) if p1 < p2 => add("OverflowLeft", &mut out),
                (Some(p1), Some(p2)) if p1 > p2 => add("OverflowRight", &mut out),
                _ => {}
            },
            "S" => match (a.scale, b.scale) {
                (Some(s1), Some(s2)) if s1 < s2 => add("RoundingLeft", &mut out),
                (Some(s1), Some(s2)) if s1 > s2 => add("RoundingRight", &mut out),
                _ => {}
            },
            "XS" if a.xml_collection.is_some() || b.xml_collection.is_some() => {
                if a.xml_collection.is_some() {
                    add("XmlSchemaLeft", &mut out);
                }
                if b.xml_collection.is_some() {
                    add("XmlSchemaRight", &mut out);
                }
            }
            "DS" => {
                const SIZED: [&str; 6] = ["char", "nchar", "varchar", "nvarchar", "binary", "varbinary"];
                let int_text = |t: &str| match t {
                    "tinyint" => Some(1),
                    "smallint" => Some(6),
                    "int" => Some(11),
                    "bigint" => Some(20),
                    _ => None,
                };
                let (s1, s2) = (str_size(a), str_size(b));
                if ga == gb || (SIZED.contains(&ga) && SIZED.contains(&gb)) {
                    if let (Some(x), Some(y)) = (s1, s2) {
                        if x < y && x != -1 {
                            add("TruncateLeft", &mut out);
                        } else if x > y && y != -1 || (x == -1 && y != -1) {
                            add("TruncateRight", &mut out);
                        }
                    }
                } else if ga == "uniqueidentifier" {
                    if s2.is_some_and(|y| y != -1 && y < 36) {
                        add("TruncateRight", &mut out);
                    }
                } else if gb == "uniqueidentifier" {
                    if s1.is_some_and(|x| x != -1 && x < 36) {
                        add("TruncateLeft", &mut out);
                    }
                } else if let (Some(n), Some(y)) = (int_text(ga), s2) {
                    if y != -1 && y < n {
                        add("TruncateRight", &mut out);
                    }
                } else if s1 == Some(-1) && s2.is_some_and(|y| y != -1) {
                    add("TruncateRight", &mut out);
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Types that cannot be key columns.
pub fn keyable(t: &DataType) -> bool {
    !(matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml" | "sql_variant")
        || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1)))
}

struct Obj<'a> {
    name: ObjectName,
    columns: Vec<Column>,
    indexes: &'a [Index],
    view: bool,
    graph: Option<&'a str>,
}

/// Alias types are compared as their base type.
fn resolve(db: &Database, cols: &[Column]) -> Vec<Column> {
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
}

fn objects<'a>(db: &'a Database, o: &DataOptions) -> Vec<Obj<'a>> {
    let mut v: Vec<Obj> = db
        .tables
        .iter()
        .filter(|t| !t.history_table)
        .map(|t| Obj { name: t.object_name(), columns: resolve(db, &t.columns), indexes: &t.indexes, view: false, graph: t.graph.as_deref() })
        .collect();
    if o.has(DataOpt::IncludeViews) {
        v.extend(db.views.iter().map(|w| Obj {
            name: ObjectName::new(&w.schema, &w.name),
            columns: resolve(db, &w.columns),
            indexes: &w.indexes,
            view: true,
            graph: None,
        }));
    }
    v.retain(|x| x.name.schema != "sys" && x.name.schema != "INFORMATION_SCHEMA");
    v
}

/// A temporal period column (GENERATED ALWAYS AS ROW START/END).
fn period(c: &Column) -> Option<&str> {
    c.generated_always.as_deref().filter(|g| matches!(*g, "ROW START" | "ROW END"))
}

/// Pairs the tables (and, with Include views, views) of the two databases. Returns the pairs
/// and the objects only in database 1 and only in database 2.
pub fn map_tables(db1: &Database, db2: &Database, o: &DataOptions, user: &DataMappings) -> (Vec<TablePair>, Vec<ObjectName>, Vec<ObjectName>) {
    let (left, right) = (objects(db1, o), objects(db2, o));

    // Schemas: user mappings first, then by normalised name (ambiguous names stay unpaired).
    let schemas = |v: &[Obj]| -> Vec<String> {
        let mut s: Vec<String> = v.iter().map(|x| x.name.schema.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    let (s1, s2) = (schemas(&left), schemas(&right));
    let mut schema_pairs: Vec<(String, String)> = Vec::new();
    for (a, b) in &user.schemas {
        let a = s1.iter().find(|x| x.eq_ignore_ascii_case(a));
        let b = s2.iter().find(|x| x.eq_ignore_ascii_case(b));
        if let (Some(a), Some(b)) = (a, b) {
            schema_pairs.retain(|(x, y)| x != a && y != b);
            schema_pairs.push((a.clone(), b.clone()));
        }
    }
    for (a, b) in pair_names(&s1, &s2, |s| normalize(s, o)) {
        if !schema_pairs.iter().any(|(x, y)| *x == s1[a] || *y == s2[b]) {
            schema_pairs.push((s1[a].clone(), s2[b].clone()));
        }
    }

    // Tables within each schema pair, by normalised name; tables only pair with tables.
    let mut matched: Vec<(usize, usize)> = Vec::new();
    for (a, b) in &schema_pairs {
        for view in [false, true] {
            let li: Vec<usize> = (0..left.len()).filter(|&i| left[i].name.schema == *a && left[i].view == view).collect();
            let ri: Vec<usize> = (0..right.len()).filter(|&j| right[j].name.schema == *b && right[j].view == view).collect();
            let ln: Vec<String> = li.iter().map(|&i| left[i].name.name.clone()).collect();
            let rn: Vec<String> = ri.iter().map(|&j| right[j].name.name.clone()).collect();
            for (x, y) in pair_names(&ln, &rn, |s| normalize(s, o)) {
                matched.push((li[x], ri[y]));
            }
        }
    }
    // User table mappings displace whatever either side was paired with.
    for t in user.tables.iter().filter(|t| t.target.is_some()) {
        let want = t.target.as_ref().unwrap();
        // An empty target leaves the table unmapped.
        if want.name.is_empty() {
            if let Some(i) = left.iter().position(|x| same_object(&x.name, &t.source, o)) {
                matched.retain(|&(x, _)| x != i);
            }
            continue;
        }
        // A mapping whose tables are gone leaves the pairing by name in place.
        let (Some(i), Some(j)) = (left.iter().position(|x| same_object(&x.name, &t.source, o)), right.iter().position(|x| same_object(&x.name, want, o))) else { continue };
        matched.retain(|&(x, y)| x != i && y != j);
        matched.push((i, j));
    }
    matched.sort_by(|a, b| left[a.0].name.cmp(&left[b.0].name));

    let mut pairs = Vec::new();
    for &(i, j) in &matched {
        let (l, r) = (&left[i], &right[j]);
        let over = override_of(user, &l.name, o);
        if over.is_some_and(|t| t.excluded) {
            continue;
        }
        pairs.push(pair(l, r, o, over));
    }
    let only1 = (0..left.len())
        .filter(|i| !matched.iter().any(|m| m.0 == *i) && !override_of(user, &left[*i].name, o).is_some_and(|t| t.excluded))
        .map(|i| left[i].name.clone())
        .collect();
    let only2 = (0..right.len()).filter(|j| !matched.iter().any(|m| m.1 == *j)).map(|j| right[j].name.clone()).collect();
    (pairs, only1, only2)
}

/// The user mappings that name a table or pair that does not exist, in the classic data compare tool's words
/// (the comparison goes on without them).
pub fn user_mapping_warnings(db1: &Database, db2: &Database, o: &DataOptions, user: &DataMappings) -> Vec<String> {
    let (left, right) = (objects(db1, o), objects(db2, o));
    let (pairs, _, _) = map_tables(db1, db2, o, user);
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    let has_schema = |v: &[Obj], s: &str| v.iter().any(|x| x.name.schema.eq_ignore_ascii_case(s));
    for (a, b) in &user.schemas {
        if !has_schema(&left, a) || !has_schema(&right, b) {
            add("Unable to map owners. Owners do not exist.");
        }
    }
    for t in &user.tables {
        let source = left.iter().any(|x| same_object(&x.name, &t.source, o));
        if let Some(w) = &t.target {
            if w.name.is_empty() {
                if !source {
                    add("Unable to unmap tables. The tables don't exist or have not been mapped.");
                }
            } else if !source || !right.iter().any(|x| same_object(&x.name, w, o)) {
                add("Unable to map tables. The tables don't exist or have not been unmapped.");
            }
        }
        // The settings of a table that is not paired cannot apply.
        if (t.excluded && source) || pairs.iter().any(|p| same_object(&p.source, &t.source, o)) {
            continue;
        }
        if t.excluded {
            add("Unable to deselect comparison of table mapping. Table mapping does not exist.");
        }
        if t.where1.as_deref().is_some_and(|w| !w.trim().is_empty()) {
            add("Unable to set left where clause for comparison of table mapping. Table mapping does not exist.");
        }
        if t.where2.as_deref().is_some_and(|w| !w.trim().is_empty()) {
            add("Unable to set right where clause for comparison of table mapping. Table mapping does not exist.");
        }
        match t.key.first() {
            Some(k) if k.starts_with("index:") => add("Unable to use index for table mapping. Table mapping does not exist."),
            Some(_) => add("Unable to use column for custom comparison of table mapping. Table mapping does not exist."),
            None => {}
        }
        if !t.excluded_columns.is_empty() {
            add("Unable to deselect column of table mapping. Table mapping does not exist.");
        }
        if !t.columns.is_empty() {
            add("Unable to map columns of table mapping. Table mapping does not exist.");
        }
    }
    out
}

/// Pairs equal keys 1:1; a key that occurs twice on either side pairs nothing.
fn pair_names(a: &[String], b: &[String], key: impl Fn(&str) -> String) -> Vec<(usize, usize)> {
    let ka: Vec<String> = a.iter().map(|s| key(s)).collect();
    let kb: Vec<String> = b.iter().map(|s| key(s)).collect();
    let mut out = Vec::new();
    for (i, k) in ka.iter().enumerate() {
        if ka.iter().filter(|x| *x == k).count() != 1 {
            continue;
        }
        let hits: Vec<usize> = (0..kb.len()).filter(|&j| kb[j] == *k).collect();
        if let [j] = hits[..] {
            out.push((i, j));
        }
    }
    out
}

/// The name a column pairs by: graph pseudo-columns by their kind.
fn column_key(c: &Column, o: &DataOptions) -> String {
    graph_kind(&c.name).map(str::to_string).unwrap_or_else(|| normalize(&c.name, o))
}

fn pair(l: &Obj, r: &Obj, o: &DataOptions, over: Option<&TableOverride>) -> TablePair {
    let (c1, c2) = (&l.columns, &r.columns);
    // Columns that are not mapped at all.
    let unmapped = |c: &Column| {
        (c.identity.is_some() && !o.has(DataOpt::IncludeIdentities))
            || (matches!(c.data_type.name.as_str(), "timestamp" | "rowversion") && !o.has(DataOpt::IncludeTimestamps))
            || c.column_set
            || graph_internal(c)
    };
    let fq = |t: &ObjectName, c: &str| format!("{}.{}", t.quoted(), crate::db::quote(c));
    let mut notes = Vec::new();
    let note = |notes: &mut Vec<Note>, code: &str, text: String| notes.push(Note { code: code.into(), severity: Severity::High, text });

    // Pair columns: user column mappings, then by name.
    let left_cols: Vec<&Column> = c1.iter().filter(|c| !unmapped(c)).collect();
    let right_cols: Vec<&Column> = c2.iter().filter(|c| !unmapped(c)).collect();
    let mut links: Vec<(usize, usize)> = Vec::new();
    let user_cols: &[(String, String)] = over.map(|t| t.columns.as_slice()).unwrap_or_default();
    let ln: Vec<String> = left_cols.iter().map(|c| column_key(c, o)).collect();
    let rn: Vec<String> = right_cols.iter().map(|c| column_key(c, o)).collect();
    for (a, b) in pair_names(&ln, &rn, |s| s.to_string()) {
        links.push((a, b));
    }
    let mut excluded = None;
    for (a, b) in user_cols {
        let (Some(i), Some(j)) = (left_cols.iter().position(|c| c.name.eq_ignore_ascii_case(a)), right_cols.iter().position(|c| c.name.eq_ignore_ascii_case(b)))
        else {
            excluded.get_or_insert_with(|| "Unable to map columns of table mapping. Columns do not exist.".to_string());
            continue;
        };
        links.retain(|&(x, y)| x != i && y != j);
        links.push((i, j));
    }
    links.sort();

    let mut columns = Vec::new();
    for &(i, j) in &links {
        let (a, b) = (left_cols[i], right_cols[j]);
        if over.is_some_and(|t| t.excluded_columns.iter().any(|x| x.eq_ignore_ascii_case(&a.name))) {
            continue;
        }
        let Some(check) = type_check(&a.data_type, &b.data_type, a.collation.as_deref(), b.collation.as_deref()) else {
            note(
                &mut notes,
                "type",
                format!("The fields {} and {} have incomparable types {} and {}.", fq(&l.name, &a.name), fq(&r.name, &b.name), a.data_type.sql(), b.data_type.sql()),
            );
            continue;
        };
        if !o.has(DataOpt::IncludeGeneratedAlwaysColumns)
            && let Some((g, col)) = period(a).map(|g| (g, a)).or_else(|| period(b).map(|g| (g, b)))
        {
            let kind = if g == "ROW START" { "AsRowStart" } else { "AsRowEnd" };
            note(&mut notes, "exclude", format!("Excluded {} because it is GENERATED ALWAYS AS ${kind}", col.name));
            continue;
        }
        for n in type_notes(&check, &fq(&l.name, &a.name), &fq(&r.name, &b.name), &l.name, false, o) {
            notes.push(n);
        }
        columns.push(ColumnPair {
            source: a.name.clone(),
            target: b.name.clone(),
            source_type: a.data_type.clone(),
            target_type: b.data_type.clone(),
            read_only: b.computed.is_some() || matches!(b.data_type.name.as_str(), "timestamp" | "rowversion") || b.generated_always.is_some() || b.column_set,
            identity: b.identity.is_some(),
            case_insensitive: is_case_insensitive(a.collation.as_deref().or(b.collation.as_deref())),
            nullable: b.nullable,
            source_collation: a.collation.clone(),
            target_collation: b.collation.clone(),
            source_identity: a.identity.is_some(),
            graph: graph_kind(&b.name).map(str::to_string),
            masked: a.masking_function.is_some(),
        });
    }
    for (i, a) in left_cols.iter().enumerate() {
        if !links.iter().any(|x| x.0 == i) {
            note(&mut notes, "missing", format!("The column {} has no matching column in {}; it is not compared.", fq(&l.name, &a.name), r.name));
        }
    }
    for (j, b) in right_cols.iter().enumerate() {
        if !links.iter().any(|x| x.1 == j) {
            note(&mut notes, "missing", format!("The column {} has no matching column in {}; it is not compared.", fq(&r.name, &b.name), l.name));
        }
    }

    let mut p = TablePair {
        source: l.name.clone(),
        target: r.name.clone(),
        columns,
        key: Vec::new(),
        key_name: None,
        target_key_name: None,
        where1: over.and_then(|t| t.where1.clone()),
        where2: over.and_then(|t| t.where2.clone()),
        problem: None,
        is_view: l.view,
        notes,
        excluded,
        target_missing: false,
    };
    let index_of = |name: &str, p: &TablePair| p.columns.iter().position(|c| c.source.eq_ignore_ascii_case(name));
    let paired = index_pairs(l, r, &p);
    let custom = over.map(|t| t.key.as_slice()).filter(|k| !k.is_empty());
    if let Some(custom) = custom {
        if let Some(name) = custom[0].strip_prefix("index:") {
            match paired.iter().find(|(a, _)| a.name.eq_ignore_ascii_case(name)) {
                Some((a, b)) => use_index(&mut p, a, b),
                None => {
                    p.problem = Some(format!("the index {name} is not on the same columns in both databases"));
                    p.excluded.get_or_insert_with(|| "Unable to use index for table mapping. Index mapping does not exist.".to_string());
                }
            }
        } else {
            p.key = custom.iter().filter_map(|k| index_of(k, &p)).collect();
            p.key_name = Some("custom".into());
            if p.key.len() != custom.len() {
                p.problem = Some("a custom key column is not compared in both databases".into());
                if custom.iter().any(|k| !c1.iter().any(|c| c.name.eq_ignore_ascii_case(k))) {
                    p.excluded.get_or_insert_with(|| "Unable to use column for custom comparison of table mapping. Column does not exist.".to_string());
                }
            }
        }
    } else {
        let suitable = |ix: &Index| ((ix.unique || ix.unique_constraint) && ix.filter.is_none()) || ix.kind == IndexKind::ClusteredColumnstore;
        let pk = paired.iter().find(|(a, b)| a.primary_key && b.primary_key);
        let mut by_name: Vec<&(&Index, &Index)> = paired.iter().filter(|(a, b)| suitable(a) && suitable(b)).collect();
        by_name.sort_by(|x, y| x.0.name.cmp(&y.0.name));
        if let Some((a, b)) = pk.or(by_name.first().copied()) {
            use_index(&mut p, a, b);
        } else if let (Some(g1), Some(g2)) = (l.graph, r.graph)
            && g1 == g2
        {
            let kind = if g1 == "NODE" { "$node_id" } else { "$edge_id" };
            if let Some(k) = p.columns.iter().position(|c| c.graph.as_deref() == Some(kind)) {
                p.key = vec![k];
                p.key_name = Some(kind.into());
            }
        }
        if p.key.is_empty() {
            p.problem = Some("no primary key or unique index on the same columns in both databases; choose key columns".into());
        }
    }
    if p.problem.is_none()
        && let Some(bad) = p.key.iter().map(|&k| &p.columns[k]).find(|c| !keyable(&c.source_type) || !keyable(&c.target_type))
    {
        p.problem = Some(format!("column {} cannot be part of the comparison key", bad.source));
    }
    if p.problem.is_none() && p.columns.is_empty() {
        p.problem = Some("no columns are compared".into());
    }
    if p.problem.is_none() {
        for &k in &p.key {
            let c = &p.columns[k];
            if let Some(check) = type_check(&c.source_type, &c.target_type, None, None)
                && check.has("GuidComparisonKey")
            {
                p.notes.push(Note {
                    code: "guidcomparisonkey".into(),
                    severity: Severity::High,
                    text: format!(
                        "When using a uniqueidentifier as part of a comparison key and both fields are not uniqueidentifiers there may be unexpected behaviour. Only one of the fields {} and {} is a unique identifier.",
                        fq(&p.source, &c.source),
                        fq(&p.target, &c.target)
                    ),
                });
            }
        }
    }
    p
}

/// The warnings a comparable column pair raises: both halves at mapping time, only the
/// target half (with the table in the text) when the script is written.
pub fn type_notes(check: &TypeCheck, f1: &str, f2: &str, table: &ObjectName, script: bool, o: &DataOptions) -> Vec<Note> {
    let mut out = Vec::new();
    let any = |a: &str, b: &str| check.has(b) || (!script && check.has(a));
    let (on, sep) = if script { (format!(" on table {table}"), ";") } else { (String::new(), ",") };
    let mut push = |code: &str, text: String| out.push(Note { code: code.into(), severity: Severity::High, text });
    if check.has("Collation") && !o.has(DataOpt::ForceBinaryCollation) {
        push("collation", format!("Collations on the fields {f1} and {f2} on table {table} vary; results may be unpredictable -  consider using the Force Binary Collation option."));
    }
    if any("TruncateLeft", "TruncateRight") {
        push("truncate", format!("Character fields {f1} and {f2}{on} have different sizes{sep} migrating the data may cause truncation."));
    }
    if any("RoundingLeft", "RoundingRight") {
        push("rounding", format!("Decimal or numeric fields {f1} and {f2}{on} have difference scales{sep} migrating the data may cause rounding."));
    }
    if any("OverflowLeft", "OverflowRight") {
        push("overflow", format!("Decimal or numeric fields {f1} and {f2}{on} have different precisions{sep} migrating the data may cause overflows."));
    }
    if any("XmlConversionLeft", "XmlConversionRight") {
        let fragment = if script { "" } else { " fragment" };
        push("xmlconversion", format!("A conversion to XML will occur when migrating from fields {f1} to {f2}{on}{sep} if the data isn't valid XML{fragment} this may cause errors."));
    }
    if any("XmlSchemaLeft", "XmlSchemaRight") {
        push("xmlschema", format!("The XML fields {f1} and {f2}{on} have XML schemas associated with them{sep} if the migrating data is not valid against the schema of the other, errors will occur."));
    }
    if !script && check.has("Clr") {
        push("clr", "Because CLR columns can contain highly variable data,\r\ncomparison and deployment to other data types may\r\nfail or produce unexpected results.".into());
    }
    out
}

/// The relational indexes of the two sides that pair (same structure, mapped columns).
fn index_pairs<'a>(l: &'a Obj, r: &'a Obj, p: &TablePair) -> Vec<(&'a Index, &'a Index)> {
    let eligible = |ix: &Index| !ix.disabled && !matches!(ix.kind, IndexKind::Xml | IndexKind::Spatial) && ix.filter.is_none();
    // Key columns, then included ones, with (descending, included) flags.
    let fields = |ix: &'a Index| -> Vec<(&'a str, bool, bool)> {
        let mut v: Vec<(&str, bool, bool)> = ix.columns.iter().map(|c| (c.name.as_str(), c.descending, false)).collect();
        v.extend(ix.included.iter().map(|c| (c.as_str(), false, true)));
        v
    };
    let column = |name: &str| p.columns.iter().find(|c| c.source.eq_ignore_ascii_case(name));
    let valid_left = |ix: &'a Index| {
        ix.kind == IndexKind::ClusteredColumnstore
            || fields(ix).iter().all(|(n, _, _)| column(n).is_some_and(|c| keyable(&c.source_type) && keyable(&c.target_type)))
    };
    let same_shape = |a: &'a Index, b: &'a Index| {
        if a.kind == IndexKind::ClusteredColumnstore || b.kind == IndexKind::ClusteredColumnstore {
            return a.kind == b.kind;
        }
        let (fa, fb) = (fields(a), fields(b));
        fa.len() == fb.len()
            && !fa.is_empty()
            && fa.iter().zip(&fb).all(|(x, y)| x.1 == y.1 && x.2 == y.2 && column(x.0).is_some_and(|c| c.target == y.0))
    };
    let mut out = Vec::new();
    let mut used = vec![false; r.indexes.len()];
    for a in l.indexes.iter().filter(|i| eligible(i) && valid_left(i)) {
        let same_name = r.indexes.iter().position(|b| b.name == a.name && eligible(b) && same_shape(a, b));
        let found = same_name.filter(|&j| !used[j]).or_else(|| (0..r.indexes.len()).find(|&j| !used[j] && eligible(&r.indexes[j]) && same_shape(a, &r.indexes[j])));
        if let Some(j) = found {
            used[j] = true;
            out.push((a, &r.indexes[j]));
        }
    }
    out
}

fn use_index(p: &mut TablePair, a: &Index, b: &Index) {
    p.key = if a.kind == IndexKind::ClusteredColumnstore {
        (0..p.columns.len()).filter(|&i| keyable(&p.columns[i].source_type) && keyable(&p.columns[i].target_type)).collect()
    } else {
        a.columns.iter().filter_map(|c| p.columns.iter().position(|x| x.source.eq_ignore_ascii_case(&c.name))).collect()
    };
    p.key_name = Some(a.name.clone());
    p.target_key_name = Some(b.name.clone());
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
        let db1 = Database { tables: vec![table("Order Line", vec![col("Id", "int"), col("Qty", "int"), col("Note", "nvarchar"), col("Flag", "bit")], &["Id"])], ..Default::default() };
        let db2 = Database { tables: vec![table("orderline", vec![col("ID", "bigint"), col("Qty", "int"), col("Note", "datetime"), col("Flag", "int")], &["ID"])], ..Default::default() };
        let (pairs, only1, only2) = map_tables(&db1, &db2, &DataOptions::default_set(), &DataMappings::default());
        assert!(only1.is_empty() && only2.is_empty());
        let p = &pairs[0];
        assert_eq!(p.columns.iter().map(|c| c.source.as_str()).collect::<Vec<_>>(), ["Id", "Qty"], "Note's and Flag's types don't match");
        assert_eq!(p.key, [0]);
        assert_eq!(p.key_name.as_deref(), Some("PK_Order Line"));
        assert!(p.problem.is_none());
        let types: Vec<&str> = p.notes.iter().filter(|n| n.code == "type").map(|n| n.text.as_str()).collect();
        assert_eq!(types[0], "The fields [dbo].[Order Line].[Note] and [dbo].[orderline].[Note] have incomparable types nvarchar and datetime.");
        assert_eq!(types.len(), 2);
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
            ..Default::default()
        };
        let (pairs, _, _) = map_tables(&db, &db, &DataOptions::default_set(), &user);
        assert!(pairs[0].problem.is_none());
        assert_eq!(pairs[0].key, [1]);
    }

    #[test]
    fn owner_and_column_mappings() {
        let mut a = table("Orders", vec![col("Id", "int"), col("Amount", "money")], &["Id"]);
        a.schema = "dev".into();
        let b = table("Orders", vec![col("Id", "int"), col("Total", "money")], &["Id"]);
        let db1 = Database { tables: vec![a], ..Default::default() };
        let db2 = Database { tables: vec![b], ..Default::default() };
        let (pairs, only1, _) = map_tables(&db1, &db2, &DataOptions::default_set(), &DataMappings::default());
        assert!(pairs.is_empty() && only1.len() == 1, "different schemas do not pair by themselves");
        let user = DataMappings {
            schemas: vec![("dev".into(), "dbo".into())],
            tables: vec![TableOverride { source: ObjectName::new("dev", "Orders"), columns: vec![("Amount".into(), "Total".into())], ..Default::default() }],
        };
        let (pairs, _, _) = map_tables(&db1, &db2, &DataOptions::default_set(), &user);
        assert_eq!(pairs[0].target, ObjectName::new("dbo", "Orders"));
        assert_eq!(pairs[0].columns.iter().map(|c| (c.source.as_str(), c.target.as_str())).collect::<Vec<_>>(), [("Id", "Id"), ("Amount", "Total")]);
        assert!(pairs[0].notes.is_empty(), "{:?}", pairs[0].notes);
    }

    #[test]
    fn user_mapping_warnings_and_exclusions() {
        let db = Database { tables: vec![table("Orders", vec![col("Id", "int"), col("Amount", "money")], &["Id"])], ..Default::default() };
        let o = DataOptions::default_set();
        let over = |name: &str| TableOverride { source: ObjectName::new("dbo", name), ..Default::default() };
        // Names match ignoring case; a mapping to a table that is gone leaves the name pairing.
        let user = DataMappings {
            tables: vec![TableOverride { where1: Some("Id > 1".into()), target: Some(ObjectName::new("dbo", "Gone")), ..over("ORDERS") }],
            ..Default::default()
        };
        let (pairs, _, _) = map_tables(&db, &db, &o, &user);
        assert_eq!(pairs[0].where1.as_deref(), Some("Id > 1"));
        assert_eq!(pairs[0].target, ObjectName::new("dbo", "Orders"));
        assert_eq!(user_mapping_warnings(&db, &db, &o, &user), ["Unable to map tables. The tables don't exist or have not been unmapped."]);
        let cs = o.clone().with(DataOpt::CaseSensitiveObjectDefinition);
        assert!(map_tables(&db, &db, &cs, &user).0[0].where1.is_none(), "case-sensitive names must match exactly");
        // Settings of a table that does not exist.
        let user = DataMappings { tables: vec![TableOverride { where2: Some("x = 1".into()), key: vec!["index:IX".into()], ..over("Nope") }], ..Default::default() };
        assert_eq!(
            user_mapping_warnings(&db, &db, &o, &user),
            ["Unable to set right where clause for comparison of table mapping. Table mapping does not exist.", "Unable to use index for table mapping. Table mapping does not exist."]
        );
        // Mappings that cannot apply exclude the pair.
        let user = DataMappings { tables: vec![TableOverride { columns: vec![("Amount".into(), "Total".into())], ..over("Orders") }], ..Default::default() };
        assert_eq!(map_tables(&db, &db, &o, &user).0[0].excluded.as_deref(), Some("Unable to map columns of table mapping. Columns do not exist."));
        let user = DataMappings { tables: vec![TableOverride { key: vec!["index:IX_None".into()], ..over("Orders") }], ..Default::default() };
        assert_eq!(map_tables(&db, &db, &o, &user).0[0].excluded.as_deref(), Some("Unable to use index for table mapping. Index mapping does not exist."));
        let user = DataMappings { tables: vec![TableOverride { key: vec!["Nope".into()], ..over("Orders") }], ..Default::default() };
        assert_eq!(map_tables(&db, &db, &o, &user).0[0].excluded.as_deref(), Some("Unable to use column for custom comparison of table mapping. Column does not exist."));
        let user = DataMappings { tables: vec![TableOverride { excluded_columns: vec!["Nope".into()], ..over("Orders") }], ..Default::default() };
        assert!(map_tables(&db, &db, &o, &user).0[0].excluded.is_none(), "a missing deselected column is ignored");
    }

    #[test]
    fn ambiguous_names_stay_unpaired() {
        let o = DataOptions::default_set().with(DataOpt::IgnoreUnderscores);
        let db1 = Database { tables: vec![table("Order_Item", vec![col("Id", "int")], &["Id"]), table("OrderItem", vec![col("Id", "int")], &["Id"])], ..Default::default() };
        let db2 = Database { tables: vec![table("orderitem", vec![col("Id", "int")], &["Id"])], ..Default::default() };
        let (pairs, only1, only2) = map_tables(&db1, &db2, &o, &DataMappings::default());
        assert!(pairs.is_empty());
        assert_eq!((only1.len(), only2.len()), (2, 1));
    }

    #[test]
    fn type_matrix() {
        let t = |n: &str| DataType { name: n.into(), ..Default::default() };
        let sized = |n: &str, l: i32| DataType { name: n.into(), length: Some(l), ..Default::default() };
        assert!(type_check(&t("int"), &sized("nvarchar", 20), None, None).is_some());
        assert!(type_check(&sized("nvarchar", 20), &t("int"), None, None).unwrap().has("TypeConversion"));
        assert!(type_check(&sized("varchar", 20), &t("int"), None, None).is_none(), "the matrix is not symmetric");
        assert!(type_check(&t("date"), &t("datetime"), None, None).is_some());
        assert!(type_check(&sized("char", 36), &t("uniqueidentifier"), None, None).unwrap().has("GuidComparisonKey"));
        assert!(type_check(&t("xml"), &sized("nvarchar", -1), None, None).unwrap().has("XmlConversionLeft"));
        assert!(type_check(&t("sql_variant"), &t("sql_variant"), None, None).is_some());
        assert!(type_check(&t("sql_variant"), &t("int"), None, None).is_none());
        assert!(type_check(&sized("nvarchar", 50), &sized("nvarchar", 10), None, None).unwrap().has("TruncateRight"));
        assert!(type_check(&t("int"), &sized("varchar", 5), None, None).unwrap().has("TruncateRight"));
        let c = type_check(&sized("varchar", 5), &sized("varchar", 5), Some("Latin1_General_CI_AS"), Some("Latin1_General_BIN"));
        assert!(c.unwrap().has("Collation"));
        assert_eq!(graph_kind("$node_id_6981CB632A5040CA91FDDACD1FE59377"), Some("$node_id"));
        assert_eq!(graph_kind("$node_id"), None);
    }
}
