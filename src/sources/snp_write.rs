//! Writes SQL Compare snapshot files (`.snp`, format version 23): the mirror of `snp::read`.
//! Members the model has no place for get the empty or system values SQL Compare writes for a
//! database that does not use them; object ids are assigned here, in file order.

use std::collections::{HashMap, HashSet};

use crate::catalog::*;
use crate::db::quote;
use crate::schema::ddl::module_sql;
use crate::schema::text::{Tok, tokenize};

mod objects;

const MAGIC: &str = "SQLCOMPARE";
const VERSION: f32 = 23.0;
/// Version of keyed collections and permission lists.
const COLL: f32 = 4.0;
/// Version of the relational index tail.
const INDEX_TAIL: f32 = 11.0;
/// Version of the XML index tail, its paths and namespaces.
const XML_TAIL: f32 = 11.1;
const BLOBS: f32 = 7.0;
/// First object id handed out.
const FIRST_ID: i32 = 1_000_000;
/// Blob id (-1) of the text SQL Compare keeps for modules with no syscomments row.
const MISSING_ID: u64 = u64::MAX;
const MISSING_TEXT: &str = "-- Missing syscomments entry. Please check your database";
const ENCRYPTED_TEXT: &str = "-- Text was encrypted";

const GRANT: u8 = 205;
const FIXED_ROLES: [&str; 9] = [
    "db_owner",
    "db_accessadmin",
    "db_securityadmin",
    "db_ddladmin",
    "db_backupoperator",
    "db_datareader",
    "db_datawriter",
    "db_denydatareader",
    "db_denydatawriter",
];

/// Database permissions every database grants to public.
const PUBLIC_DEFAULTS: [&str; 2] = ["VIEW ANY COLUMN ENCRYPTION KEY DEFINITION", "VIEW ANY COLUMN MASTER KEY DEFINITION"];

// ObjectType codes.
const OT_TABLE: u8 = 1;
const OT_PROCEDURE: u8 = 2;
const OT_VIEW: u8 = 3;
const OT_FUNCTION: u8 = 6;
const OT_TYPE: u8 = 10;
const OT_TRIGGER: u8 = 11;
const OT_SYNONYM: u8 = 14;
const OT_MESSAGE_TYPE: u8 = 16;
const OT_CONTRACT: u8 = 17;
const OT_CHECK: u8 = 31;
const OT_SEQUENCE: u8 = 35;

const SB: &str = "http://schemas.microsoft.com/SQL/ServiceBroker/";
const SN: &str = "http://schemas.microsoft.com/SQL/Notifications/";
/// (prefix, name) of a system name.
type SysName = (&'static str, &'static str);
/// Service Broker message types every database has: (prefix, name, validation, contract).
const MESSAGE_TYPES: [(&str, &str, i32, Option<SysName>); 14] = [
    (SB, "Error", 1, None),
    (SB, "EndDialog", 2, None),
    (SN, "QueryNotification", 1, Some((SN, "PostQueryNotification"))),
    (SN, "EventNotification", 1, Some((SN, "PostEventNotification"))),
    (SB, "DialogTimer", 2, None),
    (SB, "BrokerConfigurationNotice/MissingRoute", 1, Some((SB, "BrokerConfigurationNotice"))),
    (SB, "BrokerConfigurationNotice/FailedRoute", 1, Some((SB, "BrokerConfigurationNotice"))),
    (SB, "BrokerConfigurationNotice/MissingRemoteServiceBinding", 1, Some((SB, "BrokerConfigurationNotice"))),
    (SB, "BrokerConfigurationNotice/FailedRemoteServiceBinding", 1, Some((SB, "BrokerConfigurationNotice"))),
    (SB, "ServiceEcho/Echo", 0, Some((SB, "ServiceEcho"))),
    (SB, "ServiceDiagnostic/Query", 1, Some((SB, "ServiceDiagnostic"))),
    (SB, "ServiceDiagnostic/Status", 1, Some((SB, "ServiceDiagnostic"))),
    (SB, "ServiceDiagnostic/Description", 0, Some((SB, "ServiceDiagnostic"))),
    ("", "DEFAULT", 0, Some(("", "DEFAULT"))),
];
/// System contracts: (prefix, name, id, message types used as (prefix, name, by initiator, by target)).
type Usage = (&'static str, &'static str, bool, bool);
const CONTRACTS: [(&str, &str, i32, &[Usage]); 6] = [
    ("", "DEFAULT", 6, &[("", "DEFAULT", true, true)]),
    (SN, "PostEventNotification", 2, &[(SN, "EventNotification", true, false)]),
    (SN, "PostQueryNotification", 1, &[(SN, "QueryNotification", true, false)]),
    (
        SB,
        "BrokerConfigurationNotice",
        3,
        &[
            (SB, "BrokerConfigurationNotice/MissingRoute", true, false),
            (SB, "BrokerConfigurationNotice/FailedRoute", true, false),
            (SB, "BrokerConfigurationNotice/MissingRemoteServiceBinding", true, false),
            (SB, "BrokerConfigurationNotice/FailedRemoteServiceBinding", true, false),
        ],
    ),
    (
        SB,
        "ServiceDiagnostic",
        5,
        &[(SB, "ServiceDiagnostic/Query", true, false), (SB, "ServiceDiagnostic/Status", false, true), (SB, "ServiceDiagnostic/Description", false, true)],
    ),
    (SB, "ServiceEcho", 4, &[(SB, "ServiceEcho/Echo", true, true)]),
];
/// System queues (dbo) with the object ids they have in every database.
const QUEUES: [(&str, i32); 3] =
    [("QueryNotificationErrorsQueue", 1_977_058_079), ("EventNotificationErrorsQueue", 2_009_058_193), ("ServiceBrokerQueue", 2_041_058_307)];

/// Writes the model as a version 23 snapshot.
pub fn write(db: &Database) -> Vec<u8> {
    let mut w = W::new(db);
    w.header();
    w.body();
    w.b.b
}

// ------------------------------------------------------------------------------- primitives

/// Little-endian .NET `BinaryWriter` values.
#[derive(Default)]
struct Buf {
    b: Vec<u8>,
}

impl Buf {
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }

    fn bool(&mut self, v: bool) {
        self.b.push(u8::from(v));
    }

    fn i8(&mut self, v: i8) {
        self.b.extend(v.to_le_bytes());
    }

    fn i16(&mut self, v: i16) {
        self.b.extend(v.to_le_bytes());
    }

    fn i32(&mut self, v: i32) {
        self.b.extend(v.to_le_bytes());
    }

    fn i64(&mut self, v: i64) {
        self.b.extend(v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.b.extend(v.to_le_bytes());
    }

    fn f32(&mut self, v: f32) {
        self.b.extend(v.to_le_bytes());
    }

    fn f64(&mut self, v: f64) {
        self.b.extend(v.to_le_bytes());
    }

    /// A count or length as i32.
    fn len(&mut self, n: usize) {
        self.i32(i32::try_from(n).unwrap_or(i32::MAX));
    }

    /// .NET 7-bit encoded length.
    fn len7(&mut self, mut n: usize) {
        while n >= 0x80 {
            self.b.push((n & 0x7F) as u8 | 0x80);
            n >>= 7;
        }
        self.b.push(n as u8);
    }

    /// Nullable string: 0 = null, 1 then the length-prefixed UTF-8 bytes.
    fn str(&mut self, v: Option<&str>) {
        match v {
            None => self.b.push(0),
            Some(s) => {
                self.b.push(1);
                self.len7(s.len());
                self.b.extend(s.as_bytes());
            }
        }
    }

    fn s(&mut self, v: &str) {
        self.str(Some(v));
    }

    fn strs<S: AsRef<str>>(&mut self, list: &[S]) {
        self.len(list.len());
        for s in list {
            self.s(s.as_ref());
        }
    }

    /// Keyed collection header: format version, case-sensitivity, count.
    fn coll_head(&mut self, cs: bool, n: usize) {
        self.f32(COLL);
        self.bool(cs);
        self.len(n);
    }

    /// Null script sources.
    fn no_ss(&mut self, n: usize) {
        for _ in 0..n {
            self.u8(0);
        }
    }
}

// ------------------------------------------------------------------------------- writer

/// What a written object needs from the rest of the model.
struct W<'a> {
    db: &'a Database,
    b: Buf,
    cs: bool,
    next_id: i32,
    /// Comments blob store: (id, text), in the order written.
    texts: Vec<(u64, String)>,
    encrypted: Vec<u64>,
    /// Object kinds by name, for dependencies and references.
    kinds: HashMap<ObjectName, u8>,
    /// DML triggers by (schema, name) → their parent.
    trigger_parents: HashMap<ObjectName, ObjectName>,
    early: objects::Early,
}

impl std::ops::Deref for W<'_> {
    type Target = Buf;
    fn deref(&self) -> &Buf {
        &self.b
    }
}

impl std::ops::DerefMut for W<'_> {
    fn deref_mut(&mut self) -> &mut Buf {
        &mut self.b
    }
}

/// A Table record's contents: a table, or the table behind a table type.
struct TableRec<'a> {
    t: &'a Table,
    object_id: i32,
    /// Table types: no filegroups.
    type_table: bool,
}

