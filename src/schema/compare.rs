//! Compares two schema models: pairs objects by name (through schema and table mappings),
//! then compares each pair property by property under the options.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::options::{Opt, Options};
use super::text::{self, TextKind};
use crate::catalog::*;

/// Top-level object types, in comparison order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    Schema,
    Role,
    User,
    UserDefinedType,
    Table,
    View,
    Function,
    StoredProcedure,
    Sequence,
    Synonym,
    Assembly,
    XmlSchemaCollection,
    Default,
    Rule,
    DdlTrigger,
    PartitionFunction,
    PartitionScheme,
    FullTextCatalog,
    FullTextStoplist,
    SearchPropertyList,
    MessageType,
    Contract,
    Queue,
    Service,
    Route,
    ServiceBinding,
    EventNotification,
    Certificate,
    AsymmetricKey,
    SymmetricKey,
    SecurityPolicy,
    ExternalDataSource,
    ExternalFileFormat,
    ExternalTable,
    ExtendedProperty,
}

impl ObjectType {
    pub const ALL: [ObjectType; 35] = [
        ObjectType::Schema,
        ObjectType::Role,
        ObjectType::User,
        ObjectType::UserDefinedType,
        ObjectType::Table,
        ObjectType::View,
        ObjectType::Function,
        ObjectType::StoredProcedure,
        ObjectType::Sequence,
        ObjectType::Synonym,
        ObjectType::Assembly,
        ObjectType::XmlSchemaCollection,
        ObjectType::Default,
        ObjectType::Rule,
        ObjectType::DdlTrigger,
        ObjectType::PartitionFunction,
        ObjectType::PartitionScheme,
        ObjectType::FullTextCatalog,
        ObjectType::FullTextStoplist,
        ObjectType::SearchPropertyList,
        ObjectType::MessageType,
        ObjectType::Contract,
        ObjectType::Queue,
        ObjectType::Service,
        ObjectType::Route,
        ObjectType::ServiceBinding,
        ObjectType::EventNotification,
        ObjectType::Certificate,
        ObjectType::AsymmetricKey,
        ObjectType::SymmetricKey,
        ObjectType::SecurityPolicy,
        ObjectType::ExternalDataSource,
        ObjectType::ExternalFileFormat,
        ObjectType::ExternalTable,
        ObjectType::ExtendedProperty,
    ];

    /// The name SQL Compare uses in reports and in `/include:<type>`.
    pub fn label(self) -> &'static str {
        match self {
            ObjectType::Schema => "Schema",
            ObjectType::Role => "Role",
            ObjectType::User => "User",
            ObjectType::UserDefinedType => "User-defined type",
            ObjectType::Table => "Table",
            ObjectType::View => "View",
            ObjectType::Function => "Function",
            ObjectType::StoredProcedure => "Stored procedure",
            ObjectType::Sequence => "Sequence",
            ObjectType::Synonym => "Synonym",
            ObjectType::Assembly => "Assembly",
            ObjectType::XmlSchemaCollection => "XML schema collection",
            ObjectType::Default => "Default",
            ObjectType::Rule => "Rule",
            ObjectType::DdlTrigger => "DDL trigger",
            ObjectType::PartitionFunction => "Partition function",
            ObjectType::PartitionScheme => "Partition scheme",
            ObjectType::FullTextCatalog => "Full text catalog",
            ObjectType::FullTextStoplist => "Full text stoplist",
            ObjectType::SearchPropertyList => "Search property list",
            ObjectType::MessageType => "Message type",
            ObjectType::Contract => "Contract",
            ObjectType::Queue => "Queue",
            ObjectType::Service => "Service",
            ObjectType::Route => "Route",
            ObjectType::ServiceBinding => "Service binding",
            ObjectType::EventNotification => "Event notification",
            ObjectType::Certificate => "Certificate",
            ObjectType::AsymmetricKey => "Asymmetric key",
            ObjectType::SymmetricKey => "Symmetric key",
            ObjectType::SecurityPolicy => "Security policy",
            ObjectType::ExternalDataSource => "External data source",
            ObjectType::ExternalFileFormat => "External file format",
            ObjectType::ExternalTable => "External table",
            ObjectType::ExtendedProperty => "Extended property",
        }
    }

    /// Objects that belong to a schema; the others have a database-wide name.
    pub fn schema_scoped(self) -> bool {
        matches!(
            self,
            ObjectType::UserDefinedType
                | ObjectType::Table
                | ObjectType::View
                | ObjectType::Function
                | ObjectType::StoredProcedure
                | ObjectType::Sequence
                | ObjectType::Synonym
                | ObjectType::XmlSchemaCollection
                | ObjectType::Default
                | ObjectType::Rule
                | ObjectType::Queue
                | ObjectType::SecurityPolicy
                | ObjectType::ExternalTable
        )
    }

    /// Accepts the label with or without spaces and dashes: `StoredProcedure`, `stored procedure`.
    pub fn parse(s: &str) -> Option<ObjectType> {
        let key = |x: &str| x.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let k = key(s);
        ObjectType::ALL.into_iter().find(|t| key(t.label()) == k || (k == "udt" && *t == ObjectType::UserDefinedType))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiffKind {
    Equal,
    Different,
    /// Only in database 1 (the source).
    OnlyIn1,
    /// Only in database 2 (the target).
    OnlyIn2,
}

impl DiffKind {
    /// The markers SQL Compare prints: `==`, `<>`, `>>`, `<<`.
    pub fn marker(self) -> &'static str {
        match self {
            DiffKind::Equal => "==",
            DiffKind::Different => "<>",
            DiffKind::OnlyIn1 => ">>",
            DiffKind::OnlyIn2 => "<<",
        }
    }
}

/// One level of where a property difference is: `Column(Name, Name)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// A property that differs; one-sided sub-objects have `None` on the missing side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropDiff {
    /// Outermost first.
    pub location: Vec<Frame>,
    pub name: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    pub object_type: ObjectType,
    /// Name in database 1, or in database 2 for objects only there.
    pub name: ObjectName,
    /// Name in database 2 when it differs from `name` (mapped or renamed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_name: Option<ObjectName>,
    pub kind: DiffKind,
    pub properties: Vec<PropDiff>,
    /// Chosen for deployment; every non-equal difference starts selected.
    pub selected: bool,
    /// The only differences are extended properties.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trivial: bool,
}

impl Difference {
    /// The name the object has in the dependency graph: its own name for schema-scoped objects,
    /// a key naming the kind for database-wide ones that take part in dependencies.
    pub fn key(&self) -> ObjectName {
        object_key(self.object_type, &self.name)
    }

    pub fn target_key(&self) -> ObjectName {
        object_key(self.object_type, self.target_name.as_ref().unwrap_or(&self.name))
    }
}

pub fn object_key(ty: ObjectType, name: &ObjectName) -> ObjectName {
    let kind = match ty {
        ObjectType::Assembly => KEY_ASSEMBLY,
        ObjectType::MessageType => KEY_MESSAGE_TYPE,
        ObjectType::Contract => KEY_CONTRACT,
        ObjectType::Service => KEY_SERVICE,
        ObjectType::EventNotification => KEY_EVENT_NOTIFICATION,
        ObjectType::ExternalDataSource => KEY_EXTERNAL_DATA_SOURCE,
        _ => return name.clone(),
    };
    ObjectName::new(kind, &name.name)
}

/// User-defined mappings between the two databases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Mappings {
    /// Schema in database 1 → schema in database 2.
    pub schemas: Vec<(String, String)>,
    pub tables: Vec<TableMapping>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TableMapping {
    pub source: ObjectName,
    pub target: ObjectName,
    /// Column in source → column in target, for columns whose names differ.
    pub columns: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub differences: Vec<Difference>,
    /// Column pairs found by "Auto-map similar columns": (table, source column, target column).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub auto_mapped_columns: Vec<(ObjectName, String, String)>,
}

