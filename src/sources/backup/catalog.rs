//! The model of a database read from its system base tables in a backup: what the catalog
//! views of a live database would show, derived from the same tables they are defined over.

use std::collections::{BTreeMap, HashMap};

use super::collation;
use super::tables::{Catalog, SysRow};
use super::values::{self, type_name};
use crate::catalog::*;
use crate::data::value::Value;

/// Facts about the database outside its tables (from the boot page and the backup set).
pub struct DatabaseInfo {
    pub name: String,
    pub collation: u32,
    pub compatibility_level: u32,
    pub version: u16,
    pub server: String,
}

/// The SQL Server major version that writes a database version.
pub fn major_version(dbi_version: u16) -> u32 {
    match dbi_version {
        0..=610 => 8,
        611..=660 => 9,
        661..=705 => 10,
        706..=781 => 11,
        782..=851 => 12,
        852..=868 => 13,
        869..=903 => 14,
        904..=956 => 15,
        _ => 16,
    }
}

#[derive(Clone, Debug, Default)]
struct Obj {
    id: i32,
    name: String,
    schema: i32,
    kind: String,
    parent: i32,
    status: i64,
    status2: i64,
    intprop: i64,
}

impl Obj {
    fn flag(&self, bit: i64) -> bool {
        self.status & bit != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Table(usize),
    View(usize),
    TableType(usize),
}

/// Object status bits.
mod st {
    pub const MS_SHIPPED: i64 = 0x1;
    pub const SYSTEM_NAMED: i64 = 0x4;
    pub const DISABLED: i64 = 0x100;
    pub const NOT_FOR_REPLICATION: i64 = 0x200;
    pub const NOT_TRUSTED: i64 = 0x400;
    pub const ENCRYPTED: i64 = 0x10000;
    pub const SCHEMA_BOUND: i64 = 0x20000;
    pub const ANSI_NULLS: i64 = 0x40000;
    pub const QUOTED_IDENTIFIER: i64 = 0x80000;
    pub const NATIVE: i64 = 0x200;
    pub const SEQ_CYCLE: i64 = 0x100;
    pub const SEQ_CACHED: i64 = 0x200;
    pub const TRIGGER_DISABLED: i64 = 0x100;
    pub const TRIGGER_INSTEAD_OF: i64 = 0x2;
}

/// Column status bits (syscolpars).
mod cs {
    pub const NOT_NULL: i64 = 0x1;
    pub const IDENTITY: i64 = 0x4;
    pub const ROWGUIDCOL: i64 = 0x8;
    pub const COMPUTED: i64 = 0x10;
    pub const FILESTREAM: i64 = 0x20;
    pub const PERSISTED: i64 = 0x40;
    pub const XML_DOCUMENT: i64 = 0x800;
    pub const HIDDEN: i64 = 0x2000;
    pub const ID_NOT_FOR_REPLICATION: i64 = 0x10000;
    pub const SPARSE: i64 = 0x1000000;
    pub const COLUMN_SET: i64 = 0x2000000;
    pub const ROW_START: i64 = 0x10000000;
    pub const ROW_END: i64 = 0x20000000;
}

/// Reference classes (syssingleobjrefs / sysmultiobjrefs).
mod rc {
    pub const INDEX_TO_DATA_SPACE: i64 = 7;
    pub const INDEX_TO_LOB_DATA_SPACE: i64 = 8;
    pub const EXECUTE_AS: i64 = 22;
    pub const ROLE_MEMBER: i64 = 25;
    pub const FK_REFERENCED_INDEX: i64 = 27;
    pub const FK_PARENT_COLUMN: i64 = 28;
    pub const FK_REFERENCED_COLUMN: i64 = 29;
    pub const TYPE_TO_TABLE: i64 = 36;
    pub const OBJECT_TO_FILESTREAM_DATA_SPACE: i64 = 42;
    pub const OBJECT_TO_TYPE: i64 = 43;
    pub const SCHEMA_OWNER: i64 = 50;
    pub const ROLE_OWNER: i64 = 51;
    pub const CONSTRAINT_TO_INDEX: i64 = 93;
    pub const TEMPORAL_HISTORY: i64 = 112;
}

/// Value classes (sysobjvalues).
mod vc {
    pub const DEFINITION: i64 = 1;
    pub const COMPUTED: i64 = 2;
    pub const SYNONYM_BASE: i64 = 4;
    pub const SEQUENCE: i64 = 63;
    pub const INDEX_FILTER: i64 = 66;
    pub const TRIGGER_EVENT: i64 = 70;
    pub const GENERATED_ALWAYS: i64 = 119;
    pub const MASK: i64 = 120;
    pub const RETENTION: i64 = 123;
    pub const STATS_FILTER: i64 = 67;
}

struct Builder<'a> {
    cat: &'a Catalog<'a>,
    db: Database,
    objects: HashMap<i32, Obj>,
    schema_names: HashMap<i32, String>,
    principals: HashMap<i64, String>,
    types: HashMap<i64, SysRow>,
    data_spaces: HashMap<i64, String>,
    /// (valclass, objid) → rows.
    values: HashMap<(i64, i64), Vec<SysRow>>,
    /// class → rows.
    single: HashMap<i64, Vec<SysRow>>,
    multi: HashMap<i64, Vec<SysRow>>,
    owners: HashMap<i32, Owner>,
    index_slots: HashMap<(i32, i32), (Owner, usize)>,
    /// Objects excluded as tools-support objects (diagrams).
    shipped: std::collections::HashSet<i32>,
    xprops: Vec<SysRow>,
    /// Names sort without regard to case (the database collation is case-insensitive).
    ci: bool,
}

/// Reads the model from the base tables.
pub fn read(cat: &Catalog, info: &DatabaseInfo) -> Result<Database, String> {
    let mut b = Builder {
        cat,
        db: Database { model_version: MODEL_VERSION, ..Default::default() },
        objects: HashMap::new(),
        schema_names: HashMap::new(),
        principals: HashMap::new(),
        types: HashMap::new(),
        data_spaces: HashMap::new(),
        values: HashMap::new(),
        single: HashMap::new(),
        multi: HashMap::new(),
        owners: HashMap::new(),
        index_slots: HashMap::new(),
        shipped: Default::default(),
        xprops: cat.system("sysxprops"),
        ci: collation::name(info.collation).is_none_or(|n| n.contains("_CI")),
    };
    let major = major_version(info.version);
    b.db.server = ServerInfo { major_version: major, product_version: format!("{major}.0"), edition: String::new(), engine_edition: 3 };
    b.db.name = info.name.clone();
    b.db.collation = collation::name(info.collation).unwrap_or_default();
    b.db.compatibility_level = info.compatibility_level;
    b.load();
    b.schemas();
    b.types();
    b.tables();
    b.columns();
    b.indexes();
    b.partitions();
    b.foreign_keys();
    b.check_constraints();
    b.modules();
    b.triggers();
    b.sequences();
    b.synonyms();
    b.bindables();
    b.statistics();
    b.extended_properties();
    b.principals_and_roles();
    b.permissions();
    b.dependencies();
    Ok(b.finish())
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Variant(v) => Some(v.text.clone()),
        Value::Null => None,
        other => Some(other.display()),
    }
}

