//! SQL Compare snapshot files (`.snp`, format version 23). The file is one little-endian stream
//! of .NET `BinaryWriter` values with no framing or offsets, so every member is parsed in order
//! to reach the next one; members the model has no place for are read and dropped.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::catalog::*;
use crate::db::quote;

const MAGIC: &str = "SQLCOMPARE";
const VERSION: f32 = 23.0;
/// U+FFFF in UTF-8: written in place of a top-level member that is null.
const NULL_MEMBER: [u8; 3] = [0xEF, 0xBF, 0xBF];
/// Bound on the recursive records (script sources, nested tables) so bad input cannot overflow the stack.
const MAX_DEPTH: u32 = 64;

const FIXED_ROLES: [&str; 10] = [
    "public",
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
pub(crate) mod objects;

const BUILT_IN_USERS: [&str; 4] = ["dbo", "guest", "sys", "INFORMATION_SCHEMA"];
/// Grantees whose permissions the live reader leaves out.
const SYSTEM_GRANTEES: [&str; 3] = ["dbo", "sys", "INFORMATION_SCHEMA"];

// ObjectType codes used in the dependency list.
const OT_TABLE: u8 = 1;
const OT_PROCEDURE: u8 = 2;
const OT_VIEW: u8 = 3;
const OT_FUNCTION: u8 = 6;
const OT_TRIGGER: u8 = 11;
const OT_SYNONYM: u8 = 14;
const OT_CHECK: u8 = 31;
const OT_SEQUENCE: u8 = 35;

type R<T> = Result<T, String>;
/// A blob store: contents by id, and the ids of encrypted modules.
type Blobs = (HashMap<u64, Vec<u8>>, HashSet<u64>);

/// True when `bytes` starts with the snapshot header.
pub fn is_snp(bytes: &[u8]) -> bool {
    matches!(Cur::new(bytes).str(), Ok(Some(m)) if m == MAGIC)
}

/// Reads a snapshot into the model.
pub fn read(bytes: &[u8]) -> Result<Database, String> {
    let mut c = Cur::new(bytes);
    if !matches!(c.str(), Ok(Some(m)) if m == MAGIC) {
        return Err("not a SQL Compare snapshot".into());
    }
    let version = c.f32()?;
    if version != VERSION {
        return Err(format!("snapshot version {version} is not supported"));
    }
    let mut s = Snap::default();
    c.f64()?; // saved at
    s.name = c.s()?;
    c.str()?; // server name
    s.compatibility_level = u32::try_from(c.i32()?).unwrap_or(0).saturating_mul(10);
    s.server.major_version = u32::try_from(c.i32()?).unwrap_or(0);
    s.server.product_version = c.s()?;
    s.server.engine_edition = u32::try_from(c.i32()?).unwrap_or(0);
    if c.bool()? {
        c.str()?; // SQL Compare version
    }
    c.str()?; // scripts folder
    c.str()?; // migrations folder

    s.tables = c.member(Cur::table_item)?;
    s.tables.append(&mut c.histories);
    s.functions = c.member(Cur::function)?;
    s.more.defaults = c.member(|c| c.bindable(false))?;
    s.more.rules = c.member(|c| c.bindable(true))?;
    s.users = c.member(Cur::user)?;
    s.roles = c.member(Cur::role)?;
    s.types = c.member(Cur::user_type)?;
    s.views = c.member(Cur::view)?;
    s.procedures = c.member(Cur::procedure)?;
    if !c.null_member() {
        s.dependencies = c.dependencies()?;
    }
    if !c.null_member() {
        c.eprops()?;
    }
    c.bool()?; // full-text enabled
    c.bool()?; // remote data archive enabled
    s.more.fulltext_catalogs = c.member(Cur::fulltext_catalog_full)?;
    s.case_sensitive = c.bool()?;
    if !c.null_member() {
        (s.texts, s.encrypted) = c.blob_store()?;
    }
    // Assembly binaries and XML schema collection contents.
    if !c.null_member() {
        s.more.assembly_blobs = c.blob_store()?.0;
    }
    if !c.null_member() {
        s.more.xml_blobs = c.blob_store()?.0;
    }
    s.more.assemblies = c.member(Cur::assembly_full)?;
    if !c.null_member() {
        s.more.assembly_references = c.dependencies()?;
    }
    s.synonyms = c.member(Cur::synonym)?;
    s.more.xml_collections = c.member(Cur::xml_collection_full)?;
    s.more.message_types = c.member(Cur::message_type_full)?.into_iter().flatten().collect();
    s.more.contracts = c.member(Cur::contract_full)?.into_iter().flatten().collect();
    s.more.queues = c.member(Cur::queue_full)?.into_iter().flatten().collect();
    s.more.services = c.member(Cur::service_full)?.into_iter().flatten().collect();
    s.more.routes = c.member(Cur::route_full)?.into_iter().flatten().collect();
    s.more.event_notifications = c.member(Cur::event_notification_full)?;
    s.more.partition_functions = c.member(Cur::partition_function_full)?;
    s.more.partition_schemes = c.member(Cur::partition_scheme_full)?;
    s.schemas = c.member(Cur::schema)?;
    c.member(Cur::trigger)?; // database-level list; DML triggers live in their table or view
    s.more.ddl_triggers = c.member(Cur::ddl_trigger_full)?;
    s.more.bindings = c.member(Cur::service_binding_full)?;
    s.more.certificates = c.member(Cur::certificate_full)?;
    s.more.symmetric_keys = c.member(Cur::symmetric_key_full)?.into_iter().flatten().collect();
    s.more.asymmetric_keys = c.member(Cur::asymmetric_key_full)?;
    s.more.stoplists = c.member(Cur::stoplist_full)?;
    s.sequences = c.member(Cur::sequence)?;
    s.more.property_lists = c.member(Cur::property_list_full)?;
    s.more.policies = c.member(Cur::security_policy_full)?;
    s.more.file_formats = c.coll(Cur::file_format_full)?;
    s.more.data_sources = c.coll(Cur::data_source_full)?;
    s.more.external_tables = c.coll(Cur::external_table_full)?;
    s.more.credentials = c.coll(Cur::s)?;
    if c.p != bytes.len() {
        return Err(format!("unexpected data at offset {} after the last snapshot member", c.p));
    }
    s.props = std::mem::take(&mut c.props);
    s.type_perms = std::mem::take(&mut c.type_perms);
    Ok(s.build())
}

// ------------------------------------------------------------------------------- raw records

/// What the file holds beyond the model objects themselves.
#[derive(Default)]
struct Snap {
    name: String,
    server: ServerInfo,
    compatibility_level: u32,
    case_sensitive: bool,
    tables: Vec<RawTable>,
    functions: Vec<RawModule>,
    procedures: Vec<RawModule>,
    views: Vec<RawView>,
    types: Vec<UserType>,
    users: Vec<RawUser>,
    roles: Vec<RawRole>,
    schemas: Vec<RawSchema>,
    sequences: Vec<(Sequence, Vec<RawPerm>)>,
    synonyms: Vec<(Synonym, Vec<RawPerm>)>,
    dependencies: Vec<(u8, String, u8, String)>,
    props: Vec<RawProp>,
    /// Comments blob store: module and default text by id.
    texts: HashMap<u64, Vec<u8>>,
    encrypted: HashSet<u64>,
    more: objects::More,
    type_perms: Vec<(String, String, Vec<RawPerm>)>,
}

struct RawPerm {
    /// 205 GRANT, 206 DENY, 1 GRANT WITH GRANT OPTION, others revoke.
    statement: i32,
    action: String,
    grantee: String,
    /// An old enum action: not modelled.
    skip: bool,
    /// Column-level permissions: the columns.
    columns: Vec<String>,
}

struct RawProp {
    types: [Option<String>; 3],
    names: [Option<String>; 3],
    name: String,
    value: Option<String>,
}

#[derive(Default)]
struct RawField {
    name: String,
    size: i32,
    scale: i32,
    precision: i32,
    seed: String,
    increment: i64,
    flags: i16,
    default_name: String,
    default_system_named: bool,
    computed: String,
    collation: String,
    type_name: String,
    default_text: String,
    type_schema: String,
    xml_collection: Option<String>,
    masking: String,
    generated: u8,
    hidden: bool,
    bound_default: Option<String>,
    bound_rule: Option<String>,
    sensitivity: Option<Sensitivity>,
}

#[derive(Default)]
struct RawTable {
    name: String,
    schema: String,
    modified: Option<String>,
    perms: Vec<RawPerm>,
    indexes: Vec<Index>,
    foreign_keys: Vec<ForeignKey>,
    checks: Vec<CheckConstraint>,
    triggers: Vec<RawTrigger>,
    columns: Vec<RawField>,
    filegroup: Option<String>,
    text_filegroup: Option<String>,
    partition_column: Option<String>,
    partition_scheme: Option<String>,
    filestream_filegroup: Option<String>,
    compression: Vec<PartitionCompression>,
    lock_escalation: u8,
    memory_optimized: bool,
    durability: u8,
    change_tracking: u8,
    temporal_type: u8,
    history: Option<ObjectName>,
    retention: Option<String>,
    is_node: bool,
    is_edge: bool,
    statistics: Vec<Statistic>,
    edge_constraints: Vec<EdgeConstraint>,
}

struct RawTrigger {
    trigger: Trigger,
    text_id: i32,
}

struct RawModule {
    routine: Routine,
    text_id: i32,
    perms: Vec<RawPerm>,
}

struct RawView {
    view: View,
    text_id: i32,
    perms: Vec<RawPerm>,
    triggers: Vec<RawTrigger>,
}

struct RawUser {
    name: String,
    login: Option<String>,
    user_type: i32,
    default_schema: Option<String>,
    certificate: Option<String>,
    asymmetric_key: Option<String>,
    db_perms: Vec<(String, u8)>,
}

struct RawRole {
    name: String,
    owner: Option<String>,
    app_role: bool,
    members: Vec<String>,
    db_perms: Vec<(String, u8)>,
}

struct RawSchema {
    name: String,
    owner: String,
    system_object: bool,
    perms: Vec<RawPerm>,
}

// ------------------------------------------------------------------------------- primitives

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
    depth: u32,
    /// Every extended property met so far; they are attached by their level names, not their owner.
    props: Vec<RawProp>,
    /// Statistics met while reading a table's indexes.
    stats: Vec<Statistic>,
    /// Permissions on user-defined types: (schema, name, perms).
    type_perms: Vec<(String, String, Vec<RawPerm>)>,
    /// History tables, which are written inside their system-versioned table.
    histories: Vec<RawTable>,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, p: 0, depth: 0, props: Vec::new(), stats: Vec::new(), type_perms: Vec::new(), histories: Vec::new() }
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| format!("truncated snapshot at offset {}", self.p))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }

    fn bytes<const N: usize>(&mut self) -> R<[u8; N]> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.bytes::<1>()?[0])
    }

    fn i8(&mut self) -> R<i8> {
        Ok(i8::from_le_bytes(self.bytes()?))
    }

    fn bool(&mut self) -> R<bool> {
        Ok(self.u8()? != 0)
    }

    fn i16(&mut self) -> R<i16> {
        Ok(i16::from_le_bytes(self.bytes()?))
    }

    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }

    fn i64(&mut self) -> R<i64> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }

    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.bytes()?))
    }

    fn f64(&mut self) -> R<f64> {
        Ok(f64::from_le_bytes(self.bytes()?))
    }

    /// An i32 item count; every item takes at least a byte, so a count past the end is corrupt.
    fn count(&mut self) -> R<usize> {
        let at = self.p;
        let n = self.i32()?;
        usize::try_from(n).ok().filter(|&n| n <= self.b.len() - self.p).ok_or_else(|| format!("bad count {n} at offset {at}"))
    }

    /// A count where negative means none.
    fn count_or_none(&mut self) -> R<usize> {
        let at = self.p;
        let n = self.i32()?.max(0);
        usize::try_from(n).ok().filter(|&n| n <= self.b.len() - self.p).ok_or_else(|| format!("bad count {n} at offset {at}"))
    }

    /// .NET 7-bit encoded length.
    fn len7(&mut self) -> R<usize> {
        let at = self.p;
        let mut n = 0usize;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            n |= usize::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(format!("bad string length at offset {at}"))
    }

    /// Nullable string: 0 = null, 1 = a length-prefixed UTF-8 string follows.
    fn str(&mut self) -> R<Option<String>> {
        let at = self.p;
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let n = self.len7()?;
                let b = self.take(n)?;
                String::from_utf8(b.to_vec()).map(Some).map_err(|_| format!("invalid UTF-8 string at offset {at}"))
            }
            f => Err(format!("bad string marker {f} at offset {at}")),
        }
    }

    /// A string with null read as empty.
    fn s(&mut self) -> R<String> {
        Ok(self.str()?.unwrap_or_default())
    }

    fn strs(&mut self) -> R<Vec<String>> {
        let n = self.count()?;
        self.list(n, Cur::s)
    }

    fn list<T>(&mut self, n: usize, mut f: impl FnMut(&mut Self) -> R<T>) -> R<Vec<T>> {
        (0..n).map(|_| f(self)).collect()
    }

    /// Keyed collection: format version, case-sensitivity, count, items.
    fn coll<T>(&mut self, f: impl FnMut(&mut Self) -> R<T>) -> R<Vec<T>> {
        self.f32()?;
        self.bool()?;
        let n = self.count()?;
        self.list(n, f)
    }

    /// Skips the null-member marker; true when it was there.
    fn null_member(&mut self) -> bool {
        let hit = self.b.get(self.p..self.p.saturating_add(3)) == Some(&NULL_MEMBER[..]);
        if hit {
            self.p += 3;
        }
        hit
    }

    /// A top-level collection, which may be null.
    fn member<T>(&mut self, f: impl FnMut(&mut Self) -> R<T>) -> R<Vec<T>> {
        if self.null_member() { Ok(Vec::new()) } else { self.coll(f) }
    }

    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        if self.depth >= MAX_DEPTH {
            return Err(format!("snapshot nesting too deep at offset {}", self.p));
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// Script-folder provenance; null in snapshots of live databases.
    fn ss(&mut self) -> R<()> {
        if self.u8()? == 0 {
            return Ok(());
        }
        self.nested(|c| {
            let v = c.f32()?;
            c.str()?;
            c.i32()?;
            c.i32()?;
            if v >= 18.0 {
                c.ss_list()?;
            } else {
                c.sss(2)?;
            }
            c.ss_list()
        })
    }

    fn sss(&mut self, n: usize) -> R<()> {
        for _ in 0..n {
            self.ss()?;
        }
        Ok(())
    }

    fn ss_list(&mut self) -> R<()> {
        let n = self.count()?;
        self.sss(n)
    }

    fn ss_dict(&mut self) -> R<()> {
        let n = self.count_or_none()?;
        for _ in 0..n {
            self.str()?;
            self.ss()?;
        }
        Ok(())
    }

    fn perms(&mut self) -> R<Vec<RawPerm>> {
        self.f32()?;
        let n = self.count()?;
        self.list(n, |c| {
            let v = c.f32()?;
            let statement = c.i32()?;
            let action = if v < 12.48 { c.i32()?.to_string() } else { c.s()? };
            let grantee = c.s()?;
            c.bool()?;
            let columns = if c.bool()? { c.strs()? } else { Vec::new() };
            Ok(RawPerm { statement, action, grantee, skip: v < 12.48, columns })
        })
    }

    /// Database-level permissions of a principal: (action, statement).
    fn db_perms(&mut self) -> R<Vec<(String, u8)>> {
        let v = self.f32()?;
        self.str()?;
        let mut out = Vec::new();
        if v < 12.48 {
            let n = self.count()?;
            self.take(n)?;
        } else {
            let n = self.count()?;
            out = self.list(n, |c| Ok((c.s()?, c.u8()?)))?;
        }
        if v >= 11.46 {
            self.ss_list()?;
        }
        Ok(out)
    }

    fn eprops(&mut self) -> R<()> {
        let props = self.coll(Cur::eprop)?;
        self.props.extend(props);
        Ok(())
    }

    fn eprop(&mut self) -> R<RawProp> {
        let v = self.f32()?;
        let types = [self.str()?, self.str()?, self.str()?];
        let names = [self.str()?, self.str()?, self.str()?];
        let value = self.prop_value(v)?;
        let name = self.s()?;
        if v >= 11.45 {
            self.ss()?;
        }
        if v >= 22.0 {
            self.sss(2)?;
        }
        Ok(RawProp { types, names, name, value })
    }

    /// A sql_variant value, as text.
    fn prop_value(&mut self, v: f32) -> R<Option<String>> {
        if v >= 9.1 {
            self.i32()?;
            self.i32()?;
            self.i32()?;
            self.str()?;
            self.i8()?;
        }
        let at = self.p;
        Ok(match self.u8()? {
            0 => None,
            1 | 14 => self.str()?,
            2 => Some(self.i32()?.to_string()),
            3 => Some(self.i64()?.to_string()),
            4 => Some(u8::from(self.bool()?).to_string()),
            5 => Some(oa_date(self.f64()?)),
            6 => Some(decimal(self.bytes()?)),
            7 => Some(self.f64()?.to_string()),
            8 => Some(self.f32()?.to_string()),
            9 => Some(self.i16()?.to_string()),
            10 => Some(self.u8()?.to_string()),
            11 => {
                let n = self.count()?;
                Some(format!("0x{}", self.take(n)?.iter().map(|b| format!("{b:02X}")).collect::<String>()))
            }
            12 => Some(guid(self.bytes()?)),
            13 => {
                self.i8()?;
                self.i8()?;
                self.str()?
            }
            t => return Err(format!("bad extended property value tag {t} at offset {at}")),
        })
    }

    /// EXECUTE AS: 0 CALLER, 1 OWNER, 2 SELF, 3 GUEST, 4 a user name.
    fn execute_as(&mut self) -> R<Option<String>> {
        let at = self.p;
        Ok(match self.i32()? {
            0 => None,
            1 => Some("OWNER".into()),
            2 => Some("dbo".into()),
            3 => Some("guest".into()),
            4 => self.str()?,
            n => return Err(format!("bad EXECUTE AS code {n} at offset {at}")),
        })
    }

    /// Objects referenced by a module body.
    fn references(&mut self) -> R<()> {
        let n = self.count()?;
        for _ in 0..n {
            self.u8()?;
            self.str()?;
        }
        Ok(())
    }

    fn compression(&mut self) -> R<Vec<PartitionCompression>> {
        let n = self.count()?;
        let list = self.list(n, |c| {
            let partition = u32::try_from(c.i32()?).unwrap_or(0);
            let at = c.p;
            let compression = match c.u8()? {
                0 => "NONE",
                1 => "ROW",
                2 => "PAGE",
                3 => "COLUMNSTORE",
                4 => "COLUMNSTORE_ARCHIVE",
                k => return Err(format!("bad compression kind {k} at offset {at}")),
            };
            Ok(PartitionCompression { partition, compression: compression.into() })
        })?;
        Ok(collapse(list))
    }

    fn blob_store(&mut self) -> R<Blobs> {
        let v = self.f32()?;
        let n = self.count()?;
        let mut blobs = HashMap::new();
        for _ in 0..n {
            let id = if v < 1.1 { self.i32()? as i64 as u64 } else { self.u64()? };
            let size = self.count()?;
            blobs.insert(id, self.take(size)?.to_vec());
        }
        let mut undecryptable = HashSet::new();
        if v >= 7.0 {
            let n = self.count()?;
            for _ in 0..n {
                undecryptable.insert(self.u64()?);
            }
        }
        Ok((blobs, undecryptable))
    }

    fn dependencies(&mut self) -> R<Vec<(u8, String, u8, String)>> {
        let v = self.f32()?;
        let n = self.count()?;
        self.list(n, |c| {
            let d = (c.u8()?, c.s()?, c.u8()?, c.s()?);
            if v >= 1.1 {
                c.bool()?;
            }
            Ok(d)
        })
    }
}