impl<'a> W<'a> {
    fn new(db: &'a Database) -> Self {
        let mut kinds = HashMap::new();
        let mut trigger_parents = HashMap::new();
        for t in &db.tables {
            kinds.insert(t.object_name(), OT_TABLE);
            for tr in &t.triggers {
                trigger_parents.insert(ObjectName::new(&t.schema, &tr.name), t.object_name());
            }
        }
        for v in &db.views {
            kinds.insert(ObjectName::new(&v.schema, &v.name), OT_VIEW);
            for tr in &v.triggers {
                trigger_parents.insert(ObjectName::new(&v.schema, &tr.name), ObjectName::new(&v.schema, &v.name));
            }
        }
        for r in &db.routines {
            kinds.insert(ObjectName::new(&r.schema, &r.name), if is_procedure(r) { OT_PROCEDURE } else { OT_FUNCTION });
        }
        for t in &db.types {
            kinds.insert(ObjectName::new(&t.schema, &t.name), OT_TYPE);
        }
        for s in &db.sequences {
            kinds.insert(ObjectName::new(&s.schema, &s.name), OT_SEQUENCE);
        }
        for s in &db.synonyms {
            kinds.insert(ObjectName::new(&s.schema, &s.name), OT_SYNONYM);
        }
        for name in trigger_parents.keys() {
            kinds.entry(name.clone()).or_insert(OT_TRIGGER);
        }
        W { db, b: Buf::default(), cs: db.case_sensitive(), next_id: FIRST_ID, texts: Vec::new(), encrypted: Vec::new(), kinds, trigger_parents, early: objects::Early::default() }
    }

    fn id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Stores module or default text under an object id.
    fn text(&mut self, id: i32, text: String, encrypted: bool) {
        let key = id as i64 as u64;
        if encrypted {
            self.encrypted.push(key);
            self.texts.push((key, ENCRYPTED_TEXT.into()));
        } else {
            self.texts.push((key, text));
        }
    }