impl Comparison {
    pub fn identical(&self) -> bool {
        self.differences.iter().all(|d| d.kind == DiffKind::Equal)
    }
}

pub fn compare(db1: &Database, db2: &Database, opts: &Options, mappings: &Mappings) -> Comparison {
    let mut c = Comparer::new(db1, db2, opts, mappings);
    c.schemas();
    c.principals();
    c.objects(ObjectType::UserDefinedType, &db1.types, &db2.types, |t| (&t.schema, &t.name), Comparer::udt);
    c.tables();
    c.objects(ObjectType::View, &db1.views, &db2.views, |v| (&v.schema, &v.name), Comparer::view);
    let (f1, p1): (Vec<&Routine>, Vec<&Routine>) = db1.routines.iter().partition(|r| is_function(r));
    let (f2, p2): (Vec<&Routine>, Vec<&Routine>) = db2.routines.iter().partition(|r| is_function(r));
    c.objects(ObjectType::Function, &f1, &f2, |r| (&r.schema, &r.name), |c, a, b| Comparer::routine(c, a, b));
    c.objects(ObjectType::StoredProcedure, &p1, &p2, |r| (&r.schema, &r.name), |c, a, b| Comparer::routine(c, a, b));
    c.objects(ObjectType::Sequence, &db1.sequences, &db2.sequences, |s| (&s.schema, &s.name), Comparer::sequence);
    c.objects(ObjectType::Synonym, &db1.synonyms, &db2.synonyms, |s| (&s.schema, &s.name), Comparer::synonym);
    c.more_objects();
    if !opts.has(Opt::IgnoreExtendedProperties) {
        c.database_properties();
    }
    let mut result = c.finish();
    if opts.has(Opt::IgnoreTSQLT) {
        result.differences.retain(|d| !is_tsqlt(&d.name));
    }
    if opts.has(Opt::IgnoreDataSyncSchema) {
        result.differences.retain(|d| !d.name.schema.eq_ignore_ascii_case("DataSync"));
    }
    result
}

fn is_function(r: &Routine) -> bool {
    !matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure)
}

fn is_tsqlt(name: &ObjectName) -> bool {
    ["tSQLt", "SQLCop"].iter().any(|s| name.schema.eq_ignore_ascii_case(s) || name.name.eq_ignore_ascii_case(s))
}

/// Collects property differences under a stack of context frames.
#[derive(Default)]
pub(super) struct Diffs {
    pub(super) out: Vec<PropDiff>,
    ctx: Vec<Frame>,
}

impl Diffs {
    pub(super) fn enter(&mut self, name: &str, source: Option<&str>, target: Option<&str>) {
        self.ctx.push(Frame { name: name.into(), source: source.map(Into::into), target: target.map(Into::into) });
    }

    pub(super) fn leave(&mut self) {
        self.ctx.pop();
    }

    pub(super) fn add(&mut self, name: &str, source: Option<String>, target: Option<String>) {
        self.out.push(PropDiff { location: self.ctx.clone(), name: name.into(), source, target });
    }

    /// Records `name` when the values differ.
    pub(super) fn prop<T: PartialEq + std::fmt::Display>(&mut self, name: &str, a: T, b: T) {
        if a != b {
            self.add(name, Some(a.to_string()), Some(b.to_string()));
        }
    }

    pub(super) fn opt_prop(&mut self, name: &str, a: Option<&str>, b: Option<&str>) {
        if a != b {
            self.add(name, a.map(Into::into), b.map(Into::into));
        }
    }

    pub(super) fn only1(&mut self, what: &str, name: &str) {
        self.add(what, Some(name.into()), None);
    }

    pub(super) fn only2(&mut self, what: &str, name: &str) {
        self.add(what, None, Some(name.into()));
    }
}

pub(super) struct Comparer<'a> {
    pub(super) db1: &'a Database,
    pub(super) db2: &'a Database,
    pub(super) opts: &'a Options,
    mappings: &'a Mappings,
    case_sensitive: bool,
    result: Comparison,
}

impl<'a> Comparer<'a> {
    fn new(db1: &'a Database, db2: &'a Database, opts: &'a Options, mappings: &'a Mappings) -> Self {
        Comparer {
            db1,
            db2,
            opts,
            mappings,
            case_sensitive: opts.has(Opt::CaseSensitiveObjectDefinition),
            result: Comparison::default(),
        }
    }

    fn finish(mut self) -> Comparison {
        self.result.differences.sort_by(|a, b| {
            (a.object_type, &a.name.schema.to_lowercase(), &a.name.name.to_lowercase())
                .cmp(&(b.object_type, &b.name.schema.to_lowercase(), &b.name.name.to_lowercase()))
        });
        self.result
    }

    pub(super) fn key(&self, s: &str) -> String {
        if self.case_sensitive { s.to_string() } else { s.to_lowercase() }
    }

    pub(super) fn same_name(&self, a: &str, b: &str) -> bool {
        self.key(a) == self.key(b)
    }