// ------------------------------------------------------------------------------- tables

impl Cur<'_> {
    fn field(&mut self) -> R<RawField> {
        self.nested(Cur::field_body)
    }

    fn field_body(&mut self) -> R<RawField> {
        let v = self.f32()?;
        self.i32()?; // SqlType; type_name and type_schema are authoritative
        let mut flags = 0i16;
        if v < 6.0 {
            for bit in [0x1, 0x2, 0x4] {
                if self.bool()? {
                    flags |= bit;
                }
            }
        }
        let mut f = RawField { name: self.s()?, size: self.i32()?, ..Default::default() };
        if v < 6.0 {
            self.bool()?;
        }
        f.scale = self.i32()?;
        f.precision = self.i32()?;
        if v < 6.0 && self.bool()? {
            flags |= 0x10;
        }
        f.seed = if v < 4.0 { self.i64()?.to_string() } else { self.s()? };
        if v >= 5.0 {
            self.str()?; // current identity value
        }
        f.increment = self.i64()?;
        if v >= 6.0 {
            flags = self.i16()?;
        }
        f.bound_default = self.str()?.filter(|x| !x.is_empty());
        f.default_name = self.s()?;
        if v >= 4.0 {
            self.i32()?;
        }
        if v >= 11.0 {
            f.default_system_named = self.bool()?;
        }
        f.computed = self.s()?;
        if v < 6.0 && self.bool()? {
            flags |= 0x20;
        }
        f.collation = self.s()?;
        f.bound_rule = self.str()?.filter(|x| !x.is_empty());
        f.type_name = self.s()?;
        f.default_text = self.s()?;
        if v < 6.0 {
            self.bool()?;
        }
        self.i32()?; // full-text language
        if v >= 4.0 {
            self.str()?;
            f.type_schema = self.s()?;
            if v < 6.0 {
                self.bool()?;
                if self.bool()? {
                    flags |= 0x100;
                }
                if self.bool()? {
                    flags |= 0x200;
                }
            }
            f.xml_collection = self.str()?;
            if v >= 11.35 {
                f.masking = self.s()?;
            }
            let n = self.count_or_none()?;
            for _ in 0..n {
                self.str()?;
            }
        } else {
            f.type_schema = "sys".into();
        }
        if v >= 12.2 {
            f.generated = self.u8()?;
            f.hidden = self.bool()?;
        }
        if v >= 13.02 {
            self.u8()?;
        }
        if v >= 13.43 {
            self.sss(3)?;
        }
        if v >= 13.44 {
            let sv = self.f32()?;
            let parts: Vec<Option<String>> = (0..4).map(|_| self.str().map(|s| s.filter(|x| !x.is_empty()))).collect::<R<_>>()?;
            if parts.iter().any(Option::is_some) {
                f.sensitivity = Some(Sensitivity {
                    label: parts[0].clone(),
                    label_id: parts[1].clone(),
                    information_type: parts[2].clone(),
                    information_type_id: parts[3].clone(),
                    rank: None,
                });
            }
            if sv >= 13.46 {
                self.sss(5)?;
            }
        }
        if v >= 13.63 {
            self.str()?; // qualified user type
        }
        f.flags = flags;
        Ok(f)
    }

    /// An index or key; `None` for statistics.
    fn index(&mut self) -> R<Option<Index>> {
        let at = self.p;
        let v = self.f32()?;
        if v < 7.5 {
            return Err(format!("index record version {v} at offset {at} is not supported"));
        }
        let kind = self.u8()?;
        self.bool()?;
        let name = self.s()?;
        let n = self.count()?;
        let cols = self.list(n, |c| {
            let col = (c.s()?, c.bool()?, c.bool()?);
            let id = if v >= 17.0 { c.i32()? } else { 0 };
            Ok((id, col))
        })?;
        // The records are in key order; the id is the column's index_column_id, which can differ
        // (SQL Server numbers a history table's (ValidTo, ValidFrom) index by column order).
        let flags = self.i16()?;
        let filegroup = self.str()?;
        let fill_factor = self.u8()?;
        if v >= 10.2 {
            self.i32()?;
        }
        let system_named = v >= 10.3 && self.bool()?;
        if v >= 11.45 {
            self.ss()?;
        }
        if v >= 13.23 {
            self.ss()?;
        }
        if v >= 20.0 {
            self.ss()?;
        }
        if v >= 13.68 {
            self.bool()?;
        }
        let flag = |b: i16| flags & b != 0;
        let mut ix = Index {
            name,
            system_named,
            fill_factor,
            ignore_dup_key: flag(0x1),
            no_recompute: flag(0x2),
            allow_row_locks: flag(0x4),
            allow_page_locks: flag(0x8),
            pad_index: flag(0x10),
            disabled: flag(0x40),
            incremental: flag(0x100),
            optimize_for_sequential_key: flag(0x800),
            storage: Storage { data_space: filegroup, ..Default::default() },
            ..Default::default()
        };
        match kind {
            0 | 4 => {
                let tv = self.f32()?;
                ix.primary_key = self.bool()?;
                let clustered = self.bool()?;
                ix.unique = self.bool()?;
                ix.unique_constraint = self.bool()?;
                self.bool()?; // full-text key
                if flag(0x20) && self.bool()? {
                    ix.storage.partition_column = Some(self.field()?.name);
                }
                ix.filter = self.str()?.filter(|f| !f.is_empty());
                ix.storage.compression = self.compression()?;
                let columnstore = kind == 4 || (tv >= 10.2 && self.bool()?);
                let (hash, buckets) = if tv >= 11.0 { (self.bool()?, self.i32()?) } else { (false, 0) };
                ix.kind = match (columnstore, clustered, hash) {
                    (true, true, _) => IndexKind::ClusteredColumnstore,
                    (true, false, _) => IndexKind::NonclusteredColumnstore,
                    (false, _, true) => IndexKind::NonclusteredHash,
                    (false, true, false) => IndexKind::Clustered,
                    (false, false, false) => IndexKind::Nonclustered,
                };
                if hash {
                    ix.bucket_count = Some(i64::from(buckets));
                }
            }
            1 => {
                self.f32()?;
                let filter = self.str()?.filter(|f| !f.is_empty());
                let columns = cols.into_iter().map(|(_, (name, _, _))| name).collect();
                self.stats.push(Statistic { name: ix.name, columns, filter, no_recompute: ix.no_recompute, incremental: ix.incremental });
                return Ok(None);
            }
            2 => {
                ix.kind = IndexKind::Xml;
                let tv = self.f32()?;
                let mut x = XmlIndex {
                    secondary_type: match self.i32()? {
                        1 => Some("VALUE".into()),
                        2 => Some("PATH".into()),
                        3 => Some("PROPERTY".into()),
                        _ => None,
                    },
                    using_index: self.str()?.filter(|s| !s.is_empty()),
                    ..Default::default()
                };
                x.kind = if x.using_index.is_some() { XmlIndexKind::Secondary } else { XmlIndexKind::Primary };
                if tv >= 11.1 {
                    x.kind = match self.u8()? {
                        1 => XmlIndexKind::Secondary,
                        2 => XmlIndexKind::Selective,
                        3 => XmlIndexKind::SecondarySelective,
                        _ => XmlIndexKind::Primary,
                    };
                    let n = self.count()?;
                    for _ in 0..n {
                        let at = self.p;
                        let path_type = self.u8()?;
                        self.f32()?;
                        let mut p = SelectivePath { id: self.i32()?, name: self.str()?.unwrap_or_default(), path: self.str()?.unwrap_or_default(), ..Default::default() };
                        match path_type {
                            0 => {
                                p.xquery_type = self.str()?.filter(|s| !s.is_empty());
                                p.max_length = Some(self.i32()?).filter(|n| *n > 0);
                                p.node_hint = self.bool()?;
                                p.singleton = self.bool()?;
                            }
                            1 => {
                                p.sql = true;
                                let ty = self.i32()?;
                                let (size, precision, scale) = (self.i32()?, self.i32()?, self.i32()?);
                                p.data_type = sql_type_name(ty).map(|n| system_type(n, size, precision, scale));
                                p.singleton = self.bool()?;
                            }
                            t => return Err(format!("bad XML index path type {t} at offset {at}")),
                        }
                        x.paths.push(p);
                    }
                    let n = self.count()?;
                    for _ in 0..n {
                        self.f32()?;
                        let uri = self.str()?.unwrap_or_default();
                        let prefix = self.str()?.filter(|s| !s.is_empty());
                        x.namespaces.push(XmlNamespace { prefix, uri });
                    }
                    x.namespaces.sort_by(|a, b| a.prefix.cmp(&b.prefix));
                    x.using_path = self.str()?.filter(|s| !s.is_empty());
                }
                ix.xml = Some(x);
            }
            3 => {
                ix.kind = IndexKind::Spatial;
                let tv = self.f32()?;
                let geography = self.u8()? == 2;
                let tessellation = match self.u8()? {
                    1 => "GEOMETRY_GRID",
                    2 => "GEOGRAPHY_GRID",
                    3 => "GEOMETRY_AUTO_GRID",
                    _ => "GEOGRAPHY_AUTO_GRID",
                };
                // Stored as xmin, xmax, ymin, ymax.
                let b = [self.f64()?, self.f64()?, self.f64()?, self.f64()?];
                let mut grids = Vec::new();
                for _ in 0..4 {
                    grids.push(
                        match self.i16()? {
                            16 => "LOW",
                            256 => "HIGH",
                            _ => "MEDIUM",
                        }
                        .to_string(),
                    );
                }
                let cells_per_object = i32::from(self.i16()?);
                if tv >= 13.6915 {
                    self.i16()?;
                }
                let bounding_box = if geography { Vec::new() } else { [b[0], b[2], b[1], b[3]].iter().map(|f| f.to_string()).collect() };
                ix.spatial = Some(SpatialIndex { geography, tessellation: tessellation.into(), bounding_box, grids, cells_per_object });
            }
            k => return Err(format!("unknown index kind {k} at offset {at}")),
        }
        for (_, (name, descending, included)) in cols {
            if included {
                ix.included.push(name);
            } else if ix.kind != IndexKind::ClusteredColumnstore {
                // Columnstore columns have no sort order.
                let descending = descending && !matches!(ix.kind, IndexKind::NonclusteredColumnstore);
                ix.columns.push(IndexColumn { name, descending });
            }
        }
        Ok(Some(ix))
    }

    fn foreign_key(&mut self) -> R<ForeignKey> {
        let v = self.f32()?;
        let referenced = self.s()?;
        self.str()?;
        let mut fk = ForeignKey {
            referenced_table: two_part(&referenced).unwrap_or_default(),
            name: self.s()?,
            not_for_replication: self.bool()?,
            ..Default::default()
        };
        fk.on_update = referential_action(self.u8()?).into();
        fk.on_delete = referential_action(self.u8()?).into();
        fk.disabled = self.bool()?;
        self.bool()?;
        if v > 1.0 {
            fk.not_trusted = self.bool()?;
        }
        let n = self.count()?;
        for _ in 0..n {
            fk.columns.push(self.s()?);
            fk.referenced_columns.push(self.s()?);
        }
        if v >= 10.2 {
            self.i32()?;
        }
        if v >= 10.3 {
            fk.system_named = self.bool()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
        }
        if v >= 13.32 {
            self.eprops()?;
        }
        Ok(fk)
    }

    fn check_constraint(&mut self) -> R<CheckConstraint> {
        let v = self.f32()?;
        let mut ck = CheckConstraint { name: self.s()?, not_for_replication: self.bool()?, disabled: self.bool()?, ..Default::default() };
        if v >= 4.0 {
            self.i32()?;
        }
        ck.definition = self.s()?;
        if v > 1.0 {
            self.bool()?;
            if v > 1.1 {
                ck.not_trusted = self.bool()?;
            }
            // SQL Server ties a check to a column when it references only that column.
            let columns = self.strs()?;
            if let [column] = columns.as_slice() {
                ck.column = Some(column.clone());
            }
            if v >= 4.1 {
                ck.system_named = self.bool()?;
            }
            if v >= 11.45 {
                self.sss(2)?;
            }
        }
        Ok(ck)
    }

    fn trigger(&mut self) -> R<RawTrigger> {
        let v = self.f32()?;
        let qualified = self.s()?;
        let text_id = self.i32()?;
        self.bool()?; // quoted identifier
        self.bool()?; // ansi nulls
        let (update, insert, delete) = (self.bool()?, self.bool()?, self.bool()?);
        self.bool()?; // after
        let instead_of = self.bool()?;
        let name = two_part(&qualified).map(|n| n.name).unwrap_or(qualified);
        let mut t = Trigger { name, instead_of, ..Default::default() };
        for (on, event) in [(insert, "INSERT"), (update, "UPDATE"), (delete, "DELETE")] {
            if on {
                t.events.push(event.into());
            }
        }
        if v >= 1.01 {
            self.i32()?;
        }
        if v >= 4.0 {
            let mut f = [false; 6];
            for x in &mut f {
                *x = self.bool()?;
            }
            let [first_delete, first_insert, first_update, last_delete, last_insert, last_update] = f;
            for (event, first, last) in
                [("INSERT", first_insert, last_insert), ("UPDATE", first_update, last_update), ("DELETE", first_delete, last_delete)]
            {
                if first {
                    t.order.push((event.into(), "FIRST".into()));
                }
                if last {
                    t.order.push((event.into(), "LAST".into()));
                }
            }
            t.encrypted = self.bool()?;
            if v >= 7.2 {
                self.execute_as()?;
            } else {
                self.str()?;
            }
            for _ in 0..3 {
                self.str()?;
            }
            self.bool()?; // CLR
        }
        if v >= 4.1 {
            t.disabled = self.bool()?;
        }
        if v >= 7.0 {
            t.not_for_replication = self.bool()?;
        }
        if v >= 11.46 {
            self.sss(5)?;
        }
        if v >= 13.32 {
            self.bool()?;
            self.eprops()?;
        }
        Ok(RawTrigger { trigger: t, text_id })
    }

    fn table(&mut self) -> R<RawTable> {
        self.nested(Cur::table_body)
    }

    fn table_body(&mut self) -> R<RawTable> {
        let v = self.f32()?;
        let mut t = RawTable { name: self.s()?, schema: self.s()?, ..Default::default() };
        self.eprops()?;
        self.i32()?;
        t.perms = self.perms()?;
        let stats_from = self.stats.len();
        t.indexes = self.coll(Cur::index)?.into_iter().flatten().collect();
        t.statistics = self.stats.split_off(stats_from);
        t.statistics.sort_by(|a, b| a.name.cmp(&b.name));
        t.foreign_keys = self.coll(Cur::foreign_key)?;
        t.checks = self.coll(Cur::check_constraint)?;
        t.triggers = self.coll(Cur::trigger)?;
        let fulltext = self.bool()?;
        self.bool()?;
        t.columns = self.coll(Cur::field)?;
        if fulltext {
            self.str()?;
            if v >= 7.3 && self.i32()? == 2 {
                self.str()?;
            }
            if v >= 7.4 {
                self.i32()?;
            }
            if v >= 13.21 {
                self.str()?;
            }
        }
        t.filegroup = self.str()?;
        if v >= 1.1 {
            t.text_filegroup = self.str()?;
        }
        if v < 13.74 {
            self.strs()?;
        }
        if v >= 4.0 {
            if self.bool()? {
                t.partition_column = Some(self.field()?.name);
            }
            t.partition_scheme = self.str()?;
        }
        if v >= 5.3 {
            self.bool()?;
        }
        if v >= 7.0 {
            t.filestream_filegroup = self.str()?.filter(|f| !f.is_empty());
        }
        if v >= 7.1 {
            t.compression = self.compression()?;
        }
        if v >= 7.2 {
            self.str()?;
        }
        if v >= 7.5 {
            t.change_tracking = self.u8()?;
        }
        if v >= 10.2 {
            self.str()?;
        }
        if v >= 10.5 {
            t.lock_escalation = self.u8()?;
        }
        if v >= 11.0 {
            t.memory_optimized = self.bool()?;
            t.durability = self.u8()?;
        }
        if v >= 11.1 {
            self.strs()?;
        }
        if v >= 11.35 {
            self.u8()?;
        }
        if v >= 11.45 {
            self.sss(3)?;
        }
        if v >= 12.0 {
            t.modified = modified(self.i64()?);
        }
        if v >= 12.2 {
            t.temporal_type = self.u8()?;
            t.history = if v < 13.2 {
                self.str()?.as_deref().and_then(two_part)
            } else {
                let name = self.str()?;
                let schema = self.str()?;
                name.zip(schema).map(|(n, s)| ObjectName::new(&s, &n))
            };
        }
        if v >= 12.49 {
            let period = self.i32()?;
            let unit = self.str()?;
            t.retention = retention(period, unit.as_deref());
        }
        if v >= 13.02 {
            t.is_node = self.bool()?;
            t.is_edge = self.bool()?;
        }
        if v >= 13.13 {
            self.ss()?;
        }
        if v >= 13.18 && self.bool()? {
            let history = self.table()?;
            self.histories.push(history);
        }
        if v >= 13.62 {
            self.bool()?;
        }
        if v >= 13.7 {
            t.edge_constraints = self.coll(Cur::edge_constraint)?;
            t.edge_constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }
        if v >= 21.0 {
            self.ss_list()?;
        }
        Ok(t)
    }

    fn edge_constraint(&mut self) -> R<EdgeConstraint> {
        self.f32()?;
        let mut e = EdgeConstraint { name: self.s()?, disabled: self.bool()?, ..Default::default() };
        self.i32()?;
        e.not_trusted = self.bool()?;
        let n = self.count()?;
        for _ in 0..n {
            let from = self.s()?;
            let to = self.s()?;
            e.connections.push((two_part(&from).unwrap_or_default(), two_part(&to).unwrap_or_default()));
        }
        e.system_named = self.bool()?;
        self.sss(2)?;
        Ok(e)
    }

    fn table_item(&mut self) -> R<RawTable> {
        let at = self.p;
        let kind = self.u8()?;
        let t = self.table()?;
        match kind {
            0 => {}
            1 => {
                // FileTable: directory, filename collation, primary key, system object ids.
                self.str()?;
                self.str()?;
                if self.bool()? {
                    self.index()?;
                }
                let n = self.count()?;
                self.take(n.saturating_mul(4))?;
            }
            k => return Err(format!("unknown table kind {k} at offset {at}")),
        }
        Ok(t)
    }
}