impl Builder<'_> {
    fn load(&mut self) {
        for r in self.cat.table_rows(34) {
            let o = Obj {
                id: r.int("id") as i32,
                name: r.str("name"),
                schema: r.int("nsid") as i32,
                kind: r.str("type").trim().to_string(),
                parent: r.int("pid") as i32,
                status: r.int("status"),
                status2: r.int("status2"),
                intprop: r.int("intprop"),
            };
            // DDL triggers live in their own namespace classes.
            if r.int("nsclass") >= 20 && o.kind == "TR" && r.int("pclass") == 0 {
                self.objects.insert(o.id, Obj { parent: 0, ..o });
                continue;
            }
            self.objects.insert(o.id, o);
        }
        for r in self.cat.system("sysobjvalues") {
            self.values.entry((r.int("valclass"), r.int("objid"))).or_default().push(r);
        }
        for r in self.cat.system("syssingleobjrefs") {
            self.single.entry(r.int("class")).or_default().push(r);
        }
        for r in self.cat.system("sysmultiobjrefs") {
            self.multi.entry(r.int("class")).or_default().push(r);
        }
        for r in self.cat.system("sysowners") {
            self.principals.insert(r.int("id"), r.str("name"));
        }
        for r in self.cat.system("sysscalartypes") {
            self.types.insert(r.int("id"), r);
        }
        for r in self.cat.system("sysclsobjs") {
            match r.int("class") {
                50 => {
                    self.schema_names.insert(r.int("id") as i32, r.str("name"));
                }
                31 => {
                    self.data_spaces.insert(r.int("id"), r.str("name"));
                }
                _ => {}
            }
        }
        // Objects marked as database tools support (diagram objects) are not user objects.
        for x in &self.xprops {
            if x.int("class") == 1 && x.int("subid") == 0 && x.str("name") == "microsoft_database_tools_support" {
                self.shipped.insert(x.int("id") as i32);
            }
        }
    }

    /// The sort key of a name under the database collation.
    fn key(&self, s: &str) -> String {
        if self.ci { s.to_lowercase() } else { s.to_string() }
    }

    fn user_object(&self, o: &Obj) -> bool {
        !o.flag(st::MS_SHIPPED) && !self.shipped.contains(&o.id)
    }

    fn name_of(&self, id: i32) -> Option<ObjectName> {
        let o = self.objects.get(&id)?;
        Some(ObjectName::new(self.schema_names.get(&o.schema).map(String::as_str).unwrap_or(""), &o.name))
    }

    fn value(&self, class: i64, objid: i64, subobjid: i64, valnum: i64) -> Option<&SysRow> {
        self.values.get(&(class, objid))?.iter().find(|r| r.int("subobjid") == subobjid && r.int("valnum") == valnum)
    }

    /// OBJECT_DEFINITION: the text of a module, constraint or default.
    fn definition(&self, objid: i64, subobjid: Option<i64>) -> Option<String> {
        let rows = self.values.get(&(vc::DEFINITION, objid))?;
        let r = match subobjid {
            Some(s) => rows.iter().find(|r| r.int("subobjid") == s && r.int("valnum") == 0)?,
            None => rows.iter().filter(|r| r.int("valnum") == 0).min_by_key(|r| r.int("subobjid"))?,
        };
        Some(stored_text(r))
    }

    fn single_ref(&self, class: i64, depid: i64, depsubid: i64) -> Option<&SysRow> {
        self.single.get(&class)?.iter().find(|r| r.int("depid") == depid && r.int("depsubid") == depsubid)
    }

    fn principal(&self, id: i64) -> Option<String> {
        self.principals.get(&id).cloned()
    }

    fn schemas(&mut self) {
        let mut list: Vec<(i32, String)> = self.schema_names.iter().map(|(&k, v)| (k, v.clone())).collect();
        list.sort_by_key(|a| self.key(&a.1));
        for (id, name) in list {
            if id >= 16384 || matches!(name.as_str(), "sys" | "INFORMATION_SCHEMA" | "guest") {
                continue;
            }
            let owner = self.single_ref(rc::SCHEMA_OWNER, id as i64, 0).map(|r| r.int("indepid")).unwrap_or(1);
            self.db.schemas.push(Schema { name, owner: self.principal(owner).unwrap_or_default() });
        }
    }

    /// The data type of a column or type: user types by name, system types with their facets.
    fn data_type(&self, utype: i64, xtype: i64, length: i64, prec: i64, scale: i64) -> DataType {
        let ty = self.types.get(&utype);
        if utype > 256
            && let Some(t) = ty
        {
            let schema = self.schema_names.get(&(t.int("schid") as i32)).cloned();
            return DataType { name: t.str("name"), schema, ..Default::default() };
        }
        let name = match ty {
            Some(t) if utype > 0 => t.str("name"),
            _ => self.types.get(&xtype).map(|t| t.str("name")).unwrap_or_else(|| type_name(xtype as u8).to_string()),
        };
        // TYPE_NAME(system_type_id): sysname shows as nvarchar.
        let system = self.types.get(&xtype).map(|t| t.str("name")).unwrap_or(name);
        crate::catalog::read::system_type(&system, length as i32, prec as i32, scale as i32)
    }

    fn types(&mut self) {
        let mut rows: Vec<&SysRow> = self.types.values().filter(|t| t.int("id") > 256).collect();
        rows.sort_by_key(|t| (self.key(self.schema_names.get(&(t.int("schid") as i32)).map(String::as_str).unwrap_or("")), self.key(&t.str("name"))));
        let mut types = Vec::new();
        let mut owners = Vec::new();
        for t in rows {
            let schema = self.schema_names.get(&(t.int("schid") as i32)).cloned().unwrap_or_default();
            let mut ty = UserType { schema, name: t.str("name"), nullable: t.int("status") & 1 == 0, ..Default::default() };
            match t.int("xtype") {
                243 => {
                    ty.kind = UserTypeKind::Table;
                    ty.nullable = false;
                    if let Some(r) = self.single_ref(rc::TYPE_TO_TABLE, t.int("id"), 0) {
                        owners.push((r.int("indepid") as i32, types.len()));
                        if let Some(o) = self.objects.get(&(r.int("indepid") as i32)) {
                            ty.memory_optimized = o.status2 & 0x8 != 0;
                        }
                    }
                }
                240 => ty.kind = UserTypeKind::Clr,
                x => ty.base_type = Some(self.data_type(x, x, t.int("length"), t.int("prec"), t.int("scale"))),
            }
            types.push(ty);
        }
        for (id, i) in owners {
            self.owners.insert(id, Owner::TableType(i));
        }
        self.db.types = types;
    }

    fn sorted_objects(&self, kinds: &[&str]) -> Vec<Obj> {
        let mut v: Vec<Obj> = self.objects.values().filter(|o| kinds.contains(&o.kind.as_str()) && self.user_object(o)).cloned().collect();
        v.sort_by_key(|o| (self.key(self.schema_names.get(&o.schema).map(String::as_str).unwrap_or("")), self.key(&o.name)));
        v
    }

    fn data_space_of(&self, object: i32, index: i32) -> Option<String> {
        let id = self.single_ref(rc::INDEX_TO_DATA_SPACE, object as i64, index as i64).map(|r| r.int("indepid")).unwrap_or(1);
        self.data_spaces.get(&id).cloned()
    }

    fn tables(&mut self) {
        let mut history: Vec<(usize, i32)> = Vec::new();
        for o in self.sorted_objects(&["U"]) {
            let mut t = Table {
                schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(),
                name: o.name.clone(),
                lock_escalation: match (o.status >> 28) & 3 {
                    1 => "DISABLE",
                    2 => "AUTO",
                    _ => "TABLE",
                }
                .into(),
                ..Default::default()
            };
            let heap = if self.single_ref(rc::INDEX_TO_DATA_SPACE, o.id as i64, 1).is_some() { 1 } else { 0 };
            t.storage.data_space = self.data_space_of(o.id, heap);
            if let Some(r) = self.single.get(&rc::INDEX_TO_LOB_DATA_SPACE).and_then(|v| v.iter().find(|r| r.int("depid") == o.id as i64 && r.int("depsubid") <= 1)) {
                t.text_filegroup = self.data_spaces.get(&r.int("indepid")).cloned();
            }
            if let Some(r) = self.single_ref(rc::OBJECT_TO_FILESTREAM_DATA_SPACE, o.id as i64, 0) {
                t.filestream_filegroup = self.data_spaces.get(&r.int("indepid")).cloned();
            }
            t.memory_optimized = o.status2 & 0x8 != 0;
            if t.memory_optimized {
                t.storage.data_space = None;
            }
            if let Some(h) = self.single.get(&rc::TEMPORAL_HISTORY) {
                if let Some(r) = h.iter().find(|r| r.int("indepid") == o.id as i64) {
                    history.push((self.db.tables.len(), r.int("depid") as i32));
                    let retention = self.value(vc::RETENTION, o.id as i64, 0, 0).map(|r| r.int("value")).filter(|&n| n >= 0).map(|n| {
                        let unit = match self.value(vc::RETENTION, o.id as i64, 0, 1).map(|r| r.int("value")).unwrap_or(-1) {
                            3 => "DAY",
                            4 => "WEEK",
                            5 => "MONTH",
                            6 => "YEAR",
                            _ => "INFINITE",
                        };
                        format!("{n} {unit}S")
                    });
                    t.temporal = Some(Temporal { retention, ..Default::default() });
                } else if h.iter().any(|r| r.int("depid") == o.id as i64) {
                    t.history_table = true;
                }
            }
            self.owners.insert(o.id, Owner::Table(self.db.tables.len()));
            self.db.tables.push(t);
        }
        for (table, id) in history {
            if let (Some(name), Some(temporal)) = (self.name_of(id), self.db.tables[table].temporal.as_mut()) {
                temporal.history_table = name;
            }
        }
        for o in self.sorted_objects(&["V"]) {
            self.owners.insert(o.id, Owner::View(self.db.views.len()));
            self.db.views.push(View { schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(), name: o.name.clone(), ..Default::default() });
        }
    }

    fn columns_of(&mut self, owner: Owner) -> &mut Vec<Column> {
        match owner {
            Owner::Table(t) => &mut self.db.tables[t].columns,
            Owner::View(v) => &mut self.db.views[v].columns,
            Owner::TableType(t) => &mut self.db.types[t].columns,
        }
    }

    fn indexes_of(&mut self, owner: Owner) -> &mut Vec<Index> {
        match owner {
            Owner::Table(t) => &mut self.db.tables[t].indexes,
            Owner::View(v) => &mut self.db.views[v].indexes,
            Owner::TableType(t) => &mut self.db.types[t].indexes,
        }
    }

    fn columns(&mut self) {
        let mut owners: Vec<(i32, Owner)> = self.owners.iter().map(|(&k, &v)| (k, v)).collect();
        owners.sort();
        for (object, owner) in owners {
            let mut pars: Vec<SysRow> = self.cat.colpars.get(&object).cloned().unwrap_or_default();
            pars.sort_by_key(|r| r.int("colid"));
            for c in pars {
                let column = self.column(object, &c);
                self.columns_of(owner).push(column);
            }
        }
    }

    fn column(&self, object: i32, c: &SysRow) -> Column {
        let status = c.int("status");
        let colid = c.int("colid");
        let (utype, xtype) = (c.int("utype"), c.int("xtype"));
        let mut data_type = self.data_type(utype, xtype, c.int("length"), c.int("prec"), c.int("scale"));
        if data_type.name == "xml" {
            data_type.xml_document = status & cs::XML_DOCUMENT != 0;
            let ns = c.int("xmlns");
            if ns != 0 {
                data_type.xml_collection = self.xml_collection_name(ns);
            }
        }
        let character = data_type.schema.is_none() && matches!(xtype, 175 | 167 | 239 | 231 | 35 | 99);
        let identity = (status & cs::IDENTITY != 0).then(|| {
            let (seed, increment) = identity_values(&c.bytes("idtval"), xtype as u8);
            Identity { seed, increment, not_for_replication: status & cs::ID_NOT_FOR_REPLICATION != 0 }
        });
        let computed = (status & cs::COMPUTED != 0).then(|| Computed {
            definition: self.value(vc::COMPUTED, object as i64, colid, 0).map(stored_text).unwrap_or_default(),
            persisted: status & cs::PERSISTED != 0,
        });
        let dflt = c.int("dflt") as i32;
        let (mut default, mut bound_default) = (None, None);
        if let Some(d) = self.objects.get(&dflt).filter(|d| d.kind == "D") {
            if d.parent == object {
                default = Some(DefaultConstraint {
                    name: d.name.clone(),
                    system_named: d.flag(st::SYSTEM_NAMED),
                    definition: self.definition(d.id as i64, None).unwrap_or_default(),
                });
            } else if d.parent == 0 {
                bound_default = self.name_of(d.id).map(|n| n.quoted());
            }
        }
        let rule = c.int("chk") as i32;
        let bound_rule = (rule != 0).then(|| self.name_of(rule).map(|n| n.quoted())).flatten();
        let generated_always = if status & cs::ROW_START != 0 {
            Some("ROW START".into())
        } else if status & cs::ROW_END != 0 {
            Some("ROW END".into())
        } else {
            match self.value(vc::GENERATED_ALWAYS, object as i64, colid, 0).map(|r| r.int("value")) {
                Some(7) => Some("TRANSACTION_ID START".into()),
                Some(8) => Some("TRANSACTION_ID END".into()),
                Some(9) => Some("SEQUENCE_NUMBER START".into()),
                Some(10) => Some("SEQUENCE_NUMBER END".into()),
                _ => None,
            }
        };
        let masking_function = self.value(vc::MASK, object as i64, colid, 1).and_then(|r| text_of(r.get("value")));
        Column {
            name: c.str("name"),
            ordinal: colid as u32,
            data_type,
            collation: if character { collation::name(c.int("collationid") as u32) } else { None },
            nullable: status & cs::NOT_NULL == 0,
            rowguidcol: status & cs::ROWGUIDCOL != 0,
            sparse: status & cs::SPARSE != 0,
            column_set: status & cs::COLUMN_SET != 0,
            filestream: status & cs::FILESTREAM != 0,
            identity,
            computed,
            default,
            generated_always,
            hidden: status & cs::HIDDEN != 0,
            masking_function,
            bound_default,
            bound_rule,
            sensitivity: None,
        }
    }

    fn xml_collection_name(&self, id: i64) -> Option<String> {
        let r = self.cat.system("sysxmlcomponent").into_iter().find(|r| r.int("id") == id && r.int("kind") == 0)?;
        let schema = self.schema_names.get(&(r.int("nsid") as i32)).cloned().unwrap_or_default();
        Some(ObjectName::new(&schema, &r.str("name")).quoted())
    }

    fn indexes(&mut self) {
        let mut rows = self.cat.system("sysidxstats");
        rows.sort_by_key(|r| (r.int("id"), r.int("indid")));
        let key_constraints: HashMap<(i64, i64), &Obj> = self
            .single
            .get(&rc::CONSTRAINT_TO_INDEX)
            .into_iter()
            .flatten()
            .filter_map(|r| self.objects.get(&(r.int("depid") as i32)).map(|o| ((o.parent as i64, r.int("indepid")), o)))
            .collect();
        let mut indexes: Vec<(i32, i32, Owner, Index)> = Vec::new();
        for r in &rows {
            let status = r.int("status");
            let (object, index_id) = (r.int("id") as i32, r.int("indid") as i32);
            if status & 1 == 0 || status & 0x100 != 0 || status & 0x04000000 != 0 {
                continue;
            }
            let Some(&owner) = self.owners.get(&object) else { continue };
            let kind = match r.int("type") {
                1 => IndexKind::Clustered,
                2 => IndexKind::Nonclustered,
                3 => IndexKind::Xml,
                4 => IndexKind::Spatial,
                5 => IndexKind::ClusteredColumnstore,
                6 => IndexKind::NonclusteredColumnstore,
                7 => IndexKind::NonclusteredHash,
                _ => continue,
            };
            let memory = self.objects.get(&object).is_some_and(|o| o.status2 & 0x8 != 0);
            let kc = key_constraints.get(&(object as i64, index_id as i64));
            let filter = (status & 0x20000 != 0).then(|| self.value(vc::INDEX_FILTER, object as i64, index_id as i64, 0).map(stored_text)).flatten();
            let index = Index {
                name: r.str("name"),
                kind,
                unique: status & 0x8 != 0,
                primary_key: status & 0x20 != 0,
                unique_constraint: status & 0x40 != 0,
                system_named: kc.is_some_and(|o| o.flag(st::SYSTEM_NAMED)),
                fill_factor: r.int("fillfact") as u8,
                pad_index: status & 0x10 != 0,
                ignore_dup_key: status & 0x4 != 0,
                allow_row_locks: !memory && status & 0x200 == 0,
                allow_page_locks: !memory && status & 0x400 == 0,
                disabled: status & 0x80 != 0,
                filter,
                storage: Storage { data_space: if memory { None } else { self.data_space_of(object, index_id) }, ..Default::default() },
                no_recompute: status & 0x4000 != 0,
                incremental: status & 0x800000 != 0,
                optimize_for_sequential_key: status & 0x800 != 0,
                ..Default::default()
            };
            indexes.push((object, index_id, owner, index));
        }
        for (object, index_id, owner, index) in indexes {
            let list = self.indexes_of(owner);
            list.push(index);
            let slot = list.len() - 1;
            self.index_slots.insert((object, index_id), (owner, slot));
        }
        let mut cols = self.cat.system("sysiscols");
        cols.sort_by_key(|r| (r.int("idmajor"), r.int("idminor"), r.int("tinyprop1"), r.int("subid")));
        for r in cols {
            if r.int("status") & 2 == 0 {
                continue;
            }
            let (object, index_id) = (r.int("idmajor") as i32, r.int("idminor") as i32);
            let Some(name) = self.cat.colpars.get(&object).and_then(|p| p.iter().find(|c| c.int("colid") == r.int("intprop"))).map(|c| c.str("name")) else { continue };
            if r.int("tinyprop2") > 0 {
                if index_id <= 1
                    && let Some(Owner::Table(t)) = self.owners.get(&object).copied()
                {
                    self.db.tables[t].storage.partition_column = Some(name.clone());
                }
                if let Some(&(owner, slot)) = self.index_slots.get(&(object, index_id)) {
                    self.indexes_of(owner)[slot].storage.partition_column = Some(name.clone());
                }
            }
            let Some(&(owner, slot)) = self.index_slots.get(&(object, index_id)) else { continue };
            let status = r.int("status");
            let index = &mut self.indexes_of(owner)[slot];
            if status & 0x10 != 0 {
                index.included.push(name);
            } else if r.int("tinyprop1") > 0 || matches!(index.kind, IndexKind::Xml | IndexKind::Spatial) {
                index.columns.push(IndexColumn { name, descending: status & 0x4 != 0 });
            } else if index.kind == IndexKind::NonclusteredColumnstore {
                index.columns.push(IndexColumn { name, descending: false });
            }
        }
    }

    fn partitions(&mut self) {
        let mut rows: Vec<(i32, i32, i32, i64)> = self
            .cat
            .rowset_rows
            .iter()
            .filter(|r| r.int("ownertype") == 1)
            .map(|r| (r.int("idmajor") as i32, r.int("idminor") as i32, r.int("numpart") as i32, r.int("cmprlevel")))
            .collect();
        rows.sort();
        let mut counts: HashMap<(i32, i32), i32> = HashMap::new();
        for r in &rows {
            *counts.entry((r.0, r.1)).or_default() += 1;
        }
        for (object, index_id, partition, level) in rows {
            let compression = match level {
                1 => "ROW",
                2 => "PAGE",
                3 => "COLUMNSTORE",
                4 => "COLUMNSTORE_ARCHIVE",
                _ => "NONE",
            }
            .to_string();
            if counts[&(object, index_id)] <= 1 && compression == "NONE" {
                continue;
            }
            let entry = PartitionCompression { partition: partition as u32, compression };
            if index_id <= 1
                && let Some(Owner::Table(t)) = self.owners.get(&object).copied()
            {
                self.db.tables[t].storage.compression.push(entry.clone());
            }
            if let Some(&(owner, slot)) = self.index_slots.get(&(object, index_id)) {
                self.indexes_of(owner)[slot].storage.compression.push(entry);
            }
        }
        let collapse = |list: &mut Vec<PartitionCompression>| {
            if list.len() > 1 && list.iter().all(|p| p.compression == list[0].compression) {
                let first = PartitionCompression { partition: 0, compression: list[0].compression.clone() };
                *list = if first.compression == "NONE" { Vec::new() } else { vec![first] };
            } else if list.len() == 1 {
                list[0].partition = 0;
            }
        };
        for t in &mut self.db.tables {
            collapse(&mut t.storage.compression);
            for ix in &mut t.indexes {
                collapse(&mut ix.storage.compression);
            }
        }
        for v in &mut self.db.views {
            for ix in &mut v.indexes {
                collapse(&mut ix.storage.compression);
            }
        }
    }

    fn column_name(&self, object: i64, colid: i64) -> String {
        self.cat.colpars.get(&(object as i32)).and_then(|p| p.iter().find(|c| c.int("colid") == colid)).map(|c| c.str("name")).unwrap_or_default()
    }

    fn foreign_keys(&mut self) {
        let mut fks: Vec<Obj> = self.objects.values().filter(|o| o.kind == "F").cloned().collect();
        fks.sort_by_key(|o| o.id);
        let action = |a: i64| match a {
            1 => "CASCADE",
            2 => "SET_NULL",
            3 => "SET_DEFAULT",
            _ => "NO_ACTION",
        };
        for o in fks {
            let Some(Owner::Table(t)) = self.owners.get(&o.parent).copied() else { continue };
            let Some(referenced) = self.single_ref(rc::FK_REFERENCED_INDEX, o.id as i64, 0).and_then(|r| self.name_of(r.int("indepid") as i32)) else { continue };
            let mut parent_cols: Vec<&SysRow> = self.single.get(&rc::FK_PARENT_COLUMN).into_iter().flatten().filter(|r| r.int("depid") == o.id as i64).collect();
            parent_cols.sort_by_key(|r| r.int("depsubid"));
            let mut columns = Vec::new();
            let mut referenced_columns = Vec::new();
            for p in parent_cols {
                columns.push(self.column_name(p.int("indepid"), p.int("indepsubid")));
                if let Some(rr) = self.single_ref(rc::FK_REFERENCED_COLUMN, o.id as i64, p.int("depsubid")) {
                    referenced_columns.push(self.column_name(rr.int("indepid"), rr.int("indepsubid")));
                }
            }
            self.db.tables[t].foreign_keys.push(ForeignKey {
                name: o.name.clone(),
                system_named: o.flag(st::SYSTEM_NAMED),
                columns,
                referenced_table: referenced,
                referenced_columns,
                on_delete: action((o.status >> 14) & 3).into(),
                on_update: action((o.status >> 12) & 3).into(),
                disabled: o.flag(st::DISABLED),
                not_trusted: o.flag(st::NOT_TRUSTED),
                not_for_replication: o.flag(st::NOT_FOR_REPLICATION),
            });
        }
    }

    fn check_constraints(&mut self) {
        let mut checks: Vec<Obj> = self.objects.values().filter(|o| o.kind == "C").cloned().collect();
        checks.sort_by_key(|o| (o.parent, o.name.clone()));
        for o in checks {
            let check = CheckConstraint {
                name: o.name.clone(),
                system_named: o.flag(st::SYSTEM_NAMED),
                definition: self.definition(o.id as i64, None).unwrap_or_default(),
                column: (o.intprop > 0).then(|| self.column_name(o.parent as i64, o.intprop)),
                disabled: o.flag(st::DISABLED),
                not_trusted: o.flag(st::NOT_TRUSTED),
                not_for_replication: o.flag(st::NOT_FOR_REPLICATION),
            };
            match self.owners.get(&o.parent).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].check_constraints.push(check),
                Some(Owner::TableType(t)) => self.db.types[t].check_constraints.push(check),
                _ => {}
            }
        }
    }

    fn execute_as(&self, id: i32) -> Option<String> {
        let p = self.single_ref(rc::EXECUTE_AS, id as i64, 0)?.int("indepid");
        if p == -2 { Some("OWNER".into()) } else { self.principal(p) }
    }

    fn modules(&mut self) {
        for o in self.sorted_objects(&["P", "PC", "FN", "IF", "TF", "FS", "FT", "AF", "V"]) {
            let definition = self.definition(o.id as i64, None);
            let encrypted = o.flag(st::ENCRYPTED) || (definition.is_none() && !matches!(o.kind.as_str(), "PC" | "FS" | "FT" | "AF"));
            let definition = if encrypted { None } else { definition };
            let settings = ModuleSettings { ansi_nulls: o.flag(st::ANSI_NULLS), quoted_identifier: o.flag(st::QUOTED_IDENTIFIER) };
            let schema_bound = o.flag(st::SCHEMA_BOUND);
            if o.kind == "V" {
                if let Some(Owner::View(vi)) = self.owners.get(&o.id).copied() {
                    let view = &mut self.db.views[vi];
                    view.definition = definition.unwrap_or_default();
                    view.encrypted = encrypted;
                    view.schema_bound = schema_bound;
                    view.settings = settings;
                }
                continue;
            }
            let kind = match o.kind.as_str() {
                "P" => RoutineKind::Procedure,
                "PC" => RoutineKind::ClrProcedure,
                "FN" => RoutineKind::ScalarFunction,
                "IF" => RoutineKind::InlineTableFunction,
                "TF" => RoutineKind::TableFunction,
                "FS" => RoutineKind::ClrScalarFunction,
                "FT" => RoutineKind::ClrTableFunction,
                _ => RoutineKind::ClrAggregate,
            };
            let native = matches!(o.kind.as_str(), "P" | "FN" | "IF") && o.flag(st::NATIVE);
            self.db.routines.push(Routine {
                schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(),
                name: o.name.clone(),
                kind,
                definition: definition.unwrap_or_default(),
                encrypted,
                schema_bound,
                settings,
                execute_as: self.execute_as(o.id),
                native_compilation: native,
                ..Default::default()
            });
        }
    }

    fn trigger_events(&self, id: i32) -> (Vec<String>, Vec<(String, String)>) {
        let mut events = Vec::new();
        let mut order = Vec::new();
        let mut rows: Vec<&SysRow> = self.values.get(&(vc::TRIGGER_EVENT, id as i64)).into_iter().flatten().filter(|r| r.int("valnum") != 100).collect();
        rows.sort_by_key(|r| r.int("subobjid"));
        for r in rows {
            let name = match r.int("subobjid") {
                1 => "INSERT",
                2 => "UPDATE",
                3 => "DELETE",
                _ => continue,
            };
            events.push(name.to_string());
            let v = r.int("value");
            if v & 1 != 0 {
                order.push((name.to_string(), "FIRST".to_string()));
            } else if v & 2 != 0 {
                order.push((name.to_string(), "LAST".to_string()));
            }
        }
        (events, order)
    }

    fn triggers(&mut self) {
        let mut trs: Vec<Obj> = self.objects.values().filter(|o| o.kind == "TR" && !o.flag(st::MS_SHIPPED)).cloned().collect();
        trs.sort_by_key(|o| (o.parent, o.name.clone()));
        for o in trs {
            let definition = self.definition(o.id as i64, None).filter(|_| !o.flag(st::ENCRYPTED));
            if o.parent == 0 {
                continue;
            }
            let (events, order) = self.trigger_events(o.id);
            let trigger = Trigger {
                name: o.name.clone(),
                encrypted: definition.is_none(),
                definition: definition.unwrap_or_default(),
                disabled: o.flag(st::TRIGGER_DISABLED),
                instead_of: o.flag(st::TRIGGER_INSTEAD_OF),
                not_for_replication: o.flag(st::NOT_FOR_REPLICATION),
                events,
                order,
                clr_method: None,
                execute_as: self.execute_as(o.id),
            };
            match self.owners.get(&o.parent).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].triggers.push(trigger),
                Some(Owner::View(v)) => self.db.views[v].triggers.push(trigger),
                _ => {}
            }
        }
    }

    fn sequences(&mut self) {
        for o in self.sorted_objects(&["SO"]) {
            let Some(v) = self.value(vc::SEQUENCE, o.id as i64, 0, 0).or_else(|| self.values.get(&(vc::SEQUENCE, o.id as i64)).and_then(|v| v.first())) else { continue };
            let valnum = v.int("valnum");
            let (xtype, prec) = ((valnum & 0xff) as u8, ((valnum >> 8) & 0xff) as u8);
            let blob = variant_binary(v, "value");
            let values = sequence_values(&blob, xtype);
            let data_type = match self.single_ref(rc::OBJECT_TO_TYPE, o.id as i64, 0).map(|r| r.int("indepid")) {
                Some(u) if u > 256 => self.data_type(u, xtype as i64, 0, prec as i64, 0),
                _ => crate::catalog::read::system_type(type_name(xtype), 0, prec as i32, 0),
            };
            let get = |i: usize| values.get(i).cloned().unwrap_or_default();
            self.db.sequences.push(Sequence {
                schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(),
                name: o.name.clone(),
                data_type,
                start: get(0),
                increment: get(1),
                minimum: get(2),
                maximum: get(3),
                cycle: o.flag(st::SEQ_CYCLE),
                cached: o.flag(st::SEQ_CACHED),
                cache_size: if o.flag(st::SEQ_CACHED) && o.intprop != 0 { Some(o.intprop) } else { None },
            });
        }
    }

    fn synonyms(&mut self) {
        for o in self.sorted_objects(&["SN"]) {
            let base = self.value(vc::SYNONYM_BASE, o.id as i64, 0, 0).and_then(|r| text_of(r.get("value"))).unwrap_or_default();
            self.db.synonyms.push(Synonym { schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(), name: o.name.clone(), base_object: base });
        }
    }

    fn bindables(&mut self) {
        for o in self.sorted_objects(&["D", "R"]) {
            if o.kind == "D" && o.parent != 0 {
                continue;
            }
            let object = BindableObject {
                schema: self.schema_names.get(&o.schema).cloned().unwrap_or_default(),
                name: o.name.clone(),
                definition: self.definition(o.id as i64, None).unwrap_or_default(),
            };
            if o.kind == "R" { self.db.rules.push(object) } else { self.db.defaults.push(object) }
        }
    }

    fn statistics(&mut self) {
        let mut rows = self.cat.system("sysidxstats");
        rows.sort_by_key(|r| (r.int("id"), r.str("name")));
        let index_names: std::collections::HashSet<(i64, String)> =
            rows.iter().filter(|r| r.int("status") & 1 != 0).map(|r| (r.int("id"), r.str("name"))).collect();
        let mut slots = HashMap::new();
        for r in &rows {
            let status = r.int("status");
            // Statistics that are not auto created and not the index's own.
            if status & 2 == 0 || status & 0x2001 != 0 || index_names.contains(&(r.int("id"), r.str("name"))) {
                continue;
            }
            let object = r.int("id") as i32;
            let filter = (status & 0x20000 != 0).then(|| self.value(vc::STATS_FILTER, object as i64, r.int("indid"), 0).map(stored_text)).flatten();
            let stat = Statistic { name: r.str("name"), filter, no_recompute: status & 0x4000 != 0, incremental: status & 0x800000 != 0, columns: Vec::new() };
            let list = match self.owners.get(&object).copied() {
                Some(Owner::Table(t)) => &mut self.db.tables[t].statistics,
                Some(Owner::View(x)) => &mut self.db.views[x].statistics,
                _ => continue,
            };
            list.push(stat);
            slots.insert((object, r.int("indid") as i32), list.len() - 1);
        }
        if slots.is_empty() {
            return;
        }
        let mut cols = self.cat.system("sysiscols");
        cols.sort_by_key(|r| (r.int("idmajor"), r.int("idminor"), r.int("subid")));
        for r in cols {
            let key = (r.int("idmajor") as i32, r.int("idminor") as i32);
            let Some(&slot) = slots.get(&key) else { continue };
            if r.int("status") & 1 == 0 {
                continue;
            }
            let name = self.column_name(key.0 as i64, r.int("intprop"));
            let list = match self.owners.get(&key.0).copied() {
                Some(Owner::Table(t)) => &mut self.db.tables[t].statistics,
                Some(Owner::View(x)) => &mut self.db.views[x].statistics,
                _ => continue,
            };
            list[slot].columns.push(name);
        }
    }

    fn parent_of(&self, id: i32) -> Option<&Obj> {
        let o = self.objects.get(&id)?;
        (o.parent != 0).then(|| self.objects.get(&o.parent)).flatten()
    }

    fn extended_properties(&mut self) {
        let mut rows = self.xprops.clone();
        rows.sort_by_key(|r| (r.int("class"), r.int("id"), r.int("subid"), r.str("name")));
        for r in rows {
            let (class, major, minor) = (r.int("class"), r.int("id") as i32, r.int("subid"));
            let value = match r.get("value") {
                Value::Variant(v) => Some(v.text.clone()),
                Value::Null => None,
                v => text_of(v),
            };
            let mut p = ExtendedProperty { name: r.str("name"), value, ..Default::default() };
            match class {
                0 => p.class = "DATABASE".into(),
                3 => {
                    p.class = "SCHEMA".into();
                    p.schema = self.schema_names.get(&major).cloned();
                }
                6 => {
                    let Some(t) = self.types.get(&(major as i64)) else { continue };
                    p.class = "TYPE".into();
                    p.schema = self.schema_names.get(&(t.int("schid") as i32)).cloned();
                    p.object = Some(t.str("name"));
                }
                1 | 2 | 7 => {
                    let Some(o) = self.objects.get(&major).cloned() else { continue };
                    if o.kind == "S" || o.kind == "IT" || o.flag(st::MS_SHIPPED) {
                        continue;
                    }
                    let parent = if matches!(o.kind.as_str(), "PK" | "UQ" | "F" | "C" | "D" | "TR" | "TA") { self.parent_of(major).cloned() } else { None };
                    if let Some(parent) = parent {
                        p.class = level1_class(&parent.kind).into();
                        p.schema = self.schema_names.get(&parent.schema).cloned();
                        p.object = Some(parent.name);
                        p.minor_type = Some(if o.kind.starts_with('T') { "TRIGGER" } else { "CONSTRAINT" }.into());
                        p.minor_name = Some(o.name);
                    } else {
                        p.class = level1_class(&o.kind).into();
                        p.schema = self.schema_names.get(&o.schema).cloned();
                        p.object = Some(o.name.clone());
                        match class {
                            1 if minor > 0 => {
                                p.minor_type = Some("COLUMN".into());
                                p.minor_name = Some(self.column_name(major as i64, minor));
                            }
                            2 => {
                                p.minor_type = Some("PARAMETER".into());
                                p.minor_name = self
                                    .cat
                                    .parameters
                                    .get(&major)
                                    .and_then(|v| v.iter().find(|c| c.int("colid") == minor))
                                    .map(|c| c.str("name"));
                            }
                            7 => {
                                p.minor_type = Some("INDEX".into());
                                p.minor_name = self.cat.system("sysidxstats").into_iter().find(|x| x.int("id") == major as i64 && x.int("indid") == minor).map(|x| x.str("name"));
                            }
                            _ => {}
                        }
                    }
                }
                _ => continue,
            }
            self.db.extended_properties.push(p);
        }
    }

    fn principals_and_roles(&mut self) {
        let mut rows = self.cat.system("sysowners");
        rows.sort_by_key(|r| self.key(&r.str("name")));
        let certs = self.cat.system("syscerts");
        let asym = self.cat.system("sysasymkeys");
        for r in &rows {
            let id = r.int("id");
            let name = r.str("name");
            let ty = r.str("type").trim().to_string();
            if id >= 16384 || matches!(name.as_str(), "public" | "dbo" | "guest" | "sys" | "INFORMATION_SCHEMA") {
                continue;
            }
            let kind = match ty.as_str() {
                "S" => "SQL_USER",
                "U" => "WINDOWS_USER",
                "G" => "WINDOWS_GROUP",
                "E" => "EXTERNAL_USER",
                "X" => "EXTERNAL_GROUP",
                "C" => "CERTIFICATE_MAPPED_USER",
                "K" => "ASYMMETRIC_KEY_MAPPED_USER",
                "R" => "DATABASE_ROLE",
                "A" => "APPLICATION_ROLE",
                _ => continue,
            };
            let sid = r.bytes("sid");
            let without_login = ty == "S" && matches!(r.get("password"), Value::Null) && sid.len() >= 9 && sid[..9] == [1, 5, 0, 0, 0, 0, 0, 9, 3];
            let role = kind.ends_with("_ROLE");
            let mapped = matches!(ty.as_str(), "C" | "K");
            let login = if role || without_login || mapped {
                None
            } else if sid == [1] {
                Some("sa".to_string())
            } else {
                Some(name.clone())
            };
            let owner = if role { self.single_ref(rc::ROLE_OWNER, id, 0).and_then(|o| self.principal(o.int("indepid"))) } else { None };
            let by_sid = |rows: &[SysRow]| rows.iter().find(|c| c.bytes("sid") == sid && !sid.is_empty()).map(|c| c.str("name"));
            self.db.principals.push(Principal {
                name,
                login,
                default_schema: if role && kind != "APPLICATION_ROLE" { None } else { Some(r.str("dfltsch")).filter(|s| !s.is_empty()) },
                owner,
                kind: kind.into(),
                member_of: Vec::new(),
                certificate: if ty == "C" { by_sid(&certs) } else { None },
                asymmetric_key: if ty == "K" { by_sid(&asym) } else { None },
            });
        }
        let mut members: Vec<(String, String)> = self
            .multi
            .get(&rc::ROLE_MEMBER)
            .into_iter()
            .flatten()
            .filter(|r| r.int("depsubid") == 0 && r.int("indepsubid") == 0)
            .filter_map(|r| Some((self.principal(r.int("depid"))?, self.principal(r.int("indepid"))?)))
            .collect();
        members.sort_by_key(|(m, r)| (self.key(m), self.key(r)));
        for (member, role) in members {
            if let Some(p) = self.db.principals.iter_mut().find(|p| p.name == member) {
                p.member_of.push(role);
            }
        }
    }

    fn permissions(&mut self) {
        let mut rows = self.cat.system("sysprivs");
        let grantee_name = |b: &Builder, r: &SysRow| b.principal(r.int("grantee")).unwrap_or_default();
        rows.sort_by_key(|r| (r.int("class"), r.int("id"), self.key(&grantee_name(self, r)), permission_name(r.str("type").trim()), r.int("subid")));
        for r in rows {
            let class = r.int("class");
            let state = r.str("state");
            let (major, minor) = (r.int("id"), r.int("subid"));
            if (state == "R" && minor == 0) || class >= 100 {
                continue;
            }
            if !(minor == 0 || class == 1) {
                continue;
            }
            let grantee = grantee_name(self, &r);
            if grantee.is_empty() || matches!(grantee.as_str(), "dbo" | "sys" | "INFORMATION_SCHEMA") {
                continue;
            }
            let code = r.str("type").trim().to_string();
            if class == 0 && code == "CO" {
                continue;
            }
            let mut p = Permission {
                grantee,
                permission: permission_name(&code),
                state: match state.as_str() {
                    "G" => "GRANT",
                    "D" => "DENY",
                    "W" => "GRANT_WITH_GRANT_OPTION",
                    _ => "REVOKE",
                }
                .into(),
                column: (class == 1 && minor > 0).then(|| self.column_name(major, minor)),
                ..Default::default()
            };
            match class {
                0 => p.class = "DATABASE".into(),
                1 => {
                    let Some(o) = self.objects.get(&(major as i32)) else { continue };
                    if o.kind == "S" || o.kind == "IT" || major < 0 {
                        continue;
                    }
                    p.class = "OBJECT".into();
                    p.schema = self.schema_names.get(&o.schema).cloned();
                    p.object = Some(o.name.clone());
                }
                3 => {
                    let Some(schema) = self.schema_names.get(&(major as i32)).cloned() else { continue };
                    p.class = "SCHEMA".into();
                    p.schema = Some(schema);
                }
                6 => {
                    let Some(t) = self.types.get(&major) else { continue };
                    p.class = "TYPE".into();
                    p.schema = self.schema_names.get(&(t.int("schid") as i32)).cloned();
                    p.object = Some(t.str("name"));
                }
                _ => continue,
            }
            self.db.permissions.push(p);
        }
    }

    fn dependencies(&mut self) {
        let mut deps: BTreeMap<(ObjectName, ObjectName), bool> = BTreeMap::new();
        for class in [0i64, 1] {
            for r in self.multi.get(&class).into_iter().flatten() {
                let (referencing, referenced) = (r.int("depid") as i32, r.int("indepid") as i32);
                if referencing == referenced {
                    continue;
                }
                let from_id = match self.objects.get(&referencing) {
                    Some(o) if matches!(o.kind.as_str(), "C" | "D" | "PK" | "UQ" | "F") && o.parent != 0 => o.parent,
                    _ => referencing,
                };
                let (Some(from), Some(to)) = (self.name_of(from_id), self.name_of(referenced)) else { continue };
                if from == to || from.schema == "sys" || to.schema == "sys" {
                    continue;
                }
                let entry = deps.entry((from, to)).or_insert(false);
                *entry |= class == 1;
            }
        }
        for syn in &self.db.synonyms {
            if let Some(target) = crate::catalog::read::parse_synonym_target(&syn.base_object, &self.db.name) {
                deps.entry((ObjectName::new(&syn.schema, &syn.name), target)).or_insert(false);
            }
        }
        for t in &self.db.tables {
            let from = t.object_name();
            for fk in &t.foreign_keys {
                if fk.referenced_table != from {
                    deps.entry((from.clone(), fk.referenced_table.clone())).or_insert(false);
                }
            }
            for c in t.columns.iter().filter_map(|c| c.data_type.schema.as_ref().map(|s| (s, &c.data_type.name))) {
                deps.insert((from.clone(), ObjectName::new(c.0, c.1)), true);
            }
        }
        self.db.dependencies = deps.into_iter().map(|((from, to), strong)| Dependency { from, to, strong }).collect();
    }

    fn finish(mut self) -> Database {
        let db = &mut self.db;
        for t in &mut db.tables {
            t.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            t.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
            t.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
            t.triggers.sort_by(|a, b| a.name.cmp(&b.name));
            if let Some(temporal) = t.temporal.as_mut() {
                let period = |kind: &str| t.columns.iter().find(|c| c.generated_always.as_deref() == Some(kind)).map(|c| c.name.clone());
                temporal.period_start = period("ROW START").unwrap_or_default();
                temporal.period_end = period("ROW END").unwrap_or_default();
            }
            let lob = t.columns.iter().any(|c| {
                let d = &c.data_type;
                matches!(d.name.as_str(), "text" | "ntext" | "image" | "xml")
                    || (matches!(d.name.as_str(), "varchar" | "nvarchar" | "varbinary") && d.length == Some(-1))
                    || d.schema.is_some()
            });
            if !lob {
                t.text_filegroup = None;
            }
        }
        for v in &mut db.views {
            v.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            v.triggers.sort_by(|a, b| a.name.cmp(&b.name));
        }
        for ty in &mut db.types {
            ty.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            ty.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }
        db.routines.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        self.db
    }
}