    /// The schema in database 2 that a database-1 schema maps to.
    pub(super) fn target_schema(&self, schema: &str) -> String {
        self.mappings
            .schemas
            .iter()
            .find(|(s, _)| self.same_name(s, schema))
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| schema.to_string())
    }

    pub(super) fn push(&mut self, object_type: ObjectType, n1: Option<ObjectName>, n2: Option<ObjectName>, props: Vec<PropDiff>) {
        let kind = match (&n1, &n2) {
            (Some(_), None) => DiffKind::OnlyIn1,
            (None, Some(_)) => DiffKind::OnlyIn2,
            _ if props.is_empty() => DiffKind::Equal,
            _ => DiffKind::Different,
        };
        let trivial = kind == DiffKind::Different && props.iter().all(is_xprop);
        let name = n1.clone().or(n2.clone()).unwrap_or_default();
        let target_name = match (&n1, n2) {
            (Some(a), Some(b)) if a.schema != b.schema || a.name != b.name => Some(b),
            _ => None,
        };
        self.result.differences.push(Difference {
            object_type,
            name,
            target_name,
            kind,
            properties: props,
            selected: kind != DiffKind::Equal,
            trivial,
        });
    }

    /// Pairs objects of one type by (mapped) schema and name and compares each pair.
    pub(super) fn objects<T>(
        &mut self,
        ty: ObjectType,
        left: &[T],
        right: &[T],
        name: impl Fn(&T) -> (&String, &String),
        cmp: impl Fn(&Self, &T, &T) -> Vec<PropDiff>,
    ) {
        let mut index: HashMap<(String, String), usize> = HashMap::new();
        for (i, r) in right.iter().enumerate() {
            let (s, n) = name(r);
            index.insert((self.key(s), self.key(n)), i);
        }
        let mut matched = vec![false; right.len()];
        for l in left {
            let (s, n) = name(l);
            let target = index.get(&(self.key(&self.target_schema(s)), self.key(n))).copied();
            let n1 = Some(ObjectName::new(s, n));
            match target {
                Some(i) => {
                    matched[i] = true;
                    let props = cmp(self, l, &right[i]);
                    let (s2, n2) = name(&right[i]);
                    self.push(ty, n1, Some(ObjectName::new(s2, n2)), props);
                }
                None => self.push(ty, n1, None, Vec::new()),
            }
        }
        for (i, r) in right.iter().enumerate() {
            if !matched[i] {
                let (s, n) = name(r);
                self.push(ty, None, Some(ObjectName::new(s, n)), Vec::new());
            }
        }
    }

    fn schemas(&mut self) {
        let skip = |s: &Schema| s.name == "dbo";
        let left: Vec<&Schema> = self.db1.schemas.iter().filter(|s| !skip(s)).collect();
        let right: Vec<&Schema> = self.db2.schemas.iter().filter(|s| !skip(s)).collect();
        let empty = String::new();
        self.objects(
            ObjectType::Schema,
            &left,
            &right,
            |s| (&empty, &s.name),
            |c, a, b| {
                let mut d = Diffs::default();
                if !c.opts.has(Opt::IgnoreOwners) && !c.opts.has(Opt::IgnoreSchemaObjectAuthorization) {
                    d.prop("Owner", &a.owner, &b.owner);
                }
                c.xprops(&mut d, "SCHEMA", None, &a.name, &b.name);
                c.permissions(&mut d, "SCHEMA", &a.name, None, &b.name, None);
                d.out
            },
        );
        // Schemas have no schema of their own.
        for diff in &mut self.result.differences {
            if diff.object_type == ObjectType::Schema {
                diff.name.schema.clear();
            }
        }
    }

    fn tables(&mut self) {
        let (db1, db2) = (self.db1, self.db2);
        let mut matched = vec![false; self.db2.tables.len()];
        let mut index: HashMap<(String, String), usize> = HashMap::new();
        for (i, t) in self.db2.tables.iter().enumerate() {
            index.insert((self.key(&t.schema), self.key(&t.name)), i);
        }
        for t1 in db1.tables.iter().filter(|t| !t.history_table) {
            let mapping = self
                .mappings
                .tables
                .iter()
                .find(|m| self.same_name(&m.source.schema, &t1.schema) && self.same_name(&m.source.name, &t1.name));
            let target = match mapping {
                Some(m) => index.get(&(self.key(&m.target.schema), self.key(&m.target.name))).copied(),
                None => index.get(&(self.key(&self.target_schema(&t1.schema)), self.key(&t1.name))).copied(),
            };
            match target {
                Some(i) if !db2.tables[i].history_table => {
                    matched[i] = true;
                    let t2 = &db2.tables[i];
                    let mut d = Diffs::default();
                    self.table(&mut d, t1, t2, mapping);
                    self.push(ObjectType::Table, Some(t1.object_name()), Some(t2.object_name()), d.out);
                }
                _ => self.push(ObjectType::Table, Some(t1.object_name()), None, Vec::new()),
            }
        }
        for (i, t2) in db2.tables.iter().enumerate() {
            if !matched[i] && !t2.history_table {
                self.push(ObjectType::Table, None, Some(t2.object_name()), Vec::new());
            }
        }
    }

    fn table(&mut self, d: &mut Diffs, a: &Table, b: &Table, mapping: Option<&TableMapping>) {
        let o = self.opts;
        if !o.has(Opt::IgnoreChangeTracking) {
            d.prop("Change tracking", a.change_tracking.is_some(), b.change_tracking.is_some());
            if let (Some(x), Some(y)) = (&a.change_tracking, &b.change_tracking) {
                d.prop("Track columns updated", x.track_columns_updated, y.track_columns_updated);
            }
        }
        d.prop("Lock escalation", &a.lock_escalation, &b.lock_escalation);
        d.prop("System-versioned", a.temporal.is_some(), b.temporal.is_some());
        if let (Some(x), Some(y)) = (&a.temporal, &b.temporal) {
            d.prop("History table", x.history_table.to_string(), y.history_table.to_string());
            d.opt_prop("History retention period", x.retention.as_deref(), y.retention.as_deref());
            if let (Some(h1), Some(h2)) =
                (self.db1.table(&x.history_table.schema, &x.history_table.name), self.db2.table(&y.history_table.schema, &y.history_table.name))
            {
                d.enter("History table", Some(&h1.name), Some(&h2.name));
                self.columns(d, h1, h2, None);
                d.leave();
            }
        }
        d.prop("Memory optimized", a.memory_optimized, b.memory_optimized);
        d.opt_prop("Durability", a.durability.as_deref(), b.durability.as_deref());
        d.opt_prop("Graph table", a.graph.as_deref(), b.graph.as_deref());
        self.columns(d, a, b, mapping);
        self.indexes(d, &a.indexes, &b.indexes, a.memory_optimized);
        if !o.has(Opt::IgnoreKeys) {
            self.foreign_keys(d, a, b, mapping);
        }
        if !o.has(Opt::IgnoreChecks) {
            self.checks(d, &a.check_constraints, &b.check_constraints);
        }
        if !o.has(Opt::IgnoreTriggers) {
            self.triggers(d, &a.triggers, &b.triggers);
        }
        self.table_extras(d, a, b);
        self.xprops(d, "TABLE", Some(&a.schema), &a.name, &b.name);
        self.permissions(d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        let partitioned = a.storage.partition_column.is_some() || b.storage.partition_column.is_some();
        if !o.has(Opt::IgnoreDataCompression) && !(partitioned && o.has(Opt::IgnoreFileGroups)) {
            d.prop("Data compression", compression(&a.storage), compression(&b.storage));
        }
        if !o.has(Opt::IgnoreFileGroups) {
            self.storage(d, &a.storage, &b.storage);
            if let (Some(x), Some(y)) = (&a.text_filegroup, &b.text_filegroup) {
                d.prop("TEXTIMAGE_ON", x, y);
            }
            if let (Some(x), Some(y)) = (&a.filestream_filegroup, &b.filestream_filegroup) {
                d.prop("FILESTREAM_ON", x, y);
            }
        }
    }

    fn storage(&self, d: &mut Diffs, a: &Storage, b: &Storage) {
        d.opt_prop("Filegroup or partition scheme", a.data_space.as_deref(), b.data_space.as_deref());
        d.opt_prop("Partition column", a.partition_column.as_deref(), b.partition_column.as_deref());
    }

    /// Pairs columns: by position with ForceColumnOrder, else through the table mapping, else by
    /// name, then pairs what is left with "Auto-map similar columns".
    fn columns(&mut self, d: &mut Diffs, a: &Table, b: &Table, mapping: Option<&TableMapping>) {
        let left: Vec<&Column> = a.columns.iter().filter(|c| !is_graph_column(c)).collect();
        let right: Vec<&Column> = b.columns.iter().filter(|c| !is_graph_column(c)).collect();
        let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        if self.opts.has(Opt::ForceColumnOrder) {
            for i in 0..left.len().max(right.len()) {
                match (left.get(i), right.get(i)) {
                    (Some(x), Some(y)) if self.same_name(&x.name, &y.name) => pairs.push((Some(i), Some(i))),
                    (x, y) => {
                        if x.is_some() {
                            pairs.push((Some(i), None));
                        }
                        if y.is_some() {
                            pairs.push((None, Some(i)));
                        }
                    }
                }
            }
        } else {
            let mut used = vec![false; right.len()];
            let mut unpaired = Vec::new();
            for (i, x) in left.iter().enumerate() {
                let wanted = mapping
                    .and_then(|m| m.columns.iter().find(|(s, _)| self.same_name(s, &x.name)))
                    .map(|(_, t)| t.as_str())
                    .unwrap_or(&x.name);
                match right.iter().enumerate().position(|(j, y)| !used[j] && self.same_name(&y.name, wanted)) {
                    Some(j) => {
                        used[j] = true;
                        pairs.push((Some(i), Some(j)));
                    }
                    None => unpaired.push(i),
                }
            }
            // Auto-map: a leftover source column pairs with an otherwise identical leftover target
            // column, preferring the closest name and then the same position.
            if !self.opts.has(Opt::NoAutoColumnMapping) {
                let probe = Options::default_set().with(Opt::IgnoreConstraintNames);
                unpaired.retain(|&i| {
                    let x = left[i];
                    let candidates: Vec<usize> = (0..right.len())
                        .filter(|&j| !used[j])
                        .filter(|&j| {
                            let mut tmp = Diffs::default();
                            column_props(&mut tmp, x, right[j], &probe, false, true);
                            tmp.out.is_empty()
                        })
                        .collect();
                    if candidates.is_empty() {
                        return true;
                    }
                    let max_len = candidates.iter().map(|&j| right[j].name.len()).chain([x.name.len()]).max().unwrap_or(1);
                    let handicap = if max_len == 1 { 1 } else { max_len / 2 } as i64;
                    let best = candidates
                        .iter()
                        .copied()
                        .min_by_key(|&j| {
                            let dist = levenshtein(&self.key(&x.name), &self.key(&right[j].name)) as i64;
                            if right[j].ordinal == x.ordinal { dist - handicap } else { dist }
                        })
                        .unwrap();
                    used[best] = true;
                    pairs.push((Some(i), Some(best)));
                    self.result.auto_mapped_columns.push((a.object_name(), x.name.clone(), right[best].name.clone()));
                    false
                });
            }
            pairs.extend(unpaired.into_iter().map(|i| (Some(i), None)));
            pairs.extend((0..right.len()).filter(|&j| !used[j]).map(|j| (None, Some(j))));
        }
        for (i, j) in pairs {
            match (i.map(|i| left[i]), j.map(|j| right[j])) {
                (Some(x), Some(y)) => {
                    d.enter("Column", Some(&x.name), Some(&y.name));
                    if x.name != y.name {
                        d.prop("Name", &x.name, &y.name);
                    }
                    column_props(d, x, y, self.opts, a.memory_optimized, false);
                    self.sensitivity(d, x, y);
                    let minor = |c: &Column| Some(c.name.clone());
                    self.xprops_minor(d, "TABLE", &a.schema, &a.name, &b.name, "COLUMN", minor(x), minor(y));
                    d.leave();
                }
                (Some(x), None) => d.only1("Column", &x.name),
                (None, Some(y)) => d.only2("Column", &y.name),
                _ => {}
            }
        }
    }

    fn index_ignored(&self, ix: &Index) -> bool {
        let o = self.opts;
        o.has(Opt::IgnoreIndexes)
            || (o.has(Opt::IgnorePerformanceIndexes) && !(ix.primary_key || ix.unique || ix.unique_constraint))
    }

    fn indexes(&self, d: &mut Diffs, a: &[Index], b: &[Index], memory_optimized: bool) {
        let left: Vec<&Index> = a.iter().filter(|i| !self.index_ignored(i)).collect();
        let right: Vec<&Index> = b.iter().filter(|i| !self.index_ignored(i)).collect();
        let compare = |x: &Index, y: &Index, d: &mut Diffs| self.index_props(d, x, y, memory_optimized);
        self.pair_named(d, "Index", &left, &right, |i| (&i.name, i.system_named), compare);
    }

    /// Pairs constraints and indexes by name, or structurally when names are ignored (§4.3 of the spec).
    pub(super) fn pair_named<T>(
        &self,
        d: &mut Diffs,
        what: &str,
        left: &[&T],
        right: &[&T],
        name: impl Fn(&T) -> (&String, bool),
        compare: impl Fn(&T, &T, &mut Diffs),
    ) {
        let o = self.opts;
        let equal = |x: &T, y: &T| {
            let mut tmp = Diffs::default();
            compare(x, y, &mut tmp);
            tmp.out.is_empty()
        };
        let mut used = vec![false; right.len()];
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        let mut alone: Vec<usize> = Vec::new();
        for (i, x) in left.iter().enumerate() {
            let (n, system) = name(x);
            let structural = o.has(Opt::IgnoreConstraintNames) || (system && o.has(Opt::IgnoreSystemNamedConstraintNames));
            let found = if structural {
                (0..right.len()).find(|&j| !used[j] && equal(x, right[j]))
            } else {
                (0..right.len()).find(|&j| !used[j] && self.same_name(name(right[j]).0, n)).or_else(|| {
                    if o.has(Opt::IgnoreSystemNamedConstraintNames) {
                        (0..right.len()).find(|&j| !used[j] && name(right[j]).1 && equal(x, right[j]))
                    } else {
                        None
                    }
                })
            };
            match found {
                Some(j) => {
                    used[j] = true;
                    pairs.push((i, j));
                }
                None => alone.push(i),
            }
        }
        for (i, j) in pairs {
            let (x, y) = (left[i], right[j]);
            d.enter(what, Some(name(x).0), Some(name(y).0));
            compare(x, y, d);
            d.leave();
        }
        for i in alone {
            d.only1(what, name(left[i]).0);
        }
        for (j, y) in right.iter().enumerate() {
            if !used[j] {
                d.only2(what, name(y).0);
            }
        }
    }

    fn index_props(&self, d: &mut Diffs, a: &Index, b: &Index, memory_optimized: bool) {
        let o = self.opts;
        d.prop("Primary key", a.primary_key, b.primary_key);
        d.prop("Unique", a.unique, b.unique);
        d.prop("Unique constraint", a.unique_constraint, b.unique_constraint);
        d.prop("Index type", format!("{:?}", a.kind), format!("{:?}", b.kind));
        d.prop("IGNORE_DUP_KEY", a.ignore_dup_key, b.ignore_dup_key);
        if !o.has(Opt::IgnoreFileGroups) {
            self.storage(d, &a.storage, &b.storage);
        }
        // A rebuilt memory-optimized table's statistics are updated WITH NORECOMPUTE.
        if !o.has(Opt::IgnoreStatisticsNorecompute) && !memory_optimized {
            d.prop("STATISTICS_NORECOMPUTE", a.no_recompute, b.no_recompute);
        }
        if !o.has(Opt::IgnoreStatisticsIncremental) {
            d.prop("STATISTICS_INCREMENTAL", a.incremental, b.incremental);
        }
        let columnstore = matches!(a.kind, IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore);
        if !o.has(Opt::IgnoreIndexLockProperties) && !columnstore {
            d.prop("ALLOW_PAGE_LOCKS", a.allow_page_locks, b.allow_page_locks);
            d.prop("ALLOW_ROW_LOCKS", a.allow_row_locks, b.allow_row_locks);
        }
        if !o.has(Opt::IgnoreFillFactor) {
            d.prop("FILLFACTOR", a.fill_factor, b.fill_factor);
            d.prop("PAD_INDEX", a.pad_index, b.pad_index);
        }
        d.prop("OPTIMIZE_FOR_SEQUENTIAL_KEY", a.optimize_for_sequential_key, b.optimize_for_sequential_key);
        d.prop("Index is disabled", a.disabled, b.disabled);
        d.prop("BUCKET_COUNT", a.bucket_count.unwrap_or(0), b.bucket_count.unwrap_or(0));
        if columnstore {
            let set = |i: &Index| {
                let mut v: Vec<String> = i.columns.iter().map(|c| self.key(&c.name)).collect();
                v.sort();
                v.join(", ")
            };
            d.prop("Columns", set(a), set(b));
        } else {
            let keys = |i: &Index| {
                i.columns
                    .iter()
                    .map(|c| format!("{}{}", c.name, if c.descending { " DESC" } else { "" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if self.key(&keys(a)) != self.key(&keys(b)) {
                d.add("Key columns", Some(keys(a)), Some(keys(b)));
            }
            let inc = |i: &Index| {
                let mut v: Vec<String> = i.included.iter().map(|c| self.key(c)).collect();
                v.sort();
                v
            };
            if inc(a) != inc(b) {
                d.add("Included columns", Some(a.included.join(", ")), Some(b.included.join(", ")));
            }
        }
        match (&a.filter, &b.filter) {
            (Some(x), Some(y)) => {
                if text::compare(x, y, TextKind::Expression, o).is_some() {
                    d.add("Filter", Some(x.clone()), Some(y.clone()));
                }
            }
            (x, y) => d.opt_prop("Filter", x.as_deref(), y.as_deref()),
        }
        if !o.has(Opt::IgnoreDataCompression) && !(o.has(Opt::IgnoreFileGroups) && a.storage.partition_column.is_some()) {
            d.prop("Data compression", compression(&a.storage), compression(&b.storage));
        }
        if let (Some(x), Some(y)) = (&a.xml, &b.xml) {
            self.xml_index_props(d, x, y);
        }
        if let (Some(x), Some(y)) = (&a.spatial, &b.spatial) {
            if !x.tessellation.is_empty() && !y.tessellation.is_empty() {
                d.prop("Tessellation scheme", &x.tessellation, &y.tessellation);
            }
            d.prop("Spatial index type", if x.geography { "Geography" } else { "Geometry" }, if y.geography { "Geography" } else { "Geometry" });
            d.prop("Cells per object", x.cells_per_object, y.cells_per_object);
            let num = |v: &[String], i: usize| v.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            for (i, what) in ["X minimum", "Y minimum", "X maximum", "Y maximum"].iter().enumerate() {
                if num(&x.bounding_box, i) != num(&y.bounding_box, i) {
                    d.add(what, x.bounding_box.get(i).cloned(), y.bounding_box.get(i).cloned());
                }
            }
            for (i, what) in ["Level 1", "Level 2", "Level 3", "Level 4"].iter().enumerate() {
                d.opt_prop(what, x.grids.get(i).map(String::as_str), y.grids.get(i).map(String::as_str));
            }
        }
    }

    fn xml_index_props(&self, d: &mut Diffs, a: &XmlIndex, b: &XmlIndex) {
        d.prop("XML index type", format!("{:?}", a.kind), format!("{:?}", b.kind));
        d.opt_prop("Secondary type", a.secondary_type.as_deref(), b.secondary_type.as_deref());
        let using = |x: &XmlIndex| x.using_index.as_ref().map(|n| self.key(n));
        if using(a) != using(b) {
            d.add("XML primary index", a.using_index.clone(), b.using_index.clone());
        }
        if a.kind == XmlIndexKind::SecondarySelective {
            let path = |x: &XmlIndex| x.using_path.as_ref().map(|n| self.key(n));
            if path(a) != path(b) {
                d.add("XML primary selective path", a.using_path.clone(), b.using_path.clone());
            }
        }
        if !matches!(a.kind, XmlIndexKind::Selective | XmlIndexKind::SecondarySelective) {
            return;
        }
        for p in &a.paths {
            match b.paths.iter().find(|q| q.id == p.id) {
                None => d.add("Selective XML index path", Some(p.name.clone()), None),
                Some(q) => {
                    let same_type = p.sql == q.sql
                        && (if p.sql {
                            p.data_type == q.data_type
                        } else {
                            p.node_hint == q.node_hint
                                && (p.node_hint
                                    || ((p.xquery_type.is_none() || q.xquery_type.is_none() || p.xquery_type == q.xquery_type)
                                        && (p.max_length.is_none() || q.max_length.is_none() || p.max_length == q.max_length)))
                        });
                    if p.path != q.path || !self.same_name(&p.name, &q.name) || p.singleton != q.singleton || !same_type {
                        d.add("Selective XML index path", Some(p.name.clone()), Some(q.name.clone()));
                    }
                }
            }
        }
        for q in b.paths.iter().filter(|q| !a.paths.iter().any(|p| p.id == q.id)) {
            d.add("Selective XML index path", None, Some(q.name.clone()));
        }
        let ns = |x: &XmlIndex| {
            let mut v: Vec<(String, String)> = x.namespaces.iter().map(|n| (n.prefix.clone().unwrap_or_default(), n.uri.clone())).collect();
            v.sort();
            v
        };
        if ns(a) != ns(b) {
            let list = |x: &XmlIndex| x.namespaces.iter().map(|n| n.uri.clone()).collect::<Vec<_>>().join(", ");
            d.add("Selective XML index namespace", Some(list(a)), Some(list(b)));
        }
    }

    fn foreign_keys(&self, d: &mut Diffs, a: &Table, b: &Table, mapping: Option<&TableMapping>) {
        let o = self.opts;
        let left: Vec<&ForeignKey> = a.foreign_keys.iter().collect();
        let right: Vec<&ForeignKey> = b.foreign_keys.iter().collect();
        let mapped_col = |c: &String| {
            mapping
                .and_then(|m| m.columns.iter().find(|(s, _)| self.same_name(s, c)))
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| c.clone())
        };
        let compare = |x: &ForeignKey, y: &ForeignKey, d: &mut Diffs| {
            d.prop("ON UPDATE", &x.on_update, &y.on_update);
            d.prop("ON DELETE", &x.on_delete, &y.on_delete);
            if !o.has(Opt::IgnoreNotForReplication) {
                d.prop("NOT FOR REPLICATION", x.not_for_replication, y.not_for_replication);
            }
            let referenced = |t: &ObjectName, schema_map: bool| {
                let schema = if schema_map { self.target_schema(&t.schema) } else { t.schema.clone() };
                format!("{}.{}", self.key(&schema), self.key(&t.name))
            };
            if referenced(&x.referenced_table, true) != referenced(&y.referenced_table, false) {
                d.add("Referenced table", Some(x.referenced_table.to_string()), Some(y.referenced_table.to_string()));
            }
            if !o.has(Opt::IgnoreWithNocheck) && !o.has(Opt::IgnoreNocheckAndWithNocheck) {
                d.prop("WITH NOCHECK", x.not_trusted, y.not_trusted);
            }
            if !o.has(Opt::IgnoreNocheckAndWithNocheck) {
                d.prop("NOCHECK", x.disabled, y.disabled);
            }
            let cols = |v: &[String], map: bool| {
                v.iter().map(|c| self.key(&if map { mapped_col(c) } else { c.clone() })).collect::<Vec<_>>()
            };
            if cols(&x.columns, true) != cols(&y.columns, false) {
                d.add("Columns", Some(x.columns.join(", ")), Some(y.columns.join(", ")));
            }
            if cols(&x.referenced_columns, false) != cols(&y.referenced_columns, false) {
                d.add("Referenced columns", Some(x.referenced_columns.join(", ")), Some(y.referenced_columns.join(", ")));
            }
        };
        self.pair_named(d, "Foreign key", &left, &right, |k| (&k.name, k.system_named), compare);
    }

    fn checks(&self, d: &mut Diffs, a: &[CheckConstraint], b: &[CheckConstraint]) {
        let o = self.opts;
        let left: Vec<&CheckConstraint> = a.iter().collect();
        let right: Vec<&CheckConstraint> = b.iter().collect();
        let compare = |x: &CheckConstraint, y: &CheckConstraint, d: &mut Diffs| {
            if !o.has(Opt::IgnoreNotForReplication) {
                d.prop("NOT FOR REPLICATION", x.not_for_replication, y.not_for_replication);
            }
            if !o.has(Opt::IgnoreNocheckAndWithNocheck) {
                d.prop("NOCHECK", x.disabled, y.disabled);
                if !o.has(Opt::IgnoreWithNocheck) {
                    d.prop("WITH NOCHECK", x.not_trusted, y.not_trusted);
                }
            }
            if text::compare(&x.definition, &y.definition, TextKind::Expression, o).is_some() {
                d.add("Expression", Some(x.definition.clone()), Some(y.definition.clone()));
            }
        };
        self.pair_named(d, "Check constraint", &left, &right, |c| (&c.name, c.system_named), compare);
    }

    fn triggers(&self, d: &mut Diffs, a: &[Trigger], b: &[Trigger]) {
        let o = self.opts;
        let keep = |t: &&Trigger| {
            !(o.has(Opt::IgnoreInsteadOfTriggers) && t.instead_of) && !(o.has(Opt::IgnoreReplicationTriggers) && t.not_for_replication)
        };
        let left: Vec<&Trigger> = a.iter().filter(keep).collect();
        let right: Vec<&Trigger> = b.iter().filter(keep).collect();
        let mut used = vec![false; right.len()];
        for x in &left {
            match right.iter().enumerate().position(|(j, y)| !used[j] && self.same_name(&x.name, &y.name)) {
                Some(j) => {
                    used[j] = true;
                    let y = right[j];
                    d.enter("Trigger", Some(&x.name), Some(&y.name));
                    d.prop("Disabled", x.disabled, y.disabled);
                    self.module_text(d, &x.definition, &y.definition, x.encrypted, y.encrypted);
                    d.prop("Events", x.events.join(", "), y.events.join(", "));
                    if !o.has(Opt::IgnoreInsteadOfTriggers) {
                        d.prop("INSTEAD OF", x.instead_of, y.instead_of);
                    }
                    if !o.has(Opt::IgnoreNotForReplication) {
                        d.prop("NOT FOR REPLICATION", x.not_for_replication, y.not_for_replication);
                    }
                    if !x.instead_of && !o.has(Opt::IgnoreTriggerOrder) {
                        let order = |t: &Trigger| t.order.iter().map(|(e, w)| format!("{e} {w}")).collect::<Vec<_>>().join(", ");
                        d.prop("Trigger order", order(x), order(y));
                    }
                    d.leave();
                }
                None => d.only1("Trigger", &x.name),
            }
        }
        for (j, y) in right.iter().enumerate() {
            if !used[j] {
                d.only2("Trigger", &y.name);
            }
        }
    }

    pub(super) fn module_text(&self, d: &mut Diffs, a: &str, b: &str, enc_a: bool, enc_b: bool) {
        let o = self.opts;
        if enc_a || enc_b {
            // Without decryption only the flag can be compared.
            d.prop("Encrypted", enc_a, enc_b);
            return;
        }
        let mut a = a.to_string();
        let mut b = b.to_string();
        if o.has(Opt::IgnoreWithEncryption) {
            a = strip_with_encryption(&a);
            b = strip_with_encryption(&b);
        }
        let opts = o.clone().with(Opt::IgnoreOwners);
        if let Some(t) = text::compare(&a, &b, TextKind::Module, &opts) {
            d.add("Text", Some(t.source), Some(t.target));
        }
    }

    pub(super) fn settings(&self, d: &mut Diffs, a: &ModuleSettings, b: &ModuleSettings) {
        if !self.opts.has(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings) {
            d.prop("ANSI_NULLS", a.ansi_nulls, b.ansi_nulls);
            d.prop("QUOTED_IDENTIFIER", a.quoted_identifier, b.quoted_identifier);
        }
    }

    fn view(&self, a: &View, b: &View) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.module_text(&mut d, &a.definition, &b.definition, a.encrypted, b.encrypted);
        self.settings(&mut d, &a.settings, &b.settings);
        if !self.opts.has(Opt::IgnoreTriggers) {
            self.triggers(&mut d, &a.triggers, &b.triggers);
        }
        self.indexes(&mut d, &a.indexes, &b.indexes, false);
        self.statistics(&mut d, &a.statistics, &b.statistics);
        self.fulltext_index(&mut d, a.fulltext_index.as_ref(), b.fulltext_index.as_ref());
        self.xprops(&mut d, "VIEW", Some(&a.schema), &a.name, &b.name);
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    fn routine(&self, a: &Routine, b: &Routine) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        let clr = |r: &Routine| r.clr_method.is_some();
        d.prop("CLR", clr(a), clr(b));
        if clr(a) && clr(b) {
            d.opt_prop("CLR method", a.clr_method.as_deref(), b.clr_method.as_deref());
            d.opt_prop("EXECUTE AS", a.execute_as.as_deref(), b.execute_as.as_deref());
            self.clr_signature(&mut d, a, b);
        } else {
            d.prop("Natively compiled", a.native_compilation, b.native_compilation);
            self.module_text(&mut d, &a.definition, &b.definition, a.encrypted, b.encrypted);
        }
        self.settings(&mut d, &a.settings, &b.settings);
        let class = if is_function(a) { "FUNCTION" } else { "PROCEDURE" };
        self.xprops(&mut d, class, Some(&a.schema), &a.name, &b.name);
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    fn udt(&self, a: &UserType, b: &UserType) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        d.prop("Kind", format!("{:?}", a.kind), format!("{:?}", b.kind));
        d.prop("Allow NULLs", a.nullable, b.nullable);
        d.opt_prop("Base type", a.base_type.as_ref().map(DataType::sql).as_deref(), b.base_type.as_ref().map(DataType::sql).as_deref());
        d.opt_prop("CLR class", a.clr_class.as_deref(), b.clr_class.as_deref());
        if a.kind == UserTypeKind::Table && b.kind == UserTypeKind::Table {
            // Table types: compare as tables; their constraint names are always generated.
            let opts = self.opts.clone().with(Opt::IgnoreConstraintNames);
            let sub = Comparer::new(self.db1, self.db2, &opts, self.mappings);
            let as_table = |t: &UserType| Table {
                schema: t.schema.clone(),
                name: t.name.clone(),
                columns: t.columns.clone(),
                indexes: t.indexes.clone(),
                check_constraints: t.check_constraints.clone(),
                memory_optimized: t.memory_optimized,
                ..Default::default()
            };
            let (ta, tb) = (as_table(a), as_table(b));
            let mut sub = sub;
            sub.columns(&mut d, &ta, &tb, None);
            sub.indexes(&mut d, &ta.indexes, &tb.indexes, ta.memory_optimized);
            sub.checks(&mut d, &ta.check_constraints, &tb.check_constraints);
            d.prop("Memory optimized", a.memory_optimized, b.memory_optimized);
        }
        self.xprops(&mut d, "TYPE", Some(&a.schema), &a.name, &b.name);
        d.out
    }

    fn sequence(&self, a: &Sequence, b: &Sequence) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        d.prop("Data type", a.data_type.sql(), b.data_type.sql());
        d.prop("START WITH", &a.start, &b.start);
        d.prop("INCREMENT BY", &a.increment, &b.increment);
        d.prop("MINVALUE", &a.minimum, &b.minimum);
        d.prop("MAXVALUE", &a.maximum, &b.maximum);
        d.prop("CYCLE", a.cycle, b.cycle);
        d.prop("CACHE", a.cached, b.cached);
        if a.cached && b.cached {
            d.opt_prop("Cache size", a.cache_size.map(|v| v.to_string()).as_deref(), b.cache_size.map(|v| v.to_string()).as_deref());
        }
        self.xprops(&mut d, "SEQUENCE", Some(&a.schema), &a.name, &b.name);
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    fn synonym(&self, a: &Synonym, b: &Synonym) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        let parts = |s: &str| {
            let mut p: Vec<String> = s.split('.').map(|x| self.key(x.trim_matches(['[', ']']))).collect();
            if self.opts.has(Opt::IgnoreDatabaseAndServerName) && p.len() > 2 {
                p.drain(..p.len() - 2);
            }
            p
        };
        if parts(&a.base_object) != parts(&b.base_object) {
            d.add("Referenced object", Some(a.base_object.clone()), Some(b.base_object.clone()));
        }
        self.xprops(&mut d, "SYNONYM", Some(&a.schema), &a.name, &b.name);
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    /// Users (unless IgnoreUsers) and roles; role membership is reported on the member.
    fn principals(&mut self) {
        let o = self.opts;
        let keep = |p: &&Principal| p.is_role() || !o.has(Opt::IgnoreUsers);
        let left: Vec<&Principal> = self.db1.principals.iter().filter(keep).collect();
        let right: Vec<&Principal> = self.db2.principals.iter().filter(keep).collect();
        let empty = String::new();
        for (ty, role) in [(ObjectType::Role, true), (ObjectType::User, false)] {
            let l: Vec<&Principal> = left.iter().copied().filter(|p| p.is_role() == role).collect();
            let r: Vec<&Principal> = right.iter().copied().filter(|p| p.is_role() == role).collect();
            self.objects(ty, &l, &r, |p| (&empty, &p.name), |c, a, b| c.principal(a, b));
        }
        for diff in &mut self.result.differences {
            if matches!(diff.object_type, ObjectType::Role | ObjectType::User) {
                diff.name.schema.clear();
            }
        }
    }

    fn principal(&self, a: &Principal, b: &Principal) -> Vec<PropDiff> {
        let o = self.opts;
        let mut d = Diffs::default();
        d.prop("Type", &a.kind, &b.kind);
        if !a.is_role() && !o.has(Opt::IgnoreUserProperties) {
            // Windows logins compare without their domain.
            let login = |p: &Principal| p.login.as_deref().map(|l| self.key(l.rsplit('\\').next().unwrap_or(l)));
            if login(a) != login(b) {
                d.add("Login", a.login.clone(), b.login.clone());
            }
            d.opt_prop("Default schema", a.default_schema.as_deref(), b.default_schema.as_deref());
            d.opt_prop("Certificate", a.certificate.as_deref(), b.certificate.as_deref());
            d.opt_prop("Asymmetric key", a.asymmetric_key.as_deref(), b.asymmetric_key.as_deref());
        }
        if a.kind == "APPLICATION_ROLE" && b.kind == "APPLICATION_ROLE" {
            d.opt_prop("Default schema", a.default_schema.as_deref(), b.default_schema.as_deref());
        }
        if a.is_role() && !o.has(Opt::IgnoreOwners) && !o.has(Opt::IgnoreSchemaObjectAuthorization) {
            d.opt_prop("Owner", a.owner.as_deref(), b.owner.as_deref());
        }
        let roles = |p: &Principal| {
            let mut v: Vec<String> = p.member_of.iter().map(|r| self.key(r)).collect();
            v.sort();
            v
        };
        if roles(a) != roles(b) {
            d.add("Member of", Some(a.member_of.join(", ")), Some(b.member_of.join(", ")));
        }
        // Database-level permissions belong to their grantee.
        if !o.has(Opt::IgnorePermissions) {
            let of = |db: &Database, name: &str| -> Vec<String> {
                let mut v: Vec<String> =
                    db.permissions.iter().filter(|p| p.class == "DATABASE" && self.same_name(&p.grantee, name)).map(|p| p.grant_sql()).collect();
                v.sort();
                v
            };
            let (x, y) = (of(self.db1, &a.name), of(self.db2, &b.name));
            for g in x.iter().filter(|g| !y.iter().any(|h| self.key(h) == self.key(g))) {
                d.add("Permission", Some(g.clone()), None);
            }
            for g in y.iter().filter(|g| !x.iter().any(|h| self.key(h) == self.key(g))) {
                d.add("Permission", None, Some(g.clone()));
            }
        }
        d.out
    }

    /// GRANT/DENY on one securable. With IgnoreUsers only roles' permissions count.
    pub(super) fn permissions(&self, d: &mut Diffs, class: &str, schema1: &str, name1: Option<&str>, schema2: &str, name2: Option<&str>) {
        if self.opts.has(Opt::IgnorePermissions) {
            return;
        }
        let ignore_users = self.opts.has(Opt::IgnoreUsers);
        let of = |db: &Database, schema: &str, name: Option<&str>| -> Vec<(String, String)> {
            let mut v: Vec<(String, String)> = db
                .permissions
                .iter()
                .filter(|p| {
                    p.class == class
                        && self.same_name(p.schema.as_deref().unwrap_or_default(), schema)
                        && match (name, p.object.as_deref()) {
                            (Some(n), Some(o)) => self.same_name(n, o),
                            (None, None) => true,
                            _ => false,
                        }
                        && (!ignore_users || p.grantee == "public" || db.principals.iter().any(|x| x.is_role() && x.name == p.grantee))
                })
                // Compared without the securable, which may be named differently on each side.
                .map(|p| {
                    (
                        self.key(&format!("{} {} {}", p.state, p.action(), p.grantee)),
                        format!("{} {} TO {}", p.state.replace('_', " "), p.action(), p.grantee),
                    )
                })
                .collect();
            v.sort();
            v
        };
        let (x, y) = (of(self.db1, schema1, name1), of(self.db2, schema2, name2));
        for (k, text) in &x {
            if !y.iter().any(|(k2, _)| k2 == k) {
                d.add("Permission", Some(text.clone()), None);
            }
        }
        for (k, text) in &y {
            if !x.iter().any(|(k2, _)| k2 == k) {
                d.add("Permission", None, Some(text.clone()));
            }
        }
    }

    fn database_properties(&mut self) {
        let dbp = |db: &'a Database| db.extended_properties.iter().filter(|p| p.class == "DATABASE").collect::<Vec<_>>();
        let (left, right) = (dbp(self.db1), dbp(self.db2));
        let empty = String::new();
        self.objects(
            ObjectType::ExtendedProperty,
            &left,
            &right,
            |p| (&empty, &p.name),
            |c, a, b| {
                let mut d = Diffs::default();
                c.xprop_value(&mut d, a, b);
                d.out
            },
        );
    }

    /// Compares the object-level extended properties of an object (no minor item).
    pub(super) fn xprops(&self, d: &mut Diffs, class: &str, schema: Option<&str>, name1: &str, name2: &str) {
        let pick = |db: &'a Database, name: &str, schema: Option<String>| {
            db.extended_properties
                .iter()
                .filter(move |p| {
                    p.class == class
                        && p.minor_type.is_none()
                        && match (class, &schema) {
                            ("SCHEMA", _) => p.schema.as_deref().is_some_and(|s| self.same_name(s, name)),
                            (_, Some(s)) => {
                                p.schema.as_deref().is_some_and(|x| self.same_name(x, s))
                                    && p.object.as_deref().is_some_and(|o| self.same_name(o, name))
                            }
                            _ => false,
                        }
                })
                .collect::<Vec<_>>()
        };
        let s1 = schema.map(str::to_string);
        let s2 = schema.map(|s| self.target_schema(s));
        let (a, b) = (pick(self.db1, name1, s1), pick(self.db2, name2, s2));
        self.xprop_lists(d, &a, &b);
        // Constraint, index and trigger properties hang under the object at level 2.
        for minor in ["CONSTRAINT", "INDEX", "TRIGGER", "PARAMETER"] {
            let pick = |db: &'a Database, name: &str, schema: Option<String>| {
                db.extended_properties
                    .iter()
                    .filter(|p| {
                        p.class == class
                            && p.minor_type.as_deref() == Some(minor)
                            && p.schema.as_deref().zip(schema.as_deref()).is_some_and(|(x, s)| self.same_name(x, s))
                            && p.object.as_deref().is_some_and(|o| self.same_name(o, name))
                    })
                    .collect::<Vec<_>>()
            };
            let a = pick(self.db1, name1, schema.map(str::to_string));
            let b = pick(self.db2, name2, schema.map(|s| self.target_schema(s)));
            self.xprop_lists(d, &a, &b);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn xprops_minor(&self, d: &mut Diffs, class: &str, schema: &str, name1: &str, name2: &str, minor: &str, m1: Option<String>, m2: Option<String>) {
        let pick = |db: &'a Database, schema: &str, name: &str, m: &Option<String>| {
            db.extended_properties
                .iter()
                .filter(|p| {
                    p.class == class
                        && p.minor_type.as_deref() == Some(minor)
                        && p.schema.as_deref().is_some_and(|x| self.same_name(x, schema))
                        && p.object.as_deref().is_some_and(|o| self.same_name(o, name))
                        && p.minor_name.as_deref().zip(m.as_deref()).is_some_and(|(x, y)| self.same_name(x, y))
                })
                .collect::<Vec<_>>()
        };
        let a = pick(self.db1, schema, name1, &m1);
        let b = pick(self.db2, &self.target_schema(schema), name2, &m2);
        self.xprop_lists(d, &a, &b);
    }

    fn xprop_lists(&self, d: &mut Diffs, a: &[&ExtendedProperty], b: &[&ExtendedProperty]) {
        if self.opts.has(Opt::IgnoreExtendedProperties) {
            return;
        }
        let internal = |p: &&&ExtendedProperty| {
            self.opts.has(Opt::IgnoreInternallyUsedMicrosoftExtendedProperties) && p.name.starts_with("MS_DiagramPane")
        };
        let label = |p: &ExtendedProperty| match &p.minor_name {
            Some(m) => format!("{} ({m})", p.name),
            None => p.name.clone(),
        };
        let mut used = vec![false; b.len()];
        for x in a.iter().filter(|p| !internal(p)) {
            match b.iter().enumerate().position(|(j, y)| !used[j] && self.same_name(&x.name, &y.name) && x.minor_name == y.minor_name) {
                Some(j) => {
                    used[j] = true;
                    d.enter("Extended property", Some(&label(x)), Some(&label(b[j])));
                    self.xprop_value(d, x, b[j]);
                    d.leave();
                }
                None => d.only1("Extended property", &label(x)),
            }
        }
        for (j, y) in b.iter().enumerate() {
            if !used[j] && !internal(&y) {
                d.only2("Extended property", &label(y));
            }
        }
    }

    fn xprop_value(&self, d: &mut Diffs, a: &ExtendedProperty, b: &ExtendedProperty) {
        let norm = |v: &Option<String>| {
            v.as_deref().map(|s| {
                let s = if self.opts.has(Opt::IgnoreWhiteSpace) { s.split_whitespace().collect::<Vec<_>>().join(" ") } else { s.to_string() };
                if self.case_sensitive { s } else { s.to_lowercase() }
            })
        };
        if norm(&a.value) != norm(&b.value) {
            d.add("Value", a.value.clone(), b.value.clone());
        }
    }
}

fn is_xprop(p: &PropDiff) -> bool {
    p.name == "Extended property" || p.location.iter().any(|f| f.name == "Extended property")
}

fn is_graph_column(c: &Column) -> bool {
    c.name.starts_with("graph_id_") || c.name.starts_with('$')
        || c.name.starts_with("from_obj_id_") || c.name.starts_with("from_id_") || c.name.starts_with("to_obj_id_") || c.name.starts_with("to_id_")
}

fn compression(s: &Storage) -> String {
    if s.compression.is_empty() {
        return "NONE".into();
    }
    s.compression
        .iter()
        .map(|p| if p.partition == 0 { p.compression.clone() } else { format!("{}:{}", p.partition, p.compression) })
        .collect::<Vec<_>>()
        .join(", ")
}

fn strip_with_encryption(text: &str) -> String {
    let re_like = ["WITH ENCRYPTION,", ", ENCRYPTION", ",ENCRYPTION", "WITH ENCRYPTION"];
    let upper = text.to_ascii_uppercase();
    for pat in re_like {
        if let Some(p) = upper.find(pat) {
            let mut out = text.to_string();
            out.replace_range(p..p + pat.len(), if pat.starts_with("WITH ENCRYPTION,") { "WITH" } else { "" });
            return out;
        }
    }
    text.to_string()
}

/// The differences between two columns under the options (empty when equal).
pub(crate) fn column_differences(a: &Column, b: &Column, o: &Options, memory_optimized: bool) -> Vec<PropDiff> {
    let mut d = Diffs::default();
    column_props(&mut d, a, b, o, memory_optimized, false);
    d.out
}

/// Column properties (spec §4.1). `memory_optimized` makes collation always count;
/// `ignore_default` is used when probing for auto-mapped columns.
fn column_props(d: &mut Diffs, a: &Column, b: &Column, o: &Options, memory_optimized: bool, ignore_default: bool) {
    let computed = a.computed.is_some();
    if !o.has(Opt::IgnoreNullability) && !(computed && !a.computed.as_ref().is_some_and(|c| c.persisted)) {
        d.prop("Allow NULLs", a.nullable, b.nullable);
    }
    d.prop("Computed", a.computed.is_some(), b.computed.is_some());
    if let (Some(x), Some(y)) = (&a.computed, &b.computed) {
        d.prop("Persisted", x.persisted, y.persisted);
        if text::compare(&x.definition, &y.definition, TextKind::Expression, o).is_some() {
            d.add("Computed definition", Some(x.definition.clone()), Some(y.definition.clone()));
        }
    }
    d.opt_prop("GENERATED ALWAYS", a.generated_always.as_deref(), b.generated_always.as_deref());
    d.prop("Hidden", a.hidden, b.hidden);
    if !ignore_default {
        default_props(d, a.default.as_ref(), b.default.as_ref(), o);
    }
    if !o.has(Opt::IgnoreIdentityProperties) {
        d.prop("Identity", a.identity.is_some(), b.identity.is_some());
        if let (Some(x), Some(y)) = (&a.identity, &b.identity) {
            if !o.has(Opt::IgnoreIdentitySeedAndIncrement) {
                d.prop("Identity seed", &x.seed, &y.seed);
                d.prop("Identity increment", &x.increment, &y.increment);
            }
            if !o.has(Opt::IgnoreNotForReplication) {
                d.prop("NOT FOR REPLICATION", x.not_for_replication, y.not_for_replication);
            }
        }
    }
    d.prop("ROWGUIDCOL", a.rowguidcol, b.rowguidcol);
    if !o.has(Opt::IgnoreBindings) {
        let key = |n: &Option<String>| n.as_ref().map(|s| if o.has(Opt::CaseSensitiveObjectDefinition) { s.clone() } else { s.to_lowercase() });
        if key(&a.bound_default) != key(&b.bound_default) {
            d.add("Bound default", a.bound_default.clone(), b.bound_default.clone());
        }
        if key(&a.bound_rule) != key(&b.bound_rule) {
            d.add("Bound rule", a.bound_rule.clone(), b.bound_rule.clone());
        }
    }
    if !computed && (memory_optimized || !o.has(Opt::IgnoreCollations)) {
        d.opt_prop("Collation", a.collation.as_deref(), b.collation.as_deref());
    }
    d.prop("SPARSE", a.sparse, b.sparse);
    d.prop("COLUMN_SET", a.column_set, b.column_set);
    d.prop("FILESTREAM", a.filestream, b.filestream);
    if !o.has(Opt::IgnoreDynamicDataMasking) {
        let mask = |c: &Column| c.masking_function.as_ref().map(|m| m.to_lowercase());
        if mask(a) != mask(b) {
            d.add("Masking function", a.masking_function.clone(), b.masking_function.clone());
        }
    }
    if !computed {
        let t = |c: &Column| {
            let mut ty = c.data_type.clone();
            if !o.has(Opt::CaseSensitiveObjectDefinition) {
                ty.name = ty.name.to_lowercase();
                ty.schema = ty.schema.map(|s| s.to_lowercase());
            }
            ty
        };
        if t(a) != t(b) {
            d.add("Data type", Some(a.data_type.sql()), Some(b.data_type.sql()));
        }
    }
}

fn default_props(d: &mut Diffs, a: Option<&DefaultConstraint>, b: Option<&DefaultConstraint>, o: &Options) {
    match (a, b) {
        (None, None) => {}
        (Some(x), None) => d.add("Default value", Some(x.definition.clone()), None),
        (None, Some(y)) => d.add("Default value", None, Some(y.definition.clone())),
        (Some(x), Some(y)) => {
            let names_ignored =
                o.has(Opt::IgnoreConstraintNames) || (o.has(Opt::IgnoreSystemNamedConstraintNames) && (x.system_named || y.system_named));
            if !names_ignored && !x.name.eq_ignore_ascii_case(&y.name) {
                d.add("Default name", Some(x.name.clone()), Some(y.name.clone()));
            }
            if text::compare(&x.definition, &y.definition, TextKind::Expression, o).is_some() {
                d.add("Default value", Some(x.definition.clone()), Some(y.definition.clone()));
            }
        }
    }
}

/// Edit distance with two rows of memory.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