// ------------------------------------------------------------------------------- programmability

impl Cur<'_> {
    fn function(&mut self) -> R<RawModule> {
        let v = self.f32()?;
        let mut r = Routine { name: self.s()?, schema: self.s()?, ..Default::default() };
        let text_id = self.i32()?;
        let perms = self.perms()?;
        let settings = ModuleSettings { quoted_identifier: self.bool()?, ansi_nulls: self.bool()? };
        self.i32()?;
        self.eprops()?;
        self.bool()?;
        r.schema_bound = self.bool()?;
        self.strs()?;
        if v >= 13.42 {
            self.strs()?;
        }
        self.strs()?;
        let function_type = if v >= 1.1 { self.i32()? } else { 0 };
        let mut clr = false;
        let mut binding = [String::new(), String::new(), String::new()];
        if v >= 4.0 {
            r.encrypted = self.bool()?;
            self.bool()?;
            for b in &mut binding {
                *b = self.s()?;
            }
            let mut result = Vec::new();
            if self.i32()? > 0 {
                result = self.coll(Cur::field)?;
            }
            clr = self.bool()?;
            let params = self.params_full()?;
            let returns = if self.bool()? { Some(self.param_full()?.0.data_type) } else { None };
            if clr || function_type == 4 {
                r.parameters = params.into_iter().map(|p| p.0).collect();
                r.returns = returns;
                r.result_columns = result.iter().enumerate().map(|(i, f)| column(f, i + 1)).collect();
                for c in &mut r.result_columns {
                    c.nullable = true;
                }
            }
        }
        if v >= 4.1 {
            self.bool()?;
        }
        if v >= 4.2 {
            r.null_on_null_input = self.bool()? && (clr || function_type == 4);
        }
        if v >= 7.0 {
            self.str()?;
        }
        if v >= 7.3 {
            r.execute_as = self.execute_as()?;
        } else if v >= 7.1 {
            r.execute_as = self.str()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
        }
        if v >= 12.0 {
            r.modified = modified(self.i64()?);
        }
        if v >= 13.7916 {
            self.references()?;
        }
        r.kind = match (clr, function_type) {
            (_, 4) => RoutineKind::ClrAggregate,
            (true, 2) => RoutineKind::ClrScalarFunction,
            (true, _) => RoutineKind::ClrTableFunction,
            (false, 1) => RoutineKind::InlineTableFunction,
            (false, 3) => RoutineKind::TableFunction,
            (false, _) => RoutineKind::ScalarFunction,
        };
        if clr || function_type == 4 {
            r.clr_method = Some(binding.iter().filter(|b| !b.is_empty()).map(|b| objects::bracket(b)).collect::<Vec<_>>().join("."));
        } else {
            r.settings = settings;
        }
        Ok(RawModule { routine: r, text_id, perms })
    }

    fn procedure(&mut self) -> R<RawModule> {
        let v = self.f32()?;
        let mut r = Routine { name: self.s()?, schema: self.s()?, ..Default::default() };
        let text_id = self.i32()?;
        if v >= 7.2 {
            r.native_compilation = self.bool()?;
        }
        self.eprops()?;
        self.i32()?;
        let perms = self.perms()?;
        let mut clr = false;
        let settings = if v < 6.2 {
            ModuleSettings { quoted_identifier: self.bool()?, ansi_nulls: self.bool()? }
        } else {
            let flags = self.i16()?;
            clr = flags & 0x4 != 0;
            r.encrypted = flags & 0x8 != 0;
            ModuleSettings { quoted_identifier: flags & 0x1 != 0, ansi_nulls: flags & 0x2 != 0 }
        };
        self.bool()?;
        self.strs()?;
        let mut binding = [String::new(), String::new(), String::new()];
        if v >= 4.0 {
            r.execute_as = if v >= 7.1 { self.execute_as()? } else { self.str()? };
            for b in &mut binding {
                *b = self.s()?;
            }
            if v < 6.2 {
                clr = self.bool()?;
                r.encrypted = self.bool()?;
            }
            let params = self.params_full()?;
            if clr {
                r.parameters = params.into_iter().map(|p| p.0).collect();
            }
        }
        if (5.3..6.2).contains(&v) {
            self.bool()?;
        }
        if v >= 7.0 {
            self.str()?;
        }
        if v >= 7.3 {
            self.strs()?;
        }
        if v >= 11.45 {
            self.ss_list()?;
            self.sss(2)?;
        }
        if v >= 12.0 {
            r.modified = modified(self.i64()?);
        }
        if v >= 13.77 {
            self.references()?;
        }
        if clr {
            r.kind = RoutineKind::ClrProcedure;
            r.clr_method = Some(binding.iter().filter(|b| !b.is_empty()).map(|b| objects::bracket(b)).collect::<Vec<_>>().join("."));
        } else {
            r.settings = settings;
        }
        Ok(RawModule { routine: r, text_id, perms })
    }

    fn view(&mut self) -> R<RawView> {
        let v = self.f32()?;
        let mut view = View { name: self.s()?, schema: self.s()?, ..Default::default() };
        let text_id = self.i32()?;
        self.eprops()?;
        self.i32()?;
        let perms = self.perms()?;
        let stats_from = self.stats.len();
        view.indexes = self.coll(Cur::index)?.into_iter().flatten().collect();
        view.statistics = self.stats.split_off(stats_from);
        view.statistics.sort_by(|a, b| a.name.cmp(&b.name));
        let triggers = self.coll(Cur::trigger)?;
        self.bool()?;
        // Output columns carry names only (no types), so they are left out.
        self.coll(Cur::field)?;
        view.settings = ModuleSettings { quoted_identifier: self.bool()?, ansi_nulls: self.bool()? };
        view.schema_bound = self.bool()?;
        if v >= 4.0 {
            view.encrypted = self.bool()?;
        }
        if v >= 5.3 {
            self.bool()?;
        }
        if v >= 7.0 && self.bool()? {
            self.str()?;
            if v >= 7.2 && self.i32()? == 2 {
                self.str()?;
            }
            if v >= 7.3 {
                self.i32()?;
            }
        }
        if v >= 7.1 {
            self.str()?;
        }
        if v >= 10.2 {
            self.str()?;
        }
        if v >= 13.7916 {
            self.references()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
        }
        if v >= 12.0 {
            view.modified = modified(self.i64()?);
        }
        Ok(RawView { view, text_id, perms, triggers })
    }

    fn user_type(&mut self) -> R<UserType> {
        let v = self.f32()?;
        let mut ty = UserType { name: self.s()?, schema: self.s()?, ..Default::default() };
        self.eprops()?;
        self.bool()?;
        ty.nullable = self.bool()?;
        let (length, precision, scale) = (self.i32()?, self.i32()?, self.i32()?);
        let sql_type = self.i32()?;
        let base = self.s()?;
        self.str()?;
        self.str()?;
        for _ in 0..3 {
            self.strs()?;
        }
        if v >= 4.0 {
            let assembly = self.s()?;
            let class = self.s()?;
            match sql_type {
                -3 => {
                    ty.kind = UserTypeKind::Table;
                    ty.nullable = false;
                    if v >= 7.1 {
                        let t = self.table()?;
                        ty.memory_optimized = t.memory_optimized;
                        ty.columns = t.columns.iter().enumerate().map(|(i, f)| column(f, i + 1)).collect();
                        ty.indexes = t.indexes;
                        ty.check_constraints = t.checks;
                        for ix in &mut ty.indexes {
                            if ix.storage.data_space.is_none() && !ty.memory_optimized {
                                ix.storage.data_space = Some("PRIMARY".into());
                            }
                        }
                    }
                }
                -2 => {
                    ty.kind = UserTypeKind::Clr;
                    ty.clr_class = Some(format!("{}.{}", objects::bracket(&assembly), objects::bracket(&class)));
                }
                _ => {}
            }
            let perms = self.perms()?;
            self.type_perms.push((ty.schema.clone(), ty.name.clone(), perms));
        }
        if ty.kind == UserTypeKind::Alias {
            ty.base_type = Some(system_type(&base, length, precision, scale));
        }
        if v >= 7.2 {
            self.str()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
        }
        if v >= 13.21 {
            self.strs()?;
        }
        Ok(ty)
    }

    fn synonym(&mut self) -> R<(Synonym, Vec<RawPerm>)> {
        let v = self.f32()?;
        let mut syn = Synonym { name: self.s()?, schema: self.s()?, ..Default::default() };
        self.i32()?;
        syn.base_object = self.s()?;
        self.eprops()?;
        let perms = self.perms()?;
        if v >= 7.0 {
            self.str()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
        }
        Ok((syn, perms))
    }

    fn sequence(&mut self) -> R<(Sequence, Vec<RawPerm>)> {
        let perms = self.perms()?;
        self.eprops()?;
        let v = self.f32()?;
        let mut sq = Sequence { name: self.s()?, schema: self.s()?, ..Default::default() };
        self.i32()?;
        self.i32()?;
        self.bool()?;
        let mut values = [String::new(), String::new(), String::new(), String::new()];
        for x in &mut values {
            *x = if v < 11.2 { self.i64()?.to_string() } else { self.s()? };
        }
        [sq.start, sq.increment, sq.minimum, sq.maximum] = values;
        sq.cycle = self.bool()?;
        sq.cached = self.bool()?;
        let cache_size = self.i32()?;
        if sq.cached && cache_size >= 0 {
            sq.cache_size = Some(i64::from(cache_size));
        }
        let type_name = self.s()?;
        let type_schema = self.s()?;
        let (precision, scale) = (self.u8()?, self.u8()?);
        sq.data_type = if is_user_schema(&type_schema) {
            DataType { name: type_name, schema: Some(type_schema), ..Default::default() }
        } else {
            system_type(&type_name, 0, i32::from(precision), i32::from(scale))
        };
        if v < 11.2 {
            self.i64()?;
        } else {
            self.str()?;
        }
        self.bool()?;
        self.str()?;
        if v >= 11.45 {
            self.sss(2)?;
        }
        Ok((sq, perms))
    }
}