fn level1_class(kind: &str) -> &'static str {
    match kind {
        "U" => "TABLE",
        "V" => "VIEW",
        "P" | "PC" => "PROCEDURE",
        "FN" | "IF" | "TF" | "FS" | "FT" | "AF" => "FUNCTION",
        "SO" => "SEQUENCE",
        "SN" => "SYNONYM",
        _ => "OBJECT",
    }
}

/// The bytes of a binary sql_variant column (its raw form past the variant header).
fn variant_binary(r: &SysRow, column: &str) -> Vec<u8> {
    match r.get(column) {
        Value::Bytes(b) => b.clone(),
        Value::Variant(_) => {
            let raw = r.bytes(&format!("{column}_raw"));
            if matches!(raw.first(), Some(165 | 173)) { raw.get(4..).unwrap_or_default().to_vec() } else { raw }
        }
        _ => Vec::new(),
    }
}

/// Text kept in sysobjvalues.imageval: one byte per character when `value` has bit 2, else UTF-16.
fn stored_text(r: &SysRow) -> String {
    let b = r.bytes("imageval");
    if r.int("value") & 2 != 0 { b.iter().map(|&c| c as char).collect() } else { values::utf16(&b) }
}

/// A number of a type, stored little-endian; decimals as precision, scale, sign and magnitude.
fn number_at(b: &[u8], xtype: u8) -> (Option<String>, usize) {
    let width = match xtype {
        48 => 1,
        52 => 2,
        56 => 4,
        127 => 8,
        106 | 108 => 20,
        _ => 8,
    };
    let Some(v) = b.get(..width) else { return (None, width) };
    let text = match xtype {
        48 => v[0].to_string(),
        52 => i16::from_le_bytes([v[0], v[1]]).to_string(),
        56 => i32::from_le_bytes(v[..4].try_into().unwrap()).to_string(),
        106 | 108 => {
            let mag = v[4..20].iter().rev().fold(0u128, |a, &x| (a << 8) | x as u128);
            let scale = v[2] as u32;
            let sign = if v[3] == 1 { "" } else { "-" };
            if scale == 0 {
                format!("{sign}{mag}")
            } else {
                let p = 10u128.pow(scale);
                format!("{sign}{}.{:0w$}", mag / p, mag % p, w = scale as usize)
            }
        }
        _ => i64::from_le_bytes(v[..8].try_into().unwrap()).to_string(),
    };
    (Some(text), width)
}