    fn coll<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        let cs = self.cs;
        self.coll_head(cs, items.len());
        for x in items {
            f(self, x);
        }
    }

    fn empty_coll(&mut self) {
        let cs = self.cs;
        self.coll_head(cs, 0);
    }

    fn header(&mut self) {
        let db = self.db;
        self.s(MAGIC);
        self.f32(VERSION);
        self.f64(oa_now());
        self.s(&db.name);
        self.s("");
        self.len((db.compatibility_level / 10) as usize);
        self.len(db.server.major_version as usize);
        self.s(&db.server.product_version);
        self.len(if db.server.engine_edition == 0 { 3 } else { db.server.engine_edition as usize });
        self.bool(false); // no SQL Compare version
        self.str(None); // scripts folder
        self.str(None); // migrations folder
    }

    fn body(&mut self) {
        let db = self.db;
        let cs = self.cs;
        // History tables of system-versioned tables are written inside them.
        let histories: HashSet<ObjectName> = db.tables.iter().filter_map(|t| t.temporal.as_ref()).map(|h| h.history_table.clone()).collect();
        let tables: Vec<&Table> = db.tables.iter().filter(|t| !histories.contains(&t.object_name())).collect();
        self.coll(&tables, |w, t| {
            w.u8(0); // ordinary table, not a FileTable
            let id = w.id();
            w.table(&TableRec { t, object_id: id, type_table: false });
        });
        let functions: Vec<&Routine> = db.routines.iter().filter(|r| !is_procedure(r)).collect();
        let procedures: Vec<&Routine> = db.routines.iter().filter(|r| is_procedure(r)).collect();
        self.coll(&functions, |w, r| w.function(r));
        self.coll(&db.defaults, |w, d| w.bindable(d, "DEFAULT"));
        self.coll(&db.rules, |w, r| w.bindable(r, "RULE"));
        self.users();
        self.roles();
        // Alias types before table types, as SQL Compare lists them.
        let mut types: Vec<&UserType> = db.types.iter().collect();
        types.sort_by_key(|t| match t.kind {
            UserTypeKind::Alias => 0,
            UserTypeKind::Table => 1,
            UserTypeKind::Clr => 2,
        });
        self.coll(&types, |w, t| w.user_type(t));
        self.coll(&db.views, W::view);
        self.coll(&procedures, |w, r| w.procedure(r));
        self.dependencies();
        let props: Vec<&ExtendedProperty> = db.extended_properties.iter().filter(|p| p.class == "DATABASE").collect();
        self.eprops_of(&props);
        self.bool(true); // full-text enabled
        self.bool(false); // remote data archive
        self.coll(&db.fulltext_catalogs, W::fulltext_catalog);
        self.bool(cs);
        self.early_ids();
        self.comments();
        self.object_blobs();
        let assemblies: Vec<(usize, &Assembly)> = db.assemblies.iter().enumerate().collect();
        self.coll(&assemblies, |w, (i, a)| w.assembly(*i, a));
        // Assembly dependencies: each assembly's references to other user assemblies.
        let refs: Vec<(&str, &str)> = db.assemblies.iter().flat_map(|a| a.references.iter().map(move |r| (a.name.as_str(), r.as_str()))).collect();
        self.f32(VERSION);
        self.len(refs.len());
        for (from, to) in refs {
            self.u8(13);
            self.s(&quote(from));
            self.u8(13);
            self.s(&quote(to));
            self.bool(true);
        }
        self.coll(&db.synonyms, W::synonym);
        let collections: Vec<(usize, &XmlSchemaCollection)> = db.xml_schema_collections.iter().enumerate().collect();
        self.coll(&collections, |w, (i, x)| w.xml_collection(*i, x));
        self.message_types();
        self.contracts();
        self.queues();
        self.coll(&db.services, W::service);
        self.routes();
        let notifications: Vec<&EventNotification> = db.event_notifications.iter().filter(|e| e.queue.is_none()).collect();
        self.coll(&notifications, |w, e| w.event_notification(e));
        self.coll(&db.partition_functions, W::partition_function);
        self.coll(&db.partition_schemes, W::partition_scheme);
        self.schemas();
        self.empty_coll(); // database-level trigger list
        let triggers: Vec<(usize, &DdlTrigger)> = db.ddl_triggers.iter().enumerate().collect();
        self.coll(&triggers, |w, (i, t)| w.ddl_trigger(*i, t));
        self.coll(&db.service_bindings, W::service_binding);
        self.coll(&db.certificates, W::certificate);
        self.coll(&db.symmetric_keys, W::symmetric_key);
        self.coll(&db.asymmetric_keys, W::asymmetric_key);
        self.coll(&db.fulltext_stoplists, W::stoplist);
        self.coll(&db.sequences, W::sequence);
        self.coll(&db.search_property_lists, W::property_list);
        self.coll(&db.security_policies, W::security_policy);
        self.coll(&db.external_file_formats, W::file_format);
        self.coll(&db.external_data_sources, W::data_source);
        self.coll(&db.external_tables, W::external_table);
        self.coll(&db.credentials, |w, c| w.s(c));
    }

    // --------------------------------------------------------------------------- shared records

    /// Object or schema permissions on one securable.
    fn perms(&mut self, class: &str, schema: &str, object: Option<&str>) {
        self.perms_on(class, Some(schema), object);
    }

    /// Permissions on one securable; column permissions with the same grant share a column list.
    fn perms_on(&mut self, class: &str, schema: Option<&str>, object: Option<&str>) {
        let db = self.db;
        let mut list: Vec<(u8, &str, &str, Vec<&str>)> = Vec::new();
        for p in db.permissions.iter().filter(|p| p.class == class && p.schema.as_deref() == schema && p.object.as_deref() == object) {
            let Some(st) = statement(&p.state) else { continue };
            let Some(column) = p.column.as_deref() else {
                list.push((st, &p.permission, &p.grantee, Vec::new()));
                continue;
            };
            match list.iter_mut().find(|x| x.0 == st && x.1 == p.permission && x.2 == p.grantee && !x.3.is_empty()) {
                Some(x) => x.3.push(column),
                None => list.push((st, &p.permission, &p.grantee, vec![column])),
            }
        }
        self.f32(COLL);
        self.len(list.len());
        let cs = self.cs;
        for (st, permission, grantee, columns) in list {
            self.f32(VERSION);
            self.i32(i32::from(st));
            self.s(permission);
            self.s(grantee);
            self.bool(cs);
            self.bool(!columns.is_empty());
            if !columns.is_empty() {
                self.strs(&columns);
            }
        }
    }

    fn no_perms(&mut self) {
        self.f32(COLL);
        self.i32(0);
    }

    /// Database-level permissions of a principal.
    fn db_perms(&mut self, name: &str, connect: bool) {
        let db = self.db;
        let mut list: Vec<(&str, u8)> = Vec::new();
        if connect {
            list.push(("CONNECT", GRANT));
        }
        // SQL Compare leaves out the grants every database gives public.
        let default = |p: &Permission| name == "public" && PUBLIC_DEFAULTS.iter().any(|d| p.permission.eq_ignore_ascii_case(d)) && p.state == "GRANT";
        for p in db.permissions.iter().filter(|p| p.class == "DATABASE" && p.grantee == name && !default(p)) {
            if let Some(st) = statement(&p.state)
                && !(connect && p.permission.eq_ignore_ascii_case("CONNECT"))
            {
                list.push((&p.permission, st));
            }
        }
        self.f32(VERSION);
        self.s(name);
        self.len(list.len());
        for (action, st) in list {
            self.s(action);
            self.u8(st);
        }
        self.i32(0); // script sources
    }

    /// Extended properties of an object (and its columns, constraints and indexes).
    fn eprops(&mut self, class: &str, schema: &str, object: &str) {
        let db = self.db;
        let props: Vec<&ExtendedProperty> =
            db.extended_properties.iter().filter(|p| p.class == class && p.schema.as_deref() == Some(schema) && p.object.as_deref() == Some(object)).collect();
        self.eprops_of(&props);
    }

    fn no_eprops(&mut self) {
        self.eprops_of(&[]);
    }

    fn eprops_of(&mut self, props: &[&ExtendedProperty]) {
        let cs = self.cs;
        self.coll_head(cs, props.len());
        for p in props {
            self.f32(VERSION);
            let (types, names): ([Option<&str>; 3], [Option<&str>; 3]) = match p.class.as_str() {
                "DATABASE" => ([None; 3], [None; 3]),
                "SCHEMA" => ([Some("SCHEMA"), None, None], [p.schema.as_deref(), None, None]),
                // Database-wide objects are named at level 0.
                class if p.schema.is_none() => ([Some(class), None, None], [p.object.as_deref(), None, None]),
                class => (
                    [Some("SCHEMA"), Some(class), p.minor_type.as_deref()],
                    [p.schema.as_deref(), p.object.as_deref(), p.minor_type.as_ref().and(p.minor_name.as_deref())],
                ),
            };
            for t in types {
                self.str(t);
            }
            for n in names {
                self.str(n);
            }
            // sql_variant facets: values are written as nvarchar.
            self.i32(0);
            self.i32(0);
            match &p.value {
                Some(v) => {
                    self.len(v.encode_utf16().count());
                    self.s("nvarchar");
                    self.i8(6);
                    self.u8(1);
                    self.s(v);
                }
                None => {
                    self.i32(0);
                    self.str(None);
                    self.i8(0);
                    self.u8(0);
                }
            }
            self.s(&p.name);
            self.no_ss(3);
        }
    }

    fn execute_as(&mut self, who: Option<&str>) {
        match who {
            None => self.i32(0),
            Some(w) if w.eq_ignore_ascii_case("OWNER") => self.i32(1),
            Some(w) if w.eq_ignore_ascii_case("dbo") || w.eq_ignore_ascii_case("SELF") => self.i32(2),
            Some(w) if w.eq_ignore_ascii_case("guest") => self.i32(3),
            Some(w) => {
                self.i32(4);
                self.s(w);
            }
        }
    }

    /// Objects a module's body uses, from the model's dependencies.
    fn references(&mut self, schema: &str, name: &str) {
        let from = ObjectName::new(schema, name);
        let refs: Vec<(u8, String)> =
            self.db.dependencies.iter().filter(|d| d.from == from).filter_map(|d| self.kinds.get(&d.to).map(|k| (*k, d.to.quoted()))).collect();
        self.len(refs.len());
        for (k, n) in refs {
            self.u8(k);
            self.s(&n);
        }
    }

    /// Compression by partition; partitioned storage lists every partition, as SQL Compare does.
    fn compression(&mut self, storage: &Storage) {
        let list = &storage.compression;
        let code = |c: &str| match c {
            "ROW" => 1,
            "PAGE" => 2,
            "COLUMNSTORE" => 3,
            "COLUMNSTORE_ARCHIVE" => 4,
            _ => 0,
        };
        let partitions = self.partitions(storage);
        if partitions > 0 {
            self.len(partitions);
            for n in 1..=partitions {
                let c = list.iter().find(|p| p.partition as usize == n).or_else(|| list.iter().find(|p| p.partition == 0));
                self.len(n);
                self.u8(c.map_or(0, |c| code(&c.compression)));
            }
            return;
        }
        self.len(list.len());
        for p in list {
            self.len(p.partition.max(1) as usize);
            self.u8(code(&p.compression));
        }
    }

    /// Partitions of storage on a partition scheme; 0 when not partitioned.
    fn partitions(&self, storage: &Storage) -> usize {
        let db = self.db;
        if storage.partition_column.is_none() {
            return 0;
        }
        let scheme = storage.data_space.as_deref().and_then(|d| db.partition_schemes.iter().find(|s| s.name == d));
        scheme.and_then(|s| db.partition_functions.iter().find(|f| f.name == s.function)).map_or(0, |f| f.boundaries.len() + 1)
    }

    // --------------------------------------------------------------------------- tables

    fn table(&mut self, rec: &TableRec) {
        let t = rec.t;
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&t.name);
        self.s(&t.schema);
        if rec.type_table {
            self.no_eprops();
        } else {
            self.eprops("TABLE", &t.schema, &t.name);
        }
        self.i32(rec.object_id);
        if rec.type_table {
            self.no_perms();
        } else {
            self.perms("OBJECT", &t.schema, Some(&t.name));
        }
        let mut indexes: Vec<&Index> = t.indexes.iter().filter(|i| i.kind != IndexKind::Heap).collect();
        indexes.sort_by_key(|i| (index_rank(i), i.name.clone()));
        self.indexes(t, &indexes, &t.statistics, rec.type_table);
        self.coll(&t.foreign_keys, |w, fk| w.foreign_key(t, fk));
        self.coll(&t.check_constraints, |w, ck| w.check(t, ck));
        let parent = t.object_name().quoted();
        self.coll(&t.triggers, |w, tr| w.trigger(&t.schema, &parent, tr));
        self.bool(false); // no full-text index
        self.bool(cs);
        let columns = sorted_columns(&t.columns);
        self.coll(&columns, |w, c| w.column(t, c));
        let partitioned = t.storage.partition_column.is_some();
        // Partitioned tables name their scheme further on, not as their filegroup.
        self.str(t.storage.data_space.as_deref().filter(|_| !rec.type_table && !partitioned));
        self.str(t.text_filegroup.as_deref().filter(|_| !rec.type_table));
        self.bool(partitioned);
        if let Some(pc) = &t.storage.partition_column {
            self.partition_field(t, pc);
        }
        self.str(t.storage.data_space.as_deref().filter(|_| partitioned).map(quote).as_deref());
        self.bool(false); // system object
        self.str(t.filestream_filegroup.as_deref());
        self.compression(&t.storage);
        self.str(None); // authorization
        self.u8(match &t.change_tracking {
            None => 0,
            Some(c) if c.track_columns_updated => 2,
            Some(_) => 1,
        });
        self.str(None); // search property list
        self.u8(match t.lock_escalation.as_str() {
            "DISABLE" => 1,
            "AUTO" => 2,
            _ => 0,
        });
        self.bool(t.memory_optimized);
        self.u8(u8::from(t.durability.as_deref() == Some("SCHEMA_ONLY")));
        self.i32(0); // natively compiled procedures
        self.u8(0); // remote data archive
        self.no_ss(3);
        self.i64(t.modified.as_deref().map_or(0, crate::sources::snp::objects::text_ticks_ms)); // last modified
        let temporal_type = if t.history_table {
            1
        } else if t.temporal.is_some() {
            2
        } else {
            0
        };
        self.u8(temporal_type);
        let history = t.temporal.as_ref().map(|h| &h.history_table);
        self.str(history.map(|h| h.name.as_str()));
        self.str(history.map(|h| h.schema.as_str()));
        match t.temporal.as_ref().and_then(|h| h.retention.as_deref()).and_then(split_retention) {
            Some((period, unit)) => {
                self.i32(period);
                self.s(&unit);
            }
            None => {
                self.i32(-1);
                self.str(None);
            }
        }
        self.bool(t.graph.as_deref() == Some("NODE"));
        self.bool(t.graph.as_deref() == Some("EDGE"));
        self.no_ss(1); // change tracking
        let db = self.db;
        match history.and_then(|h| db.tables.iter().find(|x| x.object_name() == *h)) {
            Some(h) => {
                self.bool(true);
                let id = self.id();
                self.table(&TableRec { t: h, object_id: id, type_table: false });
            }
            None => self.bool(false),
        }
        self.bool(false); // large value types out of row
        self.coll(&t.edge_constraints, W::edge_constraint);
        self.i32(0); // ALTER TABLE SET script sources
    }

    /// Indexes, then statistics, which SQL Compare keeps in the same list.
    fn indexes(&mut self, t: &Table, indexes: &[&Index], statistics: &[Statistic], type_table: bool) {
        let cs = self.cs;
        self.coll_head(cs, indexes.len() + statistics.len());
        for ix in indexes {
            self.index(t, ix, type_table);
        }
        for s in statistics {
            self.statistic(s, t.storage.data_space.as_deref());
        }
    }

    /// The column a partitioned table or index is split on.
    fn partition_field(&mut self, t: &Table, name: &str) {
        match t.columns.iter().find(|c| c.name == name) {
            Some(c) => self.column(t, c),
            None => self.view_column(name),
        }
    }

    fn column(&mut self, t: &Table, c: &Column) {
        let db = self.db;
        let ty = facets(db, &c.data_type);
        self.f32(VERSION);
        self.i32(ty.sql_type);
        self.s(&c.name);
        self.i32(ty.size);
        self.i32(ty.scale);
        self.i32(ty.precision);
        match &c.identity {
            Some(id) => {
                self.s(&id.seed);
                self.s("0");
                self.i64(id.increment.trim().parse::<f64>().map(|f| f as i64).unwrap_or(1));
            }
            None => {
                self.s("0");
                self.s("0");
                self.i64(0);
            }
        }
        let mut flags = 0i16;
        for (on, bit) in [
            (c.identity.is_some(), 0x1),
            (c.computed.is_some(), 0x2),
            (c.identity.as_ref().is_some_and(|i| i.not_for_replication), 0x4),
            (c.nullable, 0x10),
            (c.rowguidcol, 0x20),
            (c.computed.as_ref().is_some_and(|x| x.persisted), 0x100),
            (c.data_type.xml_document, 0x200),
            (c.sparse, 0x800),
            (c.filestream, 0x1000),
            (c.column_set, 0x2000),
        ] {
            if on {
                flags |= bit;
            }
        }
        self.i16(flags);
        self.s(c.bound_default.as_deref().unwrap_or(""));
        let default_id = match &c.default {
            Some(d) => {
                let id = self.id();
                self.text(id, d.definition.clone(), false);
                id
            }
            None => 0,
        };
        self.s(c.default.as_ref().map_or("", |d| d.name.as_str()));
        self.i32(default_id);
        self.bool(c.default.as_ref().is_some_and(|d| d.system_named));
        let computed = c.computed.as_ref().map(|x| super::expr::rewrite(&x.definition));
        self.s(computed.as_deref().unwrap_or(""));
        let collation = match (&c.collation, ty.character) {
            (Some(col), _) => col.clone(),
            // Alias types of character types take the database collation.
            (None, true) if c.data_type.schema.is_some() => db.collation.clone(),
            _ => String::new(),
        };
        self.s(&collation);
        self.s(c.bound_rule.as_deref().unwrap_or(""));
        self.s(&ty.type_name);
        self.s(c.default.as_ref().map_or("", |d| d.definition.as_str()));
        self.i32(0); // full-text language
        self.str(None); // full-text type column
        self.s(&ty.type_schema);
        self.str(c.data_type.xml_collection.as_deref());
        self.s(c.masking_function.as_deref().unwrap_or(""));
        let used = c.computed.as_ref().map(|x| referenced_columns(&x.definition, &t.columns)).unwrap_or_default();
        self.strs(&used);
        self.u8(match c.generated_always.as_deref() {
            Some("ROW START") => 1,
            Some("ROW END") => 2,
            _ => 0,
        });
        self.bool(c.hidden);
        self.u8(0); // graph column kind
        self.no_ss(3);
        self.sensitivity(c.sensitivity.as_ref());
        self.str(ty.udt.as_deref());
    }

    /// A view's output column: names only.
    fn view_column(&mut self, name: &str) {
        self.f32(VERSION);
        self.i32(0);
        self.s(name);
        for _ in 0..3 {
            self.i32(0);
        }
        self.s("0");
        self.s("0");
        self.i64(0);
        self.i16(0);
        self.s("");
        self.str(None);
        self.i32(0);
        self.bool(false);
        self.s("");
        self.s("");
        self.s("");
        self.s("other");
        self.str(None);
        self.i32(0);
        self.str(None);
        self.s("sys");
        self.str(None);
        self.s("");
        self.i32(0);
        self.u8(0);
        self.bool(false);
        self.u8(0);
        self.no_ss(3);
        self.sensitivity(None);
        self.str(None);
    }

    /// A data classification; the rank is not kept.
    fn sensitivity(&mut self, s: Option<&Sensitivity>) {
        self.f32(VERSION);
        let parts = s.map(|s| [&s.label, &s.label_id, &s.information_type, &s.information_type_id]);
        for i in 0..4 {
            self.str(parts.and_then(|p| p[i].as_deref()));
        }
        self.no_ss(5);
    }

    fn index(&mut self, t: &Table, ix: &Index, type_table: bool) {
        let cs = self.cs;
        self.f32(VERSION);
        let columnstore = matches!(ix.kind, IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore);
        self.u8(match ix.kind {
            IndexKind::Xml => 2,
            IndexKind::Spatial => 3,
            _ if columnstore => 4,
            _ => 0,
        });
        self.bool(cs);
        self.s(&ix.name);
        self.len(ix.included.len() + ix.columns.len());
        let keys = ix.columns.len();
        for (i, name) in ix.included.iter().enumerate() {
            self.s(name);
            self.bool(false);
            self.bool(true);
            self.len(keys + i + 1);
        }
        for (i, c) in ix.columns.iter().enumerate() {
            self.s(&c.name);
            self.bool(c.descending);
            self.bool(false);
            self.len(i + 1);
        }
        let partitioned = ix.storage.partition_column.is_some();
        let mut flags = 0i16;
        for (on, bit) in [
            (ix.ignore_dup_key, 0x1),
            (ix.no_recompute, 0x2),
            (ix.allow_row_locks, 0x4),
            (ix.allow_page_locks, 0x8),
            (ix.pad_index, 0x10),
            (partitioned, 0x20),
            (ix.disabled, 0x40),
            (ix.incremental, 0x100),
            (ix.optimize_for_sequential_key, 0x800),
        ] {
            if on {
                flags |= bit;
            }
        }
        self.i16(flags);
        self.str(ix.storage.data_space.as_deref().filter(|d| !(type_table && *d == "PRIMARY")));
        self.u8(ix.fill_factor);
        let constraint = ix.primary_key || ix.unique_constraint;
        let id = if constraint { self.id() } else { 0 };
        self.i32(id);
        self.bool(ix.system_named);
        self.no_ss(3);
        self.bool(false); // FileTable primary key
        match ix.kind {
            IndexKind::Xml => self.xml_tail(ix),
            IndexKind::Spatial => self.spatial_tail(ix),
            _ => {
                self.f32(INDEX_TAIL);
                self.bool(ix.primary_key);
                self.bool(matches!(ix.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore));
                self.bool(ix.unique);
                self.bool(ix.unique_constraint);
                self.bool(false); // full-text key
                if partitioned {
                    self.bool(true);
                    self.partition_field(t, ix.storage.partition_column.as_deref().unwrap_or_default());
                }
                self.str(ix.filter.as_deref());
                self.compression(&ix.storage);
                self.bool(columnstore);
                self.bool(ix.kind == IndexKind::NonclusteredHash);
                self.i32(ix.bucket_count.and_then(|n| i32::try_from(n).ok()).unwrap_or(0));
            }
        }
    }

    fn xml_tail(&mut self, ix: &Index) {
        let x = ix.xml.clone().unwrap_or_default();
        self.f32(XML_TAIL);
        self.i32(match x.secondary_type.as_deref() {
            Some("VALUE") => 1,
            Some("PATH") => 2,
            Some("PROPERTY") => 3,
            _ => 0,
        });
        self.str(x.using_index.as_deref());
        self.u8(match x.kind {
            XmlIndexKind::Primary => 0,
            XmlIndexKind::Secondary => 1,
            XmlIndexKind::Selective => 2,
            XmlIndexKind::SecondarySelective => 3,
        });
        self.len(x.paths.len());
        for p in &x.paths {
            self.u8(u8::from(p.sql));
            self.f32(XML_TAIL);
            self.i32(p.id);
            self.s(&p.name);
            self.s(&p.path);
            if p.sql {
                let ty = p.data_type.as_ref().map(|t| facets(self.db, t)).unwrap_or_default();
                self.i32(ty.sql_type);
                self.i32(ty.size);
                self.i32(ty.precision);
                self.i32(ty.scale);
            } else {
                self.str(p.xquery_type.as_deref());
                self.i32(p.max_length.unwrap_or(if p.node_hint { 0 } else { -1 }));
                self.bool(p.node_hint);
            }
            self.bool(p.singleton);
        }
        // The DEFAULT namespace last, as SQL Compare lists them.
        let mut namespaces: Vec<&XmlNamespace> = x.namespaces.iter().collect();
        namespaces.sort_by_key(|n| n.prefix.is_none());
        self.len(namespaces.len());
        for n in namespaces {
            self.f32(XML_TAIL);
            self.s(&n.uri);
            self.str(n.prefix.as_deref());
        }
        self.str(x.using_path.as_deref());
    }

    fn spatial_tail(&mut self, ix: &Index) {
        let s = ix.spatial.clone().unwrap_or_default();
        self.f32(VERSION);
        self.u8(if s.geography { 2 } else { 1 });
        self.u8(match s.tessellation.as_str() {
            "GEOMETRY_GRID" => 1,
            "GEOGRAPHY_GRID" => 2,
            "GEOMETRY_AUTO_GRID" => 3,
            _ => 4,
        });
        // The model has xmin, ymin, xmax, ymax; the file xmin, xmax, ymin, ymax.
        let b: Vec<f64> = s.bounding_box.iter().map(|v| v.trim().parse().unwrap_or(0.0)).collect();
        let at = |i: usize| b.get(i).copied().unwrap_or(0.0);
        for v in [at(0), at(2), at(1), at(3)] {
            self.f64(v);
        }
        for i in 0..4 {
            self.i16(match s.grids.get(i).map(String::as_str) {
                Some("LOW") => 16,
                Some("HIGH") => 256,
                _ => 64,
            });
        }
        self.i16(i16::try_from(s.cells_per_object).unwrap_or(0));
        self.i16(0); // compression
    }

    fn foreign_key(&mut self, t: &Table, fk: &ForeignKey) {
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&fk.referenced_table.quoted());
        self.s(&t.object_name().quoted());
        self.s(&fk.name);
        self.bool(fk.not_for_replication);
        self.u8(referential_action(&fk.on_update));
        self.u8(referential_action(&fk.on_delete));
        self.bool(fk.disabled);
        self.bool(cs);
        self.bool(fk.not_trusted);
        self.len(fk.columns.len());
        for (i, c) in fk.columns.iter().enumerate() {
            self.s(c);
            self.s(fk.referenced_columns.get(i).map_or("", String::as_str));
        }
        let id = self.id();
        self.i32(id);
        self.bool(fk.system_named);
        self.no_ss(2);
        self.no_eprops();
    }

    fn check(&mut self, t: &Table, ck: &CheckConstraint) {
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&ck.name);
        self.bool(ck.not_for_replication);
        self.bool(ck.disabled);
        let id = self.id();
        self.i32(id);
        self.s(&super::expr::rewrite(&ck.definition));
        self.bool(cs);
        self.bool(ck.not_trusted);
        // A single column means a column-level check, so a table-level one never lists just one.
        let columns = match &ck.column {
            Some(c) => vec![c.clone()],
            None => Some(referenced_columns(&ck.definition, &t.columns)).filter(|c| c.len() != 1).unwrap_or_default(),
        };
        self.strs(&columns);
        self.bool(ck.system_named);
        self.no_ss(2);
    }

    fn trigger(&mut self, schema: &str, parent: &str, tr: &Trigger) {
        let cs = self.cs;
        let id = self.id();
        self.text(id, trigger_text(&tr.definition, schema, &tr.name, parent), tr.encrypted);
        self.f32(VERSION);
        self.s(&format!("{}.{}", quote(schema), quote(&tr.name)));
        self.i32(id);
        self.bool(true); // quoted identifier
        self.bool(true); // ansi nulls
        let on = |e: &str| tr.events.iter().any(|x| x.eq_ignore_ascii_case(e));
        self.bool(on("UPDATE"));
        self.bool(on("INSERT"));
        self.bool(on("DELETE"));
        self.bool(!tr.instead_of);
        self.bool(tr.instead_of);
        self.i32(id);
        let order = |e: &str, o: &str| tr.order.iter().any(|(x, y)| x.eq_ignore_ascii_case(e) && y.eq_ignore_ascii_case(o));
        for (e, o) in [("DELETE", "FIRST"), ("INSERT", "FIRST"), ("UPDATE", "FIRST"), ("DELETE", "LAST"), ("INSERT", "LAST"), ("UPDATE", "LAST")] {
            self.bool(order(e, o));
        }
        self.bool(tr.encrypted);
        self.i32(0); // EXECUTE AS CALLER
        for _ in 0..3 {
            self.str(None);
        }
        self.bool(false); // CLR
        self.bool(tr.disabled);
        self.bool(tr.not_for_replication);
        self.no_ss(5);
        self.bool(cs);
        self.no_eprops();
    }

    // --------------------------------------------------------------------------- programmability

    fn function(&mut self, r: &Routine) {
        let cs = self.cs;
        let clr = r.clr_method.is_some();
        let id = self.id();
        if !clr {
            self.text(id, module_sql(&r.definition, "CREATE", &qualified(&r.schema, &r.name)), r.encrypted);
        }
        self.f32(VERSION);
        self.s(&r.name);
        self.s(&r.schema);
        self.i32(if clr { -1 } else { id });
        self.perms("OBJECT", &r.schema, Some(&r.name));
        self.bool(r.settings.quoted_identifier && !clr);
        self.bool(r.settings.ansi_nulls && !clr);
        self.i32(id);
        let class = if r.kind == RoutineKind::ClrAggregate { "AGGREGATE" } else { "FUNCTION" };
        self.eprops_function(class, r);
        self.bool(cs);
        self.bool(r.schema_bound);
        self.i32(0); // parent tables
        self.i32(0); // parent security policies
        self.i32(0); // user types used
        self.i32(match r.kind {
            RoutineKind::InlineTableFunction => 1,
            RoutineKind::TableFunction | RoutineKind::ClrTableFunction => 3,
            RoutineKind::ClrAggregate => 4,
            _ => 2,
        });
        self.bool(r.encrypted);
        self.bool(false); // recursive
        self.clr_binding(r.clr_method.as_deref());
        self.result_columns(r);
        self.bool(clr);
        self.params(&r.parameters);
        self.returns(r.returns.as_deref());
        self.bool(false); // system object
        self.bool(r.null_on_null_input);
        self.str(None); // authorization
        self.execute_as(r.execute_as.as_deref());
        self.no_ss(2);
        self.i64(r.modified.as_deref().map_or(0, crate::sources::snp::objects::text_ticks_ms)); // last modified
        self.references(&r.schema, &r.name);
    }

    fn eprops_function(&mut self, class: &str, r: &Routine) {
        let db = self.db;
        // The reader files aggregates as functions.
        let props: Vec<ExtendedProperty> = db
            .extended_properties
            .iter()
            .filter(|p| p.class == "FUNCTION" && p.schema.as_deref() == Some(r.schema.as_str()) && p.object.as_deref() == Some(r.name.as_str()))
            .map(|p| ExtendedProperty { class: class.into(), ..p.clone() })
            .collect();
        let refs: Vec<&ExtendedProperty> = props.iter().collect();
        self.eprops_of(&refs);
    }

    /// CLR assembly, class and method of `[a].[b].[c]`: the assembly quoted, the others as
    /// sys.assembly_modules has them.
    fn clr_binding(&mut self, method: Option<&str>) {
        let parts = method.map(split_multipart).unwrap_or_default();
        self.str(parts.first().map(|a| quote(a)).as_deref());
        for i in 1..3 {
            self.str(parts.get(i).map(String::as_str));
        }
    }

    fn procedure(&mut self, r: &Routine) {
        let cs = self.cs;
        let clr = r.clr_method.is_some();
        let id = self.id();
        if !clr {
            self.text(id, module_sql(&r.definition, "CREATE", &qualified(&r.schema, &r.name)), r.encrypted);
        }
        self.f32(VERSION);
        self.s(&r.name);
        self.s(&r.schema);
        self.i32(if clr { -1 } else { id });
        self.bool(r.native_compilation);
        self.eprops("PROCEDURE", &r.schema, &r.name);
        self.i32(id);
        self.perms("OBJECT", &r.schema, Some(&r.name));
        let mut flags = 0i16;
        for (on, bit) in [(r.settings.quoted_identifier && !clr, 0x1), (r.settings.ansi_nulls && !clr, 0x2), (clr, 0x4), (r.encrypted, 0x8)] {
            if on {
                flags |= bit;
            }
        }
        self.i16(flags);
        self.bool(cs);
        self.i32(0); // user types used
        // SQL Compare leaves EXECUTE AS of procedures to the module text.
        self.i32(0);
        self.clr_binding(r.clr_method.as_deref());
        self.params(&r.parameters);
        self.str(None); // authorization
        self.i32(0); // schema-bound tables
        self.i32(0); // numbered procedures
        self.no_ss(2);
        self.i64(r.modified.as_deref().map_or(0, crate::sources::snp::objects::text_ticks_ms)); // last modified
        self.references(&r.schema, &r.name);
    }

    fn view(&mut self, v: &View) {
        let cs = self.cs;
        let id = self.id();
        let name = qualified(&v.schema, &v.name);
        self.text(id, module_sql(&v.definition, "CREATE", &name), v.encrypted);
        self.f32(VERSION);
        self.s(&v.name);
        self.s(&v.schema);
        self.i32(id);
        self.eprops("VIEW", &v.schema, &v.name);
        self.i32(id);
        self.perms("OBJECT", &v.schema, Some(&v.name));
        // Indexes refer to columns by name only, so a table stands in for the view.
        let host = Table { schema: v.schema.clone(), name: v.name.clone(), columns: v.columns.clone(), ..Default::default() };
        let mut indexes: Vec<&Index> = v.indexes.iter().collect();
        indexes.sort_by_key(|i| (index_rank(i), i.name.clone()));
        self.indexes(&host, &indexes, &v.statistics, false);
        self.coll(&v.triggers, |w, tr| w.trigger(&v.schema, &name, tr));
        self.bool(cs);
        // Index columns must name output columns: models read from snapshots have none.
        let mut columns: Vec<&str> = sorted_columns(&v.columns).iter().map(|c| c.name.as_str()).collect();
        for ix in &v.indexes {
            for name in ix.columns.iter().map(|c| c.name.as_str()).chain(ix.included.iter().map(String::as_str)) {
                if !columns.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                    columns.push(name);
                }
            }
        }
        self.coll(&columns, |w, c| w.view_column(c));
        self.bool(v.settings.quoted_identifier);
        self.bool(v.settings.ansi_nulls);
        self.bool(v.schema_bound);
        self.bool(v.encrypted);
        self.bool(false); // system object
        self.bool(false); // no full-text index
        self.str(None); // authorization
        self.str(None); // search property list
        self.references(&v.schema, &v.name);
        self.no_ss(2);
        self.i64(v.modified.as_deref().map_or(0, crate::sources::snp::objects::text_ticks_ms)); // last modified
    }

    fn user_type(&mut self, t: &UserType) {
        let db = self.db;
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&t.name);
        self.s(&t.schema);
        self.eprops("TYPE", &t.schema, &t.name);
        self.bool(cs);
        let (sql_type, base) = match t.kind {
            UserTypeKind::Table => (-3, None),
            UserTypeKind::Clr => (-2, None),
            UserTypeKind::Alias => (0, t.base_type.as_ref().map(|b| facets(db, b))),
        };
        self.bool(t.kind != UserTypeKind::Table && t.nullable);
        match &base {
            Some(f) => {
                self.i32(f.size);
                self.i32(f.precision);
                self.i32(f.scale);
                self.i32(f.sql_type);
                self.s(&f.type_name);
            }
            None => {
                self.i32(-1);
                self.i32(0);
                self.i32(0);
                self.i32(sql_type);
                self.str(None);
            }
        }
        self.s(""); // bound rule
        self.s(""); // bound default
        let me = ObjectName::new(&t.schema, &t.name);
        let tables: Vec<String> = db
            .tables
            .iter()
            .filter(|x| x.columns.iter().any(|c| c.data_type.schema.as_deref() == Some(me.schema.as_str()) && c.data_type.name == me.name))
            .map(|x| x.object_name().quoted())
            .collect();
        self.strs(&tables);
        self.i32(0); // functions using it
        self.i32(0); // procedures using it
        let (assembly, class) = match t.clr_class.as_deref().map(split_multipart).as_deref() {
            Some([a, rest @ ..]) if !rest.is_empty() => (a.clone(), Some(rest.join("."))),
            _ => (String::new(), None),
        };
        self.s(&if assembly.is_empty() { assembly } else { quote(&assembly) });
        self.str(class.as_deref());
        if t.kind == UserTypeKind::Table {
            let id = self.id();
            let table = Table {
                schema: "sys".into(),
                name: format!("TT_{}_{:08X}", t.name, id),
                columns: t.columns.clone(),
                indexes: t.indexes.clone(),
                check_constraints: t.check_constraints.clone(),
                memory_optimized: t.memory_optimized,
                lock_escalation: "TABLE".into(),
                ..Default::default()
            };
            self.table(&TableRec { t: &table, object_id: id, type_table: true });
        }
        self.perms("TYPE", &t.schema, Some(&t.name));
        self.str(None); // authorization
        self.no_ss(2);
        self.i32(0); // user types used
    }

    fn synonym(&mut self, s: &Synonym) {
        self.f32(VERSION);
        self.s(&s.name);
        self.s(&s.schema);
        let id = self.id();
        self.i32(id);
        self.s(&s.base_object);
        self.eprops("SYNONYM", &s.schema, &s.name);
        self.perms("OBJECT", &s.schema, Some(&s.name));
        self.str(None);
        self.no_ss(2);
    }

    fn sequence(&mut self, s: &Sequence) {
        let cs = self.cs;
        self.perms("OBJECT", &s.schema, Some(&s.name));
        self.eprops("SEQUENCE", &s.schema, &s.name);
        self.f32(VERSION);
        self.s(&s.name);
        self.s(&s.schema);
        self.i32(0);
        let id = self.id();
        self.i32(id);
        self.bool(cs);
        for v in [&s.start, &s.increment, &s.minimum, &s.maximum] {
            self.s(v);
        }
        self.bool(s.cycle);
        self.bool(s.cached);
        self.i32(s.cache_size.filter(|_| s.cached).and_then(|n| i32::try_from(n).ok()).unwrap_or(-1));
        let ty = facets(self.db, &s.data_type);
        self.s(&ty.type_name);
        self.s(&ty.type_schema);
        self.u8(u8::try_from(ty.precision).unwrap_or(0));
        self.u8(u8::try_from(ty.scale).unwrap_or(0));
        self.s(&s.start); // current value
        self.bool(false); // exhausted
        self.str(None);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- security

    fn users(&mut self) {
        let db = self.db;
        let users: Vec<&Principal> = db.principals.iter().filter(|p| !p.is_role()).collect();
        let cs = self.cs;
        self.coll_head(cs, users.len() + 2);
        // dbo and guest, then the database's own users.
        self.user(&Principal { name: "dbo".into(), login: db.owner.clone(), default_schema: Some("dbo".into()), ..Default::default() }, 1, true);
        self.user(&Principal { name: "guest".into(), default_schema: Some("guest".into()), ..Default::default() }, 1, false);
        for p in users {
            let user_type = match p.kind.as_str() {
                "WINDOWS_USER" | "WINDOWS_GROUP" => 2,
                "CERTIFICATE_MAPPED_USER" => 3,
                "ASYMMETRIC_KEY_MAPPED_USER" => 4,
                "EXTERNAL_USER" | "EXTERNAL_GROUP" => 5,
                _ => 1,
            };
            let denied = self.db.permissions.iter().any(|x| x.class == "DATABASE" && x.grantee == p.name && x.permission.eq_ignore_ascii_case("CONNECT"));
            self.user(p, user_type, !p.no_connect && !denied);
        }
    }

    fn user(&mut self, p: &Principal, user_type: i32, connect: bool) {
        self.f32(VERSION);
        self.s(&p.name);
        self.str(p.login.as_deref());
        self.i32(user_type);
        self.str(p.certificate.as_deref());
        self.str(p.asymmetric_key.as_deref());
        self.str(p.default_schema.as_deref());
        self.no_eprops();
        self.no_perms();
        self.db_perms(&p.name, connect);
        self.no_ss(1);
    }

    fn roles(&mut self) {
        let db = self.db;
        let roles: Vec<&Principal> = db.principals.iter().filter(|p| p.is_role()).collect();
        let cs = self.cs;
        self.coll_head(cs, roles.len() + 1 + FIXED_ROLES.len());
        self.role("public", Some("dbo"), false);
        for p in roles {
            // Application roles keep their default schema where roles keep their owner.
            let app = p.kind == "APPLICATION_ROLE";
            let owner = if app { p.default_schema.as_deref() } else { Some(p.owner.as_deref().unwrap_or("dbo")) };
            self.role(&p.name, owner, app);
        }
        for r in FIXED_ROLES {
            self.role(r, Some("dbo"), false);
        }
    }

    fn role(&mut self, name: &str, owner: Option<&str>, app_role: bool) {
        let db = self.db;
        let cs = self.cs;
        self.f32(VERSION);
        self.s(name);
        self.str(owner);
        self.bool(cs);
        self.bool(app_role);
        let members: Vec<&str> = db.principals.iter().filter(|p| p.member_of.iter().any(|m| m == name)).map(|p| p.name.as_str()).collect();
        self.strs(&members);
        self.no_perms();
        self.db_perms(name, false);
        self.no_eprops();
        self.no_ss(2);
        self.i32(-1); // no member script sources
    }

    fn schemas(&mut self) {
        let db = self.db;
        let builtin = ["dbo", "guest", "INFORMATION_SCHEMA", "sys"];
        let own: Vec<&Schema> = db.schemas.iter().filter(|s| !builtin.contains(&s.name.as_str()) && !FIXED_ROLES.contains(&s.name.as_str())).collect();
        let cs = self.cs;
        self.coll_head(cs, builtin.len() + own.len() + FIXED_ROLES.len());
        for name in builtin {
            let owner = db.schemas.iter().find(|s| s.name == name).map_or(name, |s| s.owner.as_str());
            self.schema(name, owner, true);
        }
        for s in own {
            self.schema(&s.name, &s.owner, false);
        }
        for name in FIXED_ROLES {
            self.schema(name, name, true);
        }
    }

    fn schema(&mut self, name: &str, owner: &str, system: bool) {
        let cs = self.cs;
        self.f32(VERSION);
        self.s(name);
        self.bool(system);
        self.s(owner);
        self.bool(cs);
        let db = self.db;
        let props: Vec<&ExtendedProperty> = db.extended_properties.iter().filter(|p| p.class == "SCHEMA" && p.schema.as_deref() == Some(name)).collect();
        self.eprops_of(&props);
        self.perms("SCHEMA", name, None);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- Service Broker

    fn message_types(&mut self) {
        let db = self.db;
        let cs = self.cs;
        self.coll_head(cs, MESSAGE_TYPES.len() + db.message_types.len());
        for (i, (prefix, name, validation, contract)) in MESSAGE_TYPES.into_iter().enumerate() {
            self.f32(VERSION);
            self.s(&format!("{prefix}{name}"));
            self.s("dbo");
            self.len(i + 1);
            self.bool(cs);
            self.i32(validation);
            self.str(None);
            self.no_eprops();
            self.no_perms();
            let contracts: Vec<String> = contract.map(|(p, n)| format!("[{p}{n}]")).into_iter().collect();
            self.strs(&contracts);
            self.no_ss(2);
        }
        for m in &db.message_types {
            self.message_type(m);
        }
    }

    fn contracts(&mut self) {
        let db = self.db;
        let cs = self.cs;
        self.coll_head(cs, CONTRACTS.len() + db.contracts.len());
        for (prefix, name, id, usages) in CONTRACTS {
            self.f32(VERSION);
            self.s(&format!("{prefix}{name}"));
            self.s("dbo");
            self.i32(id);
            self.no_eprops();
            self.bool(true); // system object
            self.bool(cs);
            self.coll_head(cs, usages.len());
            for (p, n, initiator, target) in usages {
                self.s(&format!("[{p}{n}]"));
                self.bool(*initiator);
                self.bool(*target);
            }
            self.no_perms();
            self.i32(0); // services
            self.no_ss(2);
        }
        for k in &db.contracts {
            self.contract(k);
        }
    }

    fn queues(&mut self) {
        let db = self.db;
        let cs = self.cs;
        self.coll_head(cs, QUEUES.len() + db.queues.len());
        for (name, id) in QUEUES {
            self.f32(VERSION);
            self.s(name);
            self.s("dbo");
            self.i32(id);
            self.bool(true); // system object
            self.bool(cs);
            self.i16(0); // max readers
            self.str(None); // activation procedure
            self.i32(2); // EXECUTE AS dbo
            for on in [false, true, true, false] {
                // activation, receive, enqueue, retention
                self.bool(on);
            }
            self.s("PRIMARY");
            self.no_eprops();
            self.empty_coll(); // event notifications
            self.i32(0); // services
            self.str(None); // authorization
            self.no_perms();
            self.bool(false); // poison message handling
            self.no_ss(2);
        }
        for q in &db.queues {
            self.queue(q);
        }
    }

    fn routes(&mut self) {
        let db = self.db;
        let cs = self.cs;
        self.coll_head(cs, 1 + db.routes.len());
        self.f32(VERSION);
        self.s("AutoCreatedLocal");
        self.s("dbo");
        self.i32(65_536);
        self.bool(cs);
        self.bool(true); // system object
        self.str(None);
        self.str(None);
        self.s("LOCAL");
        self.str(None);
        self.i64(0); // lifetime
        self.bool(false);
        self.no_eprops();
        self.no_perms();
        self.no_ss(2);
        for r in &db.routes {
            self.route(r);
        }
    }

    // --------------------------------------------------------------------------- dependencies and text

    /// Dependencies by name: system contracts, then what the model knows, in the forms SQL
    /// Compare lists them (DML triggers both ways, check constraints on their table, foreign keys).
    fn dependencies(&mut self) {
        let db = self.db;
        let mut list: Vec<(u8, String, u8, String, bool)> = Vec::new();
        for (prefix, name, _, usages) in CONTRACTS {
            for (p, n, _, _) in usages {
                list.push((OT_CONTRACT, format!("[{prefix}{name}]"), OT_MESSAGE_TYPE, format!("[{p}{n}]"), true));
            }
        }
        for s in &db.synonyms {
            if let Some(target) = synonym_target(&s.base_object, &db.name)
                && let Some(&k) = self.kinds.get(&target)
            {
                list.push((OT_SYNONYM, qualified(&s.schema, &s.name), k, target.quoted(), true));
            }
        }
        for t in &db.tables {
            let me = t.object_name().quoted();
            for tr in &t.triggers {
                let trigger = qualified(&t.schema, &tr.name);
                list.push((OT_TABLE, me.clone(), OT_TRIGGER, trigger.clone(), false));
                list.push((OT_TRIGGER, trigger, OT_TABLE, me.clone(), true));
            }
        }
        // Model dependencies: tables' own (sequences, functions) first, modules' after the keys.
        let mut seen = HashSet::new();
        let mut modules = Vec::new();
        for d in &db.dependencies {
            let (Some(&from), Some(&to)) = (self.kinds.get(&d.from), self.kinds.get(&d.to)) else { continue };
            // Foreign keys, typed columns and synonyms are listed from the model's own objects.
            if from == OT_SYNONYM || (from == OT_TABLE && matches!(to, OT_TABLE | OT_TYPE)) {
                continue;
            }
            if from == OT_TRIGGER && self.trigger_parents.get(&d.from) == Some(&d.to) {
                continue;
            }
            if seen.insert((&d.from, &d.to)) {
                let entry = (from, d.from.quoted(), to, d.to.quoted(), true);
                if from == OT_TABLE { list.push(entry) } else { modules.push(entry) }
            }
        }
        for t in &db.tables {
            for fk in &t.foreign_keys {
                list.push((OT_TABLE, t.object_name().quoted(), OT_TABLE, fk.referenced_table.quoted(), false));
            }
        }
        for t in &db.tables {
            for ck in &t.check_constraints {
                list.push((OT_CHECK, qualified(&t.schema, &ck.name), OT_TABLE, t.object_name().quoted(), true));
            }
        }
        modules.sort_by_key(|m| match m.0 {
            OT_VIEW => 0,
            OT_FUNCTION => 1,
            OT_PROCEDURE => 2,
            _ => 3,
        });
        list.extend(modules);
        self.f32(VERSION);
        self.len(list.len());
        for (ft, from, tt, to, flag) in list {
            self.u8(ft);
            self.s(&from);
            self.u8(tt);
            self.s(&to);
            self.bool(flag);
        }
    }

    /// The comments blob store: module and default text by id, then the encrypted ids.
    fn comments(&mut self) {
        let mut texts = std::mem::take(&mut self.texts);
        texts.sort_by_key(|(id, _)| *id);
        texts.push((MISSING_ID, MISSING_TEXT.into()));
        self.f32(BLOBS);
        self.len(texts.len());
        for (id, text) in &texts {
            self.u64(*id);
            self.len(text.len());
            self.b.b.extend(text.as_bytes());
        }
        let encrypted = std::mem::take(&mut self.encrypted);
        self.len(encrypted.len());
        for id in encrypted {
            self.u64(id);
        }
    }
}

// ------------------------------------------------------------------------------- helpers

fn is_procedure(r: &Routine) -> bool {
    matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure)
}

fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote(schema), quote(name))
}

/// Columns in ordinal order (the model keeps them so already).
fn sorted_columns(columns: &[Column]) -> Vec<&Column> {
    let mut list: Vec<&Column> = columns.iter().collect();
    list.sort_by_key(|c| c.ordinal);
    list
}

/// SQL Compare lists indexes by index id: the clustered index, keys, other relational
/// indexes, then XML and spatial indexes by name.
fn index_rank(ix: &Index) -> u8 {
    match ix.kind {
        IndexKind::Xml => 4,
        IndexKind::Spatial => 5,
        IndexKind::Clustered | IndexKind::ClusteredColumnstore => 0,
        _ if ix.primary_key => 1,
        _ if ix.unique_constraint => 2,
        _ => 3,
    }
}

/// GRANT 205, DENY 206, GRANT WITH GRANT OPTION 1.
fn statement(state: &str) -> Option<u8> {
    match state {
        "GRANT" => Some(GRANT),
        "DENY" => Some(206),
        "GRANT_WITH_GRANT_OPTION" => Some(1),
        _ => None,
    }
}

fn referential_action(action: &str) -> u8 {
    match action.replace(' ', "_").to_ascii_uppercase().as_str() {
        "CASCADE" => 1,
        "SET_NULL" => 2,
        "SET_DEFAULT" => 3,
        _ => 0,
    }
}