// ------------------------------------------------------------------------------- security

impl Cur<'_> {
    fn user(&mut self) -> R<RawUser> {
        let v = self.f32()?;
        let mut u = RawUser { name: self.s()?, login: self.str()?, user_type: 1, default_schema: None, certificate: None, asymmetric_key: None, db_perms: Vec::new() };
        if v >= 4.0 {
            u.user_type = self.i32()?;
            u.certificate = self.str()?.filter(|c| !c.is_empty());
            u.asymmetric_key = self.str()?.filter(|k| !k.is_empty());
            u.default_schema = self.str()?;
        } else {
            self.bool()?;
            if self.bool()? {
                u.user_type = 2;
            }
        }
        self.eprops()?;
        if v >= 4.0 {
            self.perms()?;
            u.db_perms = self.db_perms()?;
        }
        if v >= 11.45 {
            self.ss()?;
        }
        Ok(u)
    }

    fn role(&mut self) -> R<RawRole> {
        let v = self.f32()?;
        let name = self.s()?;
        let owner = if v >= 4.0 { self.str()? } else { None };
        self.bool()?;
        let mut r = RawRole { name, owner, app_role: self.bool()?, members: self.strs()?, db_perms: Vec::new() };
        if v >= 4.0 {
            self.perms()?;
            r.db_perms = self.db_perms()?;
        }
        if v >= 5.0 {
            self.eprops()?;
        }
        if v >= 11.45 {
            self.sss(2)?;
            if v >= 11.54 {
                self.ss_dict()?;
            }
        }
        Ok(r)
    }

    fn schema(&mut self) -> R<RawSchema> {
        let v = self.f32()?;
        let name = self.s()?;
        let system_object = self.bool()?;
        let owner = self.s()?;
        self.bool()?;
        self.eprops()?;
        let perms = self.perms()?;
        if v >= 11.45 {
            self.sss(2)?;
        }
        Ok(RawSchema { name, owner, system_object, perms })
    }

    /// The trailing pair of script sources most objects have from version 11.45.
    fn two_ss(&mut self, v: f32) -> R<()> {
        if v >= 11.45 { self.sss(2) } else { Ok(()) }
    }
}