/// Seed and increment from syscolpars.idtval: the last value, the increment, then the seed.
fn identity_values(b: &[u8], xtype: u8) -> (String, String) {
    let (_, w) = number_at(b, xtype);
    let (inc, _) = number_at(b.get(w..).unwrap_or_default(), xtype);
    let (seed, _) = number_at(b.get(2 * w..).unwrap_or_default(), xtype);
    (seed.unwrap_or_else(|| "1".into()), inc.unwrap_or_else(|| "1".into()))
}

/// Start, increment, minimum, maximum (and current value) of a sequence.
fn sequence_values(b: &[u8], xtype: u8) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    for _ in 0..4 {
        let (v, w) = number_at(b.get(at..).unwrap_or_default(), xtype);
        out.push(v.unwrap_or_default());
        at += w;
    }
    out
}

/// sys.database_permissions.permission_name from the stored code.
fn permission_name(code: &str) -> String {
    PERMISSIONS.iter().find(|p| p.0 == code).map(|p| p.1.to_string()).unwrap_or_else(|| code.to_string())
}

/// (code, name) of database-scope permissions, from sys.fn_builtin_permissions.
const PERMISSIONS: &[(&str, &str)] = &[
    ("AADS", "ALTER ANY DATABASE EVENT SESSION"),
    ("AAMK", "ALTER ANY MASK"),
    ("AASC", "ALTER ANY SENSITIVITY CLASSIFICATION"),
    ("AEDS", "ALTER ANY EXTERNAL DATA SOURCE"),
    ("AEFF", "ALTER ANY EXTERNAL FILE FORMAT"),
    ("AESJ", "ALTER ANY EXTERNAL JOB"),
    ("AEST", "ALTER ANY EXTERNAL STREAM"),
    ("AL", "ALTER"),
    ("ALAK", "ALTER ANY ASYMMETRIC KEY"),
    ("ALAR", "ALTER ANY APPLICATION ROLE"),
    ("ALAS", "ALTER ANY ASSEMBLY"),
    ("ALC", "ALTER LEDGER CONFIGURATION"),
    ("ALCF", "ALTER ANY CERTIFICATE"),
    ("ALCK", "ALTER ANY COLUMN ENCRYPTION KEY"),
    ("ALCM", "ALTER ANY COLUMN MASTER KEY"),
    ("ALDA", "ALTER ANY DATABASE AUDIT"),
    ("ALDC", "ALTER ANY DATABASE SCOPED CONFIGURATION"),
    ("ALDS", "ALTER ANY DATASPACE"),
    ("ALED", "ALTER ANY DATABASE EVENT NOTIFICATION"),
    ("ALEL", "ALTER ANY EXTERNAL LIBRARY"),
    ("ALFT", "ALTER ANY FULLTEXT CATALOG"),
    ("ALLA", "ALTER ANY EXTERNAL LANGUAGE"),
    ("ALMT", "ALTER ANY MESSAGE TYPE"),
    ("ALR", "ALTER LEDGER"),
    ("ALRL", "ALTER ANY ROLE"),
    ("ALRT", "ALTER ANY ROUTE"),
    ("ALSB", "ALTER ANY REMOTE SERVICE BINDING"),
    ("ALSC", "ALTER ANY CONTRACT"),
    ("ALSK", "ALTER ANY SYMMETRIC KEY"),
    ("ALSM", "ALTER ANY SCHEMA"),
    ("ALSP", "ALTER ANY SECURITY POLICY"),
    ("ALSV", "ALTER ANY SERVICE"),
    ("ALTG", "ALTER ANY DATABASE DDL TRIGGER"),
    ("ALUS", "ALTER ANY USER"),
    ("AUTH", "AUTHENTICATE"),
    ("BADB", "BACKUP DATABASE"),
    ("BALO", "BACKUP LOG"),
    ("CL", "CONTROL"),
    ("CO", "CONNECT"),
    ("CORP", "CONNECT REPLICATION"),
    ("CP", "CHECKPOINT"),
    ("CRAG", "CREATE AGGREGATE"),
    ("CRAK", "CREATE ASYMMETRIC KEY"),
    ("CRAS", "CREATE ASSEMBLY"),
    ("CRCF", "CREATE CERTIFICATE"),
    ("CRDB", "CREATE DATABASE"),
    ("CRDF", "CREATE DEFAULT"),
    ("CRDS", "CREATE ANY DATABASE EVENT SESSION"),
    ("CRED", "CREATE DATABASE DDL EVENT NOTIFICATION"),
    ("CREL", "CREATE EXTERNAL LIBRARY"),
    ("CRFN", "CREATE FUNCTION"),
    ("CRFT", "CREATE FULLTEXT CATALOG"),
    ("CRLA", "CREATE EXTERNAL LANGUAGE"),
    ("CRMT", "CREATE MESSAGE TYPE"),
    ("CRPR", "CREATE PROCEDURE"),
    ("CRQU", "CREATE QUEUE"),
    ("CRRL", "CREATE ROLE"),
    ("CRRT", "CREATE ROUTE"),
    ("CRRU", "CREATE RULE"),
    ("CRSB", "CREATE REMOTE SERVICE BINDING"),
    ("CRSC", "CREATE CONTRACT"),
    ("CRSK", "CREATE SYMMETRIC KEY"),
    ("CRSM", "CREATE SCHEMA"),
    ("CRSN", "CREATE SYNONYM"),
    ("CRSO", "CREATE SEQUENCE"),
    ("CRSV", "CREATE SERVICE"),
    ("CRTB", "CREATE TABLE"),
    ("CRTY", "CREATE TYPE"),
    ("CRVW", "CREATE VIEW"),
    ("CRXS", "CREATE XML SCHEMA COLLECTION"),
    ("CUSR", "CREATE USER"),
    ("DABO", "ADMINISTER DATABASE BULK OPERATIONS"),
    ("DDES", "ALTER ANY DATABASE EVENT SESSION DISABLE"),
    ("DL", "DELETE"),
    ("DRDS", "DROP ANY DATABASE EVENT SESSION"),
    ("EAEE", "EXECUTE ANY EXTERNAL ENDPOINT"),
    ("EAES", "EXECUTE ANY EXTERNAL SCRIPT"),
    ("EDES", "ALTER ANY DATABASE EVENT SESSION ENABLE"),
    ("EL", "ENABLE LEDGER"),
    ("EX", "EXECUTE"),
    ("EXES", "EXECUTE EXTERNAL SCRIPT"),
    ("IM", "IMPERSONATE"),
    ("IN", "INSERT"),
    ("KIDC", "KILL DATABASE CONNECTION"),
    ("LDAE", "ALTER ANY DATABASE EVENT SESSION ADD EVENT"),
    ("LDAT", "ALTER ANY DATABASE EVENT SESSION ADD TARGET"),
    ("LDDE", "ALTER ANY DATABASE EVENT SESSION DROP EVENT"),
    ("LDDT", "ALTER ANY DATABASE EVENT SESSION DROP TARGET"),
    ("LDSO", "ALTER ANY DATABASE EVENT SESSION OPTION"),
    ("RC", "RECEIVE"),
    ("RF", "REFERENCES"),
    ("SL", "SELECT"),
    ("SN", "SEND"),
    ("SPLN", "SHOWPLAN"),
    ("SUQN", "SUBSCRIBE QUERY NOTIFICATIONS"),
    ("TO", "TAKE OWNERSHIP"),
    ("UMSK", "UNMASK"),
    ("UP", "UPDATE"),
    ("VASC", "VIEW ANY SENSITIVITY CLASSIFICATION"),
    ("VCD", "VIEW CRYPTOGRAPHICALLY SECURED DEFINITION"),
    ("VDP", "VIEW DATABASE PERFORMANCE STATE"),
    ("VDS", "VIEW DATABASE SECURITY STATE"),
    ("VDSA", "VIEW DATABASE SECURITY AUDIT"),
    ("VLC", "VIEW LEDGER CONTENT"),
    ("VW", "VIEW DEFINITION"),
    ("VWCK", "VIEW ANY COLUMN ENCRYPTION KEY DEFINITION"),
    ("VWCM", "VIEW ANY COLUMN MASTER KEY DEFINITION"),
    ("VWCT", "VIEW CHANGE TRACKING"),
    ("VWDS", "VIEW DATABASE STATE"),
    ("VWP", "VIEW PERFORMANCE DEFINITION"),
    ("VWS", "VIEW SECURITY DEFINITION"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_numbers() {
        assert_eq!(identity_values(&[7, 0, 0, 0, 3, 0, 0, 0, 5, 0, 0, 0, 0], 56), ("5".into(), "3".into()));
        let decimal = |v: u8| {
            let mut d = vec![5, 12, 0, 1, v];
            d.resize(20, 0);
            d
        };
        let dec = [decimal(130), decimal(10), decimal(100)].concat();
        assert_eq!(identity_values(&dec, 106), ("100".into(), "10".into()));
        assert_eq!(sequence_values(&[0, 1, 0, 255, 0, 1], 48), vec!["0", "1", "0", "255"]);
        assert_eq!(permission_name("SL"), "SELECT");
        assert_eq!(major_version(957), 16);
    }
}