/// `6 MONTHS` → (6, `MONTHS`).
/// `6 MONTHS` → (6, `MONTH`), the singular unit SQL Compare stores.
fn split_retention(r: &str) -> Option<(i32, String)> {
    let (n, unit) = r.trim().split_once(' ')?;
    Some((n.parse().ok()?, unit.trim().to_ascii_uppercase().trim_end_matches('S').to_string()))
}

/// Current time as an OLE Automation date.
fn oa_now() -> f64 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    secs / 86_400.0 + 25_569.0
}

/// How a type is described in Field, Param and UDT records.
#[derive(Default)]
struct Facets {
    sql_type: i32,
    size: i32,
    precision: i32,
    scale: i32,
    type_name: String,
    type_schema: String,
    /// `[schema].[name]` of a user-defined type.
    udt: Option<String>,
    /// A character type (takes a collation).
    character: bool,
}

/// SqlType code, size (characters for character types), precision and scale as sys.columns
/// reports them.
fn facets(db: &Database, t: &DataType) -> Facets {
    if let Some(schema) = &t.schema {
        let udt = db.types.iter().find(|u| &u.schema == schema && u.name == t.name);
        let mut f = match udt {
            Some(u) if u.kind == UserTypeKind::Alias => u.base_type.as_ref().map(|b| facets(db, b)).unwrap_or_default(),
            Some(u) if u.kind == UserTypeKind::Clr => Facets { size: -1, ..Default::default() },
            _ => Facets::default(),
        };
        f.sql_type = -1;
        f.type_name = t.name.clone();
        f.type_schema = schema.clone();
        f.udt = Some(qualified(schema, &t.name));
        return f;
    }
    let name = t.name.to_ascii_lowercase();
    let length = t.length.unwrap_or(1);
    let fractional = |default: u8| i32::from(t.scale.unwrap_or(default));
    let (sql_type, size, precision, scale) = match name.as_str() {
        "binary" => (1, length, 0, 0),
        "varbinary" => (2, length, 0, 0),
        "char" => (3, length, 0, 0),
        "varchar" => (4, length, 0, 0),
        "nchar" => (5, length, 0, 0),
        "nvarchar" => (6, length, 0, 0),
        "sysname" => (6, 128, 0, 0),
        "datetime" => (7, 8, 23, 3),
        "smalldatetime" => (8, 4, 16, 0),
        "decimal" | "numeric" => {
            let p = i32::from(t.precision.unwrap_or(18));
            let size = match p {
                ..=9 => 5,
                10..=19 => 9,
                20..=28 => 13,
                _ => 17,
            };
            (if name == "decimal" { 9 } else { 10 }, size, p, i32::from(t.scale.unwrap_or(0)))
        }
        "float" => {
            let p = i32::from(t.precision.unwrap_or(53));
            (11, if p <= 24 { 4 } else { 8 }, if p <= 24 { 24 } else { 53 }, 0)
        }
        "real" => (12, 4, 24, 0),
        "bigint" => (13, 8, 19, 0),
        "int" => (14, 4, 10, 0),
        "smallint" => (15, 2, 5, 0),
        "tinyint" => (16, 1, 3, 0),
        "money" => (17, 8, 19, 4),
        "smallmoney" => (18, 4, 10, 4),
        "bit" => (19, 1, 1, 0),
        "timestamp" | "rowversion" => (20, 8, 0, 0),
        "uniqueidentifier" => (21, 16, 0, 0),
        "image" => (22, 16, 0, 0),
        "ntext" => (23, 16, 0, 0),
        "text" => (24, 16, 0, 0),
        "sql_variant" => (25, 8016, 0, 0),
        "xml" => (26, -1, 0, 0),
        "date" => (27, 3, 10, 0),
        "time" => {
            let s = fractional(7);
            (
                28,
                if s <= 2 {
                    3
                } else if s <= 4 {
                    4
                } else {
                    5
                },
                if s == 0 { 8 } else { 9 + s },
                s,
            )
        }
        "datetimeoffset" => {
            let s = fractional(7);
            (
                29,
                if s <= 2 {
                    8
                } else if s <= 4 {
                    9
                } else {
                    10
                },
                if s == 0 { 26 } else { 27 + s },
                s,
            )
        }
        "datetime2" => {
            let s = fractional(7);
            (
                30,
                if s <= 2 {
                    6
                } else if s <= 4 {
                    7
                } else {
                    8
                },
                if s == 0 { 19 } else { 20 + s },
                s,
            )
        }
        "hierarchyid" => (-2, 892, 0, 0),
        "geometry" | "geography" => (-2, -1, 0, 0),
        _ => (-1, 0, 0, 0),
    };
    Facets {
        sql_type,
        size,
        precision,
        scale,
        type_name: t.name.clone(),
        type_schema: "sys".into(),
        udt: None,
        character: matches!(name.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname"),
    }
}

/// Columns of `columns` an expression refers to, in order of first use.
fn referenced_columns(expr: &str, columns: &[Column]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tokenize(expr) {
        let name = match t.kind {
            Tok::Quoted => t.text[1..t.text.len().saturating_sub(1).max(1)].replace("]]", "]"),
            Tok::Word => t.text.clone(),
            _ => continue,
        };
        if let Some(c) = columns.iter().find(|c| c.name.eq_ignore_ascii_case(&name))
            && !out.contains(&c.name)
        {
            out.push(c.name.clone());
        }
    }
    out
}

/// A DML trigger's text with its own name and its table bracketed, as SQL Compare stores it.
fn trigger_text(definition: &str, schema: &str, name: &str, on: &str) -> String {
    let text = module_sql(definition, "CREATE", &qualified(schema, name));
    let tokens = tokenize(&text);
    let sig: Vec<usize> = (0..tokens.len()).filter(|&i| !matches!(tokens[i].kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).collect();
    let Some(p) = sig.iter().position(|&i| tokens[i].kind == Tok::Word && tokens[i].text.eq_ignore_ascii_case("ON")) else { return text };
    let first = p + 1;
    let mut last = first;
    while sig.get(last + 1).is_some_and(|&i| tokens[i].text == ".") && sig.get(last + 2).is_some() {
        last += 2;
    }
    let (Some(&a), Some(&b)) = (sig.get(first), sig.get(last)) else { return text };
    let mut out = String::with_capacity(text.len());
    for (i, t) in tokens.iter().enumerate() {
        if i == a {
            out.push_str(on);
        } else if i <= a || i > b {
            out.push_str(&t.text);
        }
    }
    out
}

/// The local object a synonym points at, when it is in this database.
fn synonym_target(base: &str, database: &str) -> Option<ObjectName> {
    match split_multipart(base).as_slice() {
        [schema, name] => Some(ObjectName::new(schema, name)),
        [db, schema, name] if db.eq_ignore_ascii_case(database) => Some(ObjectName::new(schema, name)),
        _ => None,
    }
}

/// Splits `[a].[b].c` into its unquoted parts.
fn split_multipart(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_bracket = false;
    while let Some(c) = chars.next() {
        match c {
            '[' if !in_bracket => in_bracket = true,
            ']' if in_bracket => {
                if chars.peek() == Some(&']') {
                    chars.next();
                    cur.push(']');
                } else {
                    in_bracket = false;
                }
            }
            '.' if !in_bracket => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::snp;

    #[test]
    fn strings() {
        let mut b = Buf::default();
        b.s("SQLCOMPARE");
        b.str(None);
        b.s("");
        assert_eq!(&b.b[..2], &[1, 10]);
        assert_eq!(&b.b[12..], &[0, 1, 0]);
        let mut b = Buf::default();
        b.s(&"x".repeat(300));
        assert_eq!(&b.b[..3], &[1, 0xAC, 0x02]);
        let mut b = Buf::default();
        b.s("é");
        assert_eq!(b.b, [1, 2, 0xC3, 0xA9]);
    }

    #[test]
    fn numbers_and_collections() {
        let mut b = Buf::default();
        b.f32(23.0);
        b.f32(4.0);
        b.i16(-2);
        b.i32(16);
        b.u64(u64::MAX);
        assert_eq!(b.b, [0, 0, 0xB8, 0x41, 0, 0, 0x80, 0x40, 0xFE, 0xFF, 16, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let mut b = Buf::default();
        b.coll_head(false, 0);
        assert_eq!(b.b, [0, 0, 0x80, 0x40, 0, 0, 0, 0, 0]);
        let mut b = Buf::default();
        b.strs(&["a"]);
        b.no_ss(2);
        assert_eq!(b.b, [1, 0, 0, 0, 1, 1, b'a', 0, 0]);
    }

    #[test]
    fn type_facets() {
        let db = Database::default();
        let t = |name: &str, length, precision, scale| DataType { name: name.into(), length, precision, scale, ..Default::default() };
        let f = facets(&db, &t("decimal", None, Some(18), Some(2)));
        assert_eq!((f.sql_type, f.size, f.precision, f.scale), (9, 9, 18, 2));
        let f = facets(&db, &t("datetime2", None, None, Some(3)));
        assert_eq!((f.sql_type, f.size, f.precision, f.scale), (30, 7, 23, 3));
        let f = facets(&db, &t("nvarchar", Some(-1), None, None));
        assert_eq!((f.sql_type, f.size, f.character), (6, -1, true));
        let f = facets(&db, &t("money", None, None, None));
        assert_eq!((f.sql_type, f.size, f.precision, f.scale), (17, 8, 19, 4));
    }

    #[test]
    fn texts() {
        assert_eq!(
            trigger_text("CREATE TRIGGER Sales.Order_Audit ON Sales.[Order] AFTER INSERT AS SELECT 1", "Sales", "Order_Audit", "[Sales].[Order]"),
            "CREATE TRIGGER [Sales].[Order_Audit] ON [Sales].[Order] AFTER INSERT AS SELECT 1"
        );
        assert_eq!(module_sql("CREATE VIEW Sales.V AS SELECT 1", "CREATE", "[Sales].[V]"), "CREATE VIEW [Sales].[V] AS SELECT 1");
        // SQL Compare's printed form is stable when printed again.
        for s in ["([Balance]>=(-100))", "([Status]='S' OR [Status]='P' OR [Status]='N')", "(len([Name]))"] {
            let once = super::super::expr::rewrite(s);
            assert_eq!(super::super::expr::rewrite(&once), once);
        }
        let cols = |names: &[&str]| names.iter().map(|n| Column { name: n.to_string(), ..Default::default() }).collect::<Vec<_>>();
        assert_eq!(referenced_columns("([a]+[B]*(2))>[a]", &cols(&["A", "b", "c"])), ["A", "b"]);
        assert_eq!(split_retention("6 MONTHS"), Some((6, "MONTH".into())));
    }

    #[test]
    fn empty_database_round_trip() {
        let db = Database { name: "x".into(), collation: "SQL_Latin1_General_CP1_CI_AS".into(), compatibility_level: 160, ..Default::default() };
        let bytes = write(&db);
        assert!(snp::is_snp(&bytes));
        let back = snp::read(&bytes).unwrap();
        assert_eq!(back.name, "x");
        assert_eq!(back.compatibility_level, 160);
        assert!(back.tables.is_empty() && back.principals.is_empty());
        assert_eq!(back.schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["dbo"]);
    }

    /// Reads, writes and reads again the snapshots SQL Compare made (rg_shop.snp, rg_shop2.snp,
    /// rg_xml.snp) when QUILL_TEST_SNP_DIR names the folder holding them.
    #[test]
    fn round_trips_sample_snapshots() {
        let Ok(dir) = std::env::var("QUILL_TEST_SNP_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        for name in ["rg_shop.snp", "rg_shop2.snp", "rg_xml.snp"] {
            let path = dir.join(name);
            let Ok(original) = std::fs::read(&path) else { continue };
            let db = snp::read(&original).unwrap();
            let bytes = write(&db);
            let back = snp::read(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(back, db, "{name}");
            // Writing is stable: the same model gives the same bytes after the timestamp.
            assert_eq!(write(&back)[24..], bytes[24..], "{name}");
            if let Ok(out) = std::env::var("QUILL_TEST_SNP_OUT") {
                std::fs::write(std::path::Path::new(&out).join(name), &bytes).unwrap();
            }
        }
    }
}