// ------------------------------------------------------------------------------- skipped members

impl Cur<'_> {
    /// Records written with a leading 0xFF and a version; legacy ones have neither.
    fn versioned_record(&mut self) -> R<bool> {
        if self.b.get(self.p) == Some(&0xFF) {
            self.p += 1;
            self.f32()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

}

// ------------------------------------------------------------------------------- model

impl Snap {
    fn text(&self, id: i32) -> String {
        // Ids are object ids widened with sign extension (-1 is the "missing" placeholder).
        self.texts.get(&(id as i64 as u64)).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
    }

    fn is_encrypted(&self, id: i32) -> bool {
        self.encrypted.contains(&(id as i64 as u64))
    }

    fn trigger(&self, raw: RawTrigger) -> Trigger {
        let mut t = raw.trigger;
        t.encrypted |= self.is_encrypted(raw.text_id);
        if !t.encrypted {
            t.definition = self.text(raw.text_id);
        }
        t
    }

    fn build(mut self) -> Database {
        let mut db = Database {
            model_version: MODEL_VERSION,
            server: std::mem::take(&mut self.server),
            name: std::mem::take(&mut self.name),
            compatibility_level: self.compatibility_level,
            ..Default::default()
        };
        let mut permissions = Vec::new();
        let mut object_perms = |schema: &str, name: &str, perms: &[RawPerm]| {
            for p in perms {
                permissions.extend(objects::grants(p, "OBJECT", Some(schema), name));
            }
        };

        for t in std::mem::take(&mut self.tables) {
            object_perms(&t.schema, &t.name, &t.perms);
            let (statistics, edge_constraints) = (t.statistics, t.edge_constraints);
            let triggers = t.triggers.into_iter().map(|tr| self.trigger(tr)).collect();
            let columns: Vec<Column> = t.columns.iter().enumerate().map(|(i, f)| column(f, i + 1)).collect();
            let mut table = Table {
                text_filegroup: t.text_filegroup.filter(|_| columns.iter().any(is_lob)),
                schema: t.schema,
                name: t.name,
                modified: t.modified,
                columns,
                indexes: t.indexes,
                foreign_keys: t.foreign_keys,
                check_constraints: t.checks,
                triggers,
                filestream_filegroup: t.filestream_filegroup,
                lock_escalation: match t.lock_escalation {
                    1 => "DISABLE",
                    2 => "AUTO",
                    _ => "TABLE",
                }
                .into(),
                memory_optimized: t.memory_optimized,
                history_table: t.temporal_type == 1,
                graph: if t.is_node {
                    Some("NODE".into())
                } else if t.is_edge {
                    Some("EDGE".into())
                } else {
                    None
                },
                change_tracking: match t.change_tracking {
                    0 => None,
                    n => Some(ChangeTracking { track_columns_updated: n == 2 }),
                },
                statistics,
                edge_constraints,
                ..Default::default()
            };
            if table.graph.is_some() {
                crate::catalog::read::graph_indexes(&mut table);
            }
            table.storage = Storage {
                data_space: if t.partition_column.is_some() { t.partition_scheme.as_deref().map(unquote).or(t.filegroup) } else { t.filegroup },
                partition_column: t.partition_column,
                compression: t.compression,
            };
            if table.memory_optimized {
                table.durability = Some(if t.durability == 1 { "SCHEMA_ONLY" } else { "SCHEMA_AND_DATA" }.into());
                table.storage.data_space = None;
            }
            if t.temporal_type == 2 {
                let period = |kind: &str| table.columns.iter().find(|c| c.generated_always.as_deref() == Some(kind)).map(|c| c.name.clone());
                table.temporal = Some(Temporal {
                    history_table: t.history.unwrap_or_default(),
                    period_start: period("ROW START").unwrap_or_default(),
                    period_end: period("ROW END").unwrap_or_default(),
                    retention: t.retention,
                });
            }
            table.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            table.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
            table.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
            table.triggers.sort_by(|a, b| a.name.cmp(&b.name));
            db.tables.push(table);
        }

        for v in std::mem::take(&mut self.views) {
            let mut view = v.view;
            object_perms(&view.schema, &view.name, &v.perms);
            view.encrypted |= self.is_encrypted(v.text_id);
            if !view.encrypted {
                view.definition = self.text(v.text_id);
            }
            view.triggers = v.triggers.into_iter().map(|tr| self.trigger(tr)).collect();
            view.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            view.triggers.sort_by(|a, b| a.name.cmp(&b.name));
            db.views.push(view);
        }

        for m in std::mem::take(&mut self.functions).into_iter().chain(std::mem::take(&mut self.procedures)) {
            let mut r = m.routine;
            object_perms(&r.schema, &r.name, &m.perms);
            if r.clr_method.is_none() {
                r.encrypted |= self.is_encrypted(m.text_id);
                if !r.encrypted {
                    r.definition = self.text(m.text_id);
                }
            }
            db.routines.push(r);
        }

        for (sq, perms) in std::mem::take(&mut self.sequences) {
            object_perms(&sq.schema, &sq.name, &perms);
            db.sequences.push(sq);
        }
        for (syn, perms) in std::mem::take(&mut self.synonyms) {
            object_perms(&syn.schema, &syn.name, &perms);
            db.synonyms.push(syn);
        }
        db.types = std::mem::take(&mut self.types);
        for (schema, name, perms) in std::mem::take(&mut self.type_perms) {
            for p in &perms {
                permissions.extend(objects::grants(p, "TYPE", Some(&schema), &name));
            }
        }
        let more = std::mem::take(&mut self.more);
        more.build(&mut db, &mut permissions, &|id| (self.text(id), self.is_encrypted(id)));
        for ty in &mut db.types {
            ty.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            ty.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }

        // The live reader keeps dbo and the user schemas.
        for s in &self.schemas {
            if s.system_object && s.name != "dbo" {
                continue;
            }
            for p in &s.perms {
                if let Some(mut p) = permission(p) {
                    p.class = "SCHEMA".into();
                    p.schema = Some(s.name.clone());
                    permissions.push(p);
                }
            }
            db.schemas.push(Schema { name: s.name.clone(), owner: s.owner.clone() });
        }

        self.principals(&mut db, &mut permissions);
        db.permissions = permissions;
        db.extended_properties = extended_properties(&self.props);
        // SQL Clone's marker is not part of the schema.
        let cs = self.case_sensitive;
        db.extended_properties.retain(|p| !p.is_clone_marker(cs));
        db.dependencies = self.dependencies(&db);
        db.collation = collation(&db, self.case_sensitive);

        let by_name = |a: &(&String, &String), b: &(&String, &String)| sql_order(a.0, b.0).then_with(|| sql_order(a.1, b.1));
        db.schemas.sort_by(|a, b| sql_order(&a.name, &b.name));
        db.tables.sort_by(|a, b| by_name(&(&a.schema, &a.name), &(&b.schema, &b.name)));
        db.views.sort_by(|a, b| by_name(&(&a.schema, &a.name), &(&b.schema, &b.name)));
        db.types.sort_by(|a, b| by_name(&(&a.schema, &a.name), &(&b.schema, &b.name)));
        db.sequences.sort_by(|a, b| by_name(&(&a.schema, &a.name), &(&b.schema, &b.name)));
        db.synonyms.sort_by(|a, b| by_name(&(&a.schema, &a.name), &(&b.schema, &b.name)));
        db.routines.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        db.principals.sort_by(|a, b| sql_order(&a.name, &b.name));
        db.permissions.sort();
        db
    }

    /// Users and roles; role membership is stored on the role and moved to the member.
    fn principals(&self, db: &mut Database, permissions: &mut Vec<Permission>) {
        let named = |list: &[&str], name: &str| list.iter().any(|x| x.eq_ignore_ascii_case(name));
        for u in &self.users {
            if u.name.eq_ignore_ascii_case("dbo") {
                db.owner = u.login.clone().filter(|l| !l.is_empty());
            }
            if named(&BUILT_IN_USERS, &u.name) || named(&FIXED_ROLES, &u.name) {
                continue;
            }
            let kind = match u.user_type {
                2 => "WINDOWS_USER",
                3 => "CERTIFICATE_MAPPED_USER",
                4 => "ASYMMETRIC_KEY_MAPPED_USER",
                5 => "EXTERNAL_USER",
                _ => "SQL_USER",
            };
            db.principals.push(Principal {
                name: u.name.clone(),
                kind: kind.into(),
                login: u.login.clone().filter(|l| !l.is_empty()),
                default_schema: u.default_schema.clone().filter(|s| !s.is_empty()),
                certificate: u.certificate.clone(),
                asymmetric_key: u.asymmetric_key.clone(),
                no_connect: !u.db_perms.iter().any(|(a, _)| a.eq_ignore_ascii_case("CONNECT")),
                ..Default::default()
            });
        }
        for r in &self.roles {
            if named(&FIXED_ROLES, &r.name) || named(&BUILT_IN_USERS, &r.name) {
                continue;
            }
            // An application role keeps its default schema where a role has its owner.
            let owner = r.owner.clone().filter(|o| !o.is_empty());
            db.principals.push(Principal {
                name: r.name.clone(),
                kind: if r.app_role { "APPLICATION_ROLE" } else { "DATABASE_ROLE" }.into(),
                owner: if r.app_role { None } else { owner.clone() },
                default_schema: if r.app_role { owner } else { None },
                ..Default::default()
            });
        }
        for r in &self.roles {
            for m in &r.members {
                if let Some(p) = db.principals.iter_mut().find(|p| &p.name == m) {
                    p.member_of.push(r.name.clone());
                }
            }
        }
        for p in &mut db.principals {
            p.member_of.sort_by(|a, b| sql_order(a, b));
        }
        let grants = self.users.iter().map(|u| (&u.name, &u.db_perms)).chain(self.roles.iter().map(|r| (&r.name, &r.db_perms)));
        for (grantee, perms) in grants {
            if named(&SYSTEM_GRANTEES, grantee) {
                continue;
            }
            for (action, statement) in perms {
                // CONNECT comes with CREATE USER: only DENY and WITH GRANT OPTION are kept.
                let Some(state) = state(i32::from(*statement)) else { continue };
                if action.eq_ignore_ascii_case("CONNECT") && state == "GRANT" {
                    continue;
                }
                permissions.push(Permission {
                    class: "DATABASE".into(),
                    grantee: grantee.clone(),
                    permission: action.clone(),
                    state: state.into(),
                    ..Default::default()
                });
            }
        }
    }

    /// The same references the live reader keeps: module bodies, constraints and defaults
    /// (as their table), FKs, typed columns and synonyms.
    fn dependencies(&self, db: &Database) -> Vec<Dependency> {
        let mut checks = HashMap::new();
        let mut trigger_parents = HashMap::new();
        for t in &db.tables {
            for ck in &t.check_constraints {
                checks.insert(ObjectName::new(&t.schema, &ck.name), t.object_name());
            }
        }
        // A DML trigger is in its parent's schema.
        let owners = db.tables.iter().map(|t| (&t.schema, &t.name, &t.triggers)).chain(db.views.iter().map(|v| (&v.schema, &v.name, &v.triggers)));
        for (schema, name, triggers) in owners {
            for tr in triggers {
                trigger_parents.insert(ObjectName::new(schema, &tr.name), ObjectName::new(schema, name));
            }
        }
        let mut schema_bound: HashMap<ObjectName, bool> = HashMap::new();
        for v in &db.views {
            schema_bound.insert(ObjectName::new(&v.schema, &v.name), v.schema_bound);
        }
        for r in &db.routines {
            schema_bound.insert(ObjectName::new(&r.schema, &r.name), r.schema_bound || r.native_compilation);
        }

        let mut deps: BTreeMap<(ObjectName, ObjectName), bool> = BTreeMap::new();
        for (from_type, from, to_type, to) in &self.dependencies {
            if !matches!(*to_type, OT_TABLE | OT_PROCEDURE | OT_VIEW | OT_FUNCTION | OT_SYNONYM | OT_SEQUENCE) {
                continue;
            }
            let (Some(from), Some(to)) = (two_part(from), two_part(to)) else { continue };
            let (from, strong) = match *from_type {
                // Table to table is a foreign key, taken from the model below.
                OT_TABLE if *to_type == OT_TABLE => continue,
                OT_TABLE => (from, true),
                OT_CHECK => match checks.get(&from) {
                    Some(t) => (t.clone(), true),
                    None => continue,
                },
                OT_VIEW | OT_FUNCTION | OT_PROCEDURE => {
                    let strong = schema_bound.get(&from).copied().unwrap_or(false);
                    (from, strong)
                }
                // Every trigger is listed as using its own table, whether its body does or not.
                OT_TRIGGER if trigger_parents.get(&from) == Some(&to) => continue,
                OT_TRIGGER => (from, false),
                _ => continue,
            };
            if from == to || from.schema == "sys" || to.schema == "sys" {
                continue;
            }
            *deps.entry((from, to)).or_insert(false) |= strong;
        }
        for t in &db.tables {
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
        for syn in &db.synonyms {
            if let Some(target) = synonym_target(&syn.base_object, &db.name) {
                deps.entry((ObjectName::new(&syn.schema, &syn.name), target)).or_insert(false);
            }
        }
        deps.into_iter().map(|((from, to), strong)| Dependency { from, to, strong }).collect()
    }
}

/// GRANT / DENY / GRANT_WITH_GRANT_OPTION; revokes are not stored.
fn state(statement: i32) -> Option<&'static str> {
    match statement {
        205 => Some("GRANT"),
        206 => Some("DENY"),
        1 => Some("GRANT_WITH_GRANT_OPTION"),
        _ => None,
    }
}

/// An object or schema permission without its securable.
fn permission(p: &RawPerm) -> Option<Permission> {
    if p.skip || SYSTEM_GRANTEES.iter().any(|g| g.eq_ignore_ascii_case(&p.grantee)) {
        return None;
    }
    Some(Permission { grantee: p.grantee.clone(), permission: p.action.clone(), state: state(p.statement)?.into(), ..Default::default() })
}

/// Properties named by sp_addextendedproperty levels, in the live reader's classes.
fn extended_properties(props: &[RawProp]) -> Vec<ExtendedProperty> {
    let mut out: Vec<ExtendedProperty> = Vec::new();
    for raw in props {
        let up = |i: usize| raw.types[i].as_deref().map(str::to_ascii_uppercase);
        let mut p = ExtendedProperty { name: raw.name.clone(), value: raw.value.clone(), ..Default::default() };
        match (up(0).as_deref(), up(1).as_deref()) {
            (None, _) => p.class = "DATABASE".into(),
            (Some("SCHEMA"), None) => {
                p.class = "SCHEMA".into();
                p.schema = raw.names[0].clone();
            }
            // Database-wide objects are named at level 0.
            (Some(level0 @ ("ASSEMBLY" | "MESSAGE TYPE" | "CONTRACT" | "SERVICE" | "REMOTE SERVICE BINDING" | "ROUTE" | "PARTITION FUNCTION" | "PARTITION SCHEME" | "TRIGGER")), None) => {
                p.class = level0.into();
                p.object = raw.names[0].clone();
            }
            (Some("SCHEMA"), Some(level1)) => {
                p.class = match level1 {
                    "TABLE" | "VIEW" | "PROCEDURE" | "FUNCTION" | "SEQUENCE" | "SYNONYM" | "TYPE" | "DEFAULT" | "RULE" | "QUEUE" | "XML SCHEMA COLLECTION" => level1,
                    "AGGREGATE" => "FUNCTION",
                    _ => continue,
                }
                .into();
                p.schema = raw.names[0].clone();
                p.object = raw.names[1].clone();
                if p.class != "TYPE" {
                    p.minor_type = up(2);
                    p.minor_name = raw.names[2].clone().filter(|_| p.minor_type.is_some());
                }
            }
            _ => continue,
        }
        // Trigger and FK properties may also be listed with their table.
        if !out.iter().any(|x| x.class == p.class && x.schema == p.schema && x.object == p.object && x.minor_type == p.minor_type && x.minor_name == p.minor_name && x.name == p.name) {
            out.push(p);
        }
    }
    out
}

/// The database collation is not stored: the most common column collation that agrees with
/// the database's case-sensitivity, else the server default.
fn collation(db: &Database, case_sensitive: bool) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let columns = db.tables.iter().flat_map(|t| &t.columns).chain(db.types.iter().flat_map(|t| &t.columns));
    for c in columns {
        if let Some(col) = &c.collation {
            *counts.entry(col).or_default() += 1;
        }
    }
    let cs = |c: &str| Database { collation: c.into(), ..Default::default() }.case_sensitive();
    counts
        .into_iter()
        .filter(|(c, _)| cs(c) == case_sensitive)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(c, _)| c.to_string())
        .unwrap_or_else(|| if case_sensitive { "SQL_Latin1_General_CP1_CS_AS" } else { "SQL_Latin1_General_CP1_CI_AS" }.into())
}

fn column(f: &RawField, ordinal: usize) -> Column {
    let user = is_user_schema(&f.type_schema);
    let mut data_type = if user {
        DataType { name: f.type_name.clone(), schema: Some(f.type_schema.clone()), ..Default::default() }
    } else {
        system_type(&f.type_name, f.size, f.precision, f.scale)
    };
    if data_type.name == "xml" {
        data_type.xml_collection = f.xml_collection.as_deref().filter(|x| !x.is_empty()).map(requote);
        data_type.xml_document = f.flags & 0x200 != 0;
    }
    // Alias types carry their own collation; COLLATE cannot be written on them.
    let character = !user && matches!(f.type_name.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname");
    let flag = |b: i16| f.flags & b != 0;
    Column {
        name: f.name.clone(),
        ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
        data_type,
        nullable: flag(0x10),
        collation: (character && !f.collation.is_empty()).then(|| f.collation.clone()),
        identity: flag(0x1).then(|| Identity { seed: f.seed.clone(), increment: f.increment.to_string(), not_for_replication: flag(0x4) }),
        computed: flag(0x2).then(|| Computed { definition: f.computed.clone(), persisted: flag(0x100) }),
        default: (!f.default_name.is_empty()).then(|| DefaultConstraint {
            name: f.default_name.clone(),
            system_named: f.default_system_named,
            definition: f.default_text.clone(),
        }),
        rowguidcol: flag(0x20),
        sparse: flag(0x800),
        filestream: flag(0x1000),
        column_set: flag(0x2000),
        generated_always: match f.generated {
            1 => Some("ROW START".into()),
            2 => Some("ROW END".into()),
            _ => None,
        },
        hidden: f.hidden,
        masking_function: (!f.masking.is_empty()).then(|| f.masking.clone()),
        bound_default: f.bound_default.as_deref().map(requote),
        bound_rule: f.bound_rule.as_deref().map(requote),
        sensitivity: f.sensitivity.clone(),
    }
}

/// A module's or table's last-modified time (a .NET `DateTime` binary); None when not recorded.
fn modified(ticks: i64) -> Option<String> {
    (ticks != 0).then(|| objects::ticks_text_ms(ticks & 0x3FFF_FFFF_FFFF_FFFF))
}

fn is_user_schema(schema: &str) -> bool {
    !schema.is_empty() && !schema.eq_ignore_ascii_case("sys")
}

/// A system type; snapshot lengths are already in characters.
/// System type names by the type codes of XML index paths.
fn sql_type_name(code: i32) -> Option<&'static str> {
    const NAMES: [&str; 30] = [
        "binary", "varbinary", "char", "varchar", "nchar", "nvarchar", "datetime", "smalldatetime", "decimal", "numeric", "float", "real", "bigint", "int",
        "smallint", "tinyint", "money", "smallmoney", "bit", "timestamp", "uniqueidentifier", "image", "ntext", "text", "sql_variant", "xml", "date", "time",
        "datetimeoffset", "datetime2",
    ];
    usize::try_from(code).ok().and_then(|c| c.checked_sub(1)).and_then(|c| NAMES.get(c).copied())
}

fn system_type(name: &str, length: i32, precision: i32, scale: i32) -> DataType {
    let mut t = DataType { name: name.to_string(), ..Default::default() };
    let small = |x: i32| u8::try_from(x).ok();
    match name {
        "char" | "nchar" | "binary" => t.length = Some(length),
        // Old files wrote 0 for max.
        "varchar" | "nvarchar" | "varbinary" => t.length = Some(if length == 0 { -1 } else { length }),
        "decimal" | "numeric" => {
            t.precision = small(precision);
            t.scale = small(scale);
        }
        "datetime2" | "time" | "datetimeoffset" => t.scale = small(scale),
        "float" => t.precision = small(precision),
        _ => {}
    }
    t
}

fn is_lob(c: &Column) -> bool {
    let t = &c.data_type;
    matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml")
        || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1))
        || t.schema.is_some()
}

fn referential_action(code: u8) -> &'static str {
    match code {
        0 => "NO_ACTION",
        2 => "SET_NULL",
        3 => "SET_DEFAULT",
        _ => "CASCADE",
    }
}

/// e.g. `6 MONTHS`; `None` = infinite.
fn retention(period: i32, unit: Option<&str>) -> Option<String> {
    let unit = unit?.trim().to_ascii_uppercase();
    if period < 0 || unit.is_empty() || unit == "INFINITE" {
        return None;
    }
    Some(format!("{period} {}S", unit.trim_end_matches('S')))
}

/// Keeps one entry when every partition is compressed the same way, as the live reader does.
fn collapse(mut list: Vec<PartitionCompression>) -> Vec<PartitionCompression> {
    list.sort_by_key(|p| p.partition);
    if !list.is_empty() && list.iter().all(|p| p.compression == list[0].compression) {
        let compression = list[0].compression.clone();
        return if compression == "NONE" { Vec::new() } else { vec![PartitionCompression { partition: 0, compression }] };
    }
    list
}

/// Case-insensitive first, like the server's ORDER BY under a CI collation.
fn sql_order(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// `[schema].[name]` → its parts.
fn two_part(s: &str) -> Option<ObjectName> {
    match split_multipart(s).as_slice() {
        [schema, name] => Some(ObjectName::new(schema, name)),
        _ => None,
    }
}

/// The local object a synonym points at, when it is in this database.
fn synonym_target(base: &str, database: &str) -> Option<ObjectName> {
    match split_multipart(base).as_slice() {
        [schema, name] => Some(ObjectName::new(schema, name)),
        [db, schema, name] if db.eq_ignore_ascii_case(database) => Some(ObjectName::new(schema, name)),
        _ => None,
    }
}

fn unquote(s: &str) -> String {
    split_multipart(s).pop().unwrap_or_default()
}

fn requote(s: &str) -> String {
    split_multipart(s).iter().map(|p| quote(p)).collect::<Vec<_>>().join(".")
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

/// .NET decimal: 96-bit magnitude, scale in bits 16-23 of the flags, sign in bit 31.
fn decimal(b: [u8; 16]) -> String {
    let word = |i: usize| u128::from(u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]));
    let magnitude = word(8) << 64 | word(4) << 32 | word(0);
    let flags = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    let scale = ((flags >> 16) & 0xFF) as usize;
    let mut s = format!("{magnitude:0>width$}", width = scale + 1);
    if scale > 0 {
        s.insert(s.len() - scale, '.');
    }
    if flags & 0x8000_0000 != 0 {
        s.insert(0, '-');
    }
    s
}

/// .NET `Guid.ToByteArray` order, as SQL Server prints it.
pub(crate) fn guid(b: [u8; 16]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    let tail: String = b[8..].iter().map(|x| format!("{x:02X}")).collect();
    format!("{d1:08X}-{d2:04X}-{d3:04X}-{}-{}", &tail[..4], &tail[4..])
}

/// OLE Automation date (days since 1899-12-30) as `yyyy-mm-dd hh:mm:ss.fff`.
fn oa_date(days: f64) -> String {
    let ms = (days * 86_400_000.0).round();
    if !ms.is_finite() || ms.abs() > 1e15 {
        return days.to_string();
    }
    // Days from 1970-01-01 to 1899-12-30 is -25569.
    let ms = ms as i64 - 25_569 * 86_400_000;
    let (day, ms) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil date from a day number (H. Hinnant).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nullable string as the writer stores it.
    fn s(text: &str) -> Vec<u8> {
        let mut out = vec![1];
        let mut n = text.len();
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn strings() {
        let mut bytes = s("SQLCOMPARE");
        assert_eq!(&bytes[..2], &[1, 10]);
        bytes.extend([0, 1, 0]);
        let mut c = Cur::new(&bytes);
        assert_eq!(c.str().unwrap().as_deref(), Some("SQLCOMPARE"));
        assert_eq!(c.str().unwrap(), None);
        assert_eq!(c.str().unwrap().as_deref(), Some(""));
        assert_eq!(c.p, bytes.len());

        let long = "x".repeat(300);
        let bytes = s(&long);
        assert_eq!(&bytes[..3], &[1, 0xAC, 0x02]);
        assert_eq!(Cur::new(&bytes).s().unwrap(), long);

        assert_eq!(Cur::new(&[2, 0]).str().unwrap_err(), "bad string marker 2 at offset 0");
        assert_eq!(Cur::new(&[1, 5, b'a']).str().unwrap_err(), "truncated snapshot at offset 2");
        assert!(Cur::new(&[1, 1, 0xFF]).str().unwrap_err().starts_with("invalid UTF-8"));
        assert!(Cur::new(&[1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).str().unwrap_err().starts_with("bad string length"));
    }

    #[test]
    fn collections_and_null_members() {
        // cver 4.0, case-insensitive, two strings.
        let mut bytes = vec![0, 0, 0x80, 0x40, 0, 2, 0, 0, 0];
        bytes.extend(s("a"));
        bytes.extend(s("b"));
        bytes.extend(NULL_MEMBER);
        bytes.extend([0, 0, 0x80, 0x40, 0, 0, 0, 0, 0]);
        let mut c = Cur::new(&bytes);
        assert_eq!(c.member(Cur::s).unwrap(), vec!["a", "b"]);
        assert!(c.member(Cur::s).unwrap().is_empty());
        assert_eq!(c.p, bytes.len() - 9);
        assert!(c.member(Cur::s).unwrap().is_empty());
        assert_eq!(c.p, bytes.len());

        let bad = [0, 0, 0x80, 0x40, 0, 0xFF, 0xFF, 0xFF, 0x7F];
        assert_eq!(Cur::new(&bad).coll(Cur::s).unwrap_err(), "bad count 2147483647 at offset 5");
        let negative = [0, 0, 0x80, 0x40, 0, 0xFF, 0xFF, 0xFF, 0xFF];
        assert!(Cur::new(&negative).coll(Cur::s).is_err());
    }

    #[test]
    fn permissions_and_script_sources() {
        let ver = 23f32.to_le_bytes();
        let mut bytes = 4f32.to_le_bytes().to_vec();
        bytes.extend(2i32.to_le_bytes());
        for (statement, action, grantee, columns) in [(206, "DELETE", "Reporting", false), (205, "SELECT", "app", true)] {
            bytes.extend(ver);
            bytes.extend(i32::to_le_bytes(statement));
            bytes.extend(s(action));
            bytes.extend(s(grantee));
            bytes.push(0);
            bytes.push(u8::from(columns));
            if columns {
                bytes.extend(1i32.to_le_bytes());
                bytes.extend(s("Name"));
            }
        }
        // A present script source with empty lists, then a null one.
        bytes.push(1);
        bytes.extend(ver);
        bytes.extend(s("x.sql"));
        bytes.extend([0; 16]);
        bytes.push(0);
        let mut c = Cur::new(&bytes);
        let perms = c.perms().unwrap();
        assert_eq!(perms.len(), 2);
        assert_eq!(permission(&perms[0]).unwrap().grant_sql(), "DENY DELETE TO [Reporting]");
        assert_eq!(perms[1].columns, vec!["Name"]);
        assert_eq!(objects::grants(&perms[1], "OBJECT", Some("dbo"), "T")[0].column.as_deref(), Some("Name"));
        c.ss().unwrap();
        c.ss().unwrap();
        assert_eq!(c.p, bytes.len());
    }

    #[test]
    fn deep_nesting_fails_cleanly() {
        let mut bytes = Vec::new();
        for _ in 0..100 {
            bytes.push(1);
            bytes.extend(23f32.to_le_bytes());
            bytes.push(0);
            bytes.extend([0; 8]);
            bytes.extend(1i32.to_le_bytes());
        }
        assert!(Cur::new(&bytes).ss().unwrap_err().starts_with("snapshot nesting too deep"));
    }

    #[test]
    fn values() {
        let mut b = [0u8; 16];
        b[0] = 0x39;
        b[1] = 0x30; // 12345
        b[14] = 2; // scale 2
        assert_eq!(decimal(b), "123.45");
        b[15] = 0x80;
        assert_eq!(decimal(b), "-123.45");
        assert_eq!(oa_date(0.0), "1899-12-30 00:00:00.000");
        assert_eq!(oa_date(45_000.5), "2023-03-15 12:00:00.000");
        let g = [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        assert_eq!(guid(g), "00112233-4455-6677-8899-AABBCCDDEEFF");
        assert_eq!(retention(6, Some("MONTH")), Some("6 MONTHS".into()));
        assert_eq!(retention(-1, None), None);
        assert_eq!(two_part("[Sales].[Order]"), Some(ObjectName::new("Sales", "Order")));
        assert_eq!(requote("dbo.X"), "[dbo].[X]");
    }

    #[test]
    fn header_errors() {
        assert!(!is_snp(b"PK\x03\x04"));
        assert_eq!(read(b"").unwrap_err(), "not a SQL Compare snapshot");
        let mut bytes = s(MAGIC);
        assert!(is_snp(&bytes));
        bytes.extend(22f32.to_le_bytes());
        assert_eq!(read(&bytes).unwrap_err(), "snapshot version 22 is not supported");
        let mut bytes = s(MAGIC);
        bytes.extend(23f32.to_le_bytes());
        bytes.extend([0; 3]);
        assert_eq!(read(&bytes).unwrap_err(), "truncated snapshot at offset 16");
    }

    /// Reads the snapshot SQL Compare made of tests/fixtures/compare/shop.sql, when
    /// QUILL_TEST_SNP points to it.
    #[test]
    fn reads_shop_snapshot() {
        let Ok(path) = std::env::var("QUILL_TEST_SNP") else { return };
        let bytes = std::fs::read(&path).unwrap();
        assert!(is_snp(&bytes));
        let db = read(&bytes).unwrap();
        assert_eq!(db.name, "QD_sources_Shop");
        assert_eq!(db.compatibility_level, 160);
        assert_eq!(db.collation, "SQL_Latin1_General_CP1_CI_AS");
        assert_eq!(db.server.major_version, 16);
        assert_eq!(db.schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["dbo", "Sales"]);
        let names: Vec<String> = db.tables.iter().map(|t| t.object_name().to_string()).collect();
        assert_eq!(names, ["[dbo].[Customer]", "[Sales].[Order]", "[Sales].[OrderLine]"]);

        let customer = db.table("dbo", "Customer").unwrap();
        let pk = customer.primary_key().unwrap();
        assert_eq!((pk.name.as_str(), pk.kind.clone(), pk.unique), ("PK_Customer", IndexKind::Clustered, true));
        assert_eq!(customer.column("Name").unwrap().collation.as_deref(), Some("Latin1_General_CI_AS"));
        assert_eq!(customer.column("Phone").unwrap().data_type.sql(), "[dbo].[Phone]");
        assert_eq!(customer.column("Balance").unwrap().default.as_ref().unwrap().name, "DF_Customer_Balance");
        assert!(customer.column("CreatedAt").unwrap().default.as_ref().unwrap().system_named);
        assert!(customer.column("NameLength").unwrap().computed.as_ref().unwrap().persisted);
        assert_eq!(customer.check_constraints[0].definition, "([Balance]>=(0))");
        let ix = customer.indexes.iter().find(|i| i.name == "IX_Customer_Name").unwrap();
        assert_eq!((ix.fill_factor, ix.included.clone(), ix.columns[0].descending), (90, vec!["Email".to_string()], true));
        assert_eq!(customer.text_filegroup.as_deref(), Some("PRIMARY"));

        let order = db.table("Sales", "Order").unwrap();
        assert_eq!(order.foreign_keys[0].on_delete, "CASCADE");
        assert_eq!(order.storage.compression[0].compression, "PAGE");
        assert!(order.check_constraints[0].system_named);
        assert_eq!(order.triggers[0].events, ["INSERT", "UPDATE"]);
        assert!(order.triggers[0].definition.starts_with("CREATE TRIGGER"));
        assert_eq!(order.text_filegroup, None);
        let line = db.table("Sales", "OrderLine").unwrap();
        assert!(line.foreign_keys[0].disabled && line.foreign_keys[0].not_trusted && line.foreign_keys[0].system_named);

        let view = &db.views[0];
        assert!(view.schema_bound);
        assert_eq!(view.indexes[0].name, "IX_CustomerTotals");
        assert!(view.indexes[0].unique);
        let kinds: Vec<_> = db.routines.iter().map(|r| (r.name.as_str(), r.kind.clone())).collect();
        assert_eq!(kinds, [("GetOrders", RoutineKind::Procedure), ("FullName", RoutineKind::ScalarFunction)]);

        assert_eq!(db.sequences[0].cache_size, Some(50));
        assert_eq!(db.synonyms[0].base_object, "[Sales].[Order]");
        assert_eq!(db.extended_properties.len(), 3);
        assert_eq!(db.types.len(), 2);

        let app = db.principals.iter().find(|p| p.name == "app").unwrap();
        assert_eq!((app.kind.as_str(), app.default_schema.as_deref()), ("SQL_USER", Some("Sales")));
        assert_eq!(app.member_of, ["db_datareader", "Reporting"]);
        assert_eq!(db.principals.len(), 2);
        assert!(db.permissions.iter().any(|p| p.class == "DATABASE" && p.grantee == "Reporting" && p.permission == "VIEW DEFINITION"));
        assert!(db.permissions.iter().any(|p| p.class == "SCHEMA" && p.schema.as_deref() == Some("Sales")));
        assert!(db.permissions.iter().any(|p| p.state == "DENY" && p.object.as_deref() == Some("Customer")));

        // Damaged copies fail with an error, never a panic.
        for cut in (0..bytes.len()).step_by(7) {
            assert!(read(&bytes[..cut]).is_err());
        }
        for i in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[i] ^= 0xFF;
            let _ = read(&damaged);
        }
    }
}
