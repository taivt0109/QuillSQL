//! Reads a live database into the model: a fixed set of set-based catalog queries on one
//! connection, never one query per object. Queries read under READ UNCOMMITTED so that a
//! long-running transaction on the server cannot block the comparison.

use std::collections::{BTreeMap, HashMap};

use tiberius::{ColumnData, Row};

use super::model::*;
use crate::db::{self, Connection, ConnectionSpec};

/// Reads the database named in `spec`.
pub fn read_database(spec: &ConnectionSpec) -> Result<Database, String> {
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut conn = db::connect_async(spec).await?;
        read(&mut conn).await
    })
}

/// Reads the connection's current database.
pub async fn read(conn: &mut Connection) -> Result<Database, String> {
    db::execute(conn, "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; SET LOCK_TIMEOUT 30000;").await?;
    let mut r = Reader { db: Database { model_version: MODEL_VERSION, ..Default::default() }, ..Default::default() };
    r.server_info(conn).await?;
    let v = Version::of(&r.db.server);
    r.schemas(conn).await?;
    r.objects(conn).await?;
    r.types(conn, &v).await?;
    r.tables(conn, &v).await?;
    r.columns(conn, &v).await?;
    r.indexes(conn, &v).await?;
    r.partitions(conn).await?;
    r.foreign_keys(conn).await?;
    r.check_constraints(conn).await?;
    r.modules(conn, &v).await?;
    r.triggers(conn).await?;
    r.sequences(conn).await?;
    r.synonyms(conn).await?;
    r.extended_properties(conn).await?;
    r.principals(conn).await?;
    r.permissions(conn).await?;
    r.dependencies(conn).await?;
    Ok(r.finish())
}

/// Which catalog columns exist. Azure SQL Database has every feature of the newest box version.
struct Version {
    major: u32,
}

impl Version {
    fn of(server: &ServerInfo) -> Version {
        let major = if server.is_azure_sql_database() || server.engine_edition == 8 { 99 } else { server.major_version };
        Version { major }
    }

    /// `expr` when the server is at least `major`, else `fallback`.
    fn pick<'a>(&self, major: u32, expr: &'a str, fallback: &'a str) -> &'a str {
        if self.major >= major { expr } else { fallback }
    }
}

const SQL2014: u32 = 12;
const SQL2016: u32 = 13;
const SQL2017: u32 = 14;
const SQL2019: u32 = 15;

/// Excludes objects shipped by Microsoft, including the database diagram support objects.
const USER_OBJECT: &str = "o.is_ms_shipped = 0 AND NOT EXISTS (SELECT 1 FROM sys.extended_properties AS xp \
     WHERE xp.class = 1 AND xp.major_id = o.object_id AND xp.minor_id = 0 AND xp.name = N'microsoft_database_tools_support')";

#[derive(Default)]
struct Reader {
    db: Database,
    /// object_id → (kind, name), for every object the model may refer to.
    objects: HashMap<i32, (String, ObjectName)>,
    /// object_id of a table, view or table type → where its columns and indexes go.
    owners: HashMap<i32, Owner>,
    /// (object_id, index_id) → (owner, index position).
    index_slots: HashMap<(i32, i32), (Owner, usize)>,
    /// schema_id → name.
    schema_names: HashMap<i32, String>,
    /// object_id → parent_object_id, for constraints and triggers.
    parents: HashMap<i32, i32>,
    /// object_id → definition text from sys.sql_modules.
    modules: HashMap<i32, Module>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Table(usize),
    View(usize),
    TableType(usize),
}

#[derive(Default)]
struct Module {
    definition: Option<String>,
}

// Column readers. Every query casts to a small set of types, but SQL Server picks the
// integer width for constants and catalog columns, so integers accept any width.

fn text(row: &Row, i: usize) -> Option<String> {
    match row.get_column_data(i).ok()? {
        ColumnData::String(Some(s)) => Some(s.to_string()),
        ColumnData::Xml(Some(x)) => Some(x.to_string()),
        _ => None,
    }
}

fn s(row: &Row, i: usize) -> String {
    text(row, i).unwrap_or_default()
}

fn int(row: &Row, i: usize) -> Option<i64> {
    match row.get_column_data(i).ok()? {
        ColumnData::U8(v) => v.map(i64::from),
        ColumnData::I16(v) => v.map(i64::from),
        ColumnData::I32(v) => v.map(i64::from),
        ColumnData::I64(v) => *v,
        ColumnData::Bit(v) => v.map(i64::from),
        ColumnData::Numeric(v) => v.map(|n| n.int_part() as i64),
        _ => None,
    }
}

fn float(row: &Row, i: usize) -> Option<f64> {
    match row.get_column_data(i).ok()? {
        ColumnData::F64(v) => *v,
        ColumnData::F32(v) => v.map(f64::from),
        _ => None,
    }
}

fn i(row: &Row, idx: usize) -> i32 {
    int(row, idx).unwrap_or(0) as i32
}

fn flag(row: &Row, i: usize) -> bool {
    int(row, i).unwrap_or(0) != 0
}

impl Reader {
    async fn server_info(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)),
                    CAST(SERVERPROPERTY('Edition') AS nvarchar(128)),
                    CAST(SERVERPROPERTY('EngineEdition') AS int),
                    DB_NAME(),
                    CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS nvarchar(128)),
                    (SELECT CAST(compatibility_level AS int) FROM sys.databases WHERE database_id = DB_ID())",
        )
        .await?;
        let row = rows.first().ok_or("the server returned no version information")?;
        let product_version = s(row, 0);
        let major_version = product_version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        if major_version < 11 && i(row, 2) != 5 {
            return Err(format!("SQL Server {product_version} is not supported; QuillSQL Compare needs SQL Server 2012 or later"));
        }
        self.db.server = ServerInfo { major_version, product_version, edition: s(row, 1), engine_edition: i(row, 2) as u32 };
        self.db.name = s(row, 3);
        self.db.collation = s(row, 4);
        self.db.compatibility_level = i(row, 5) as u32;
        Ok(())
    }

    async fn schemas(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT s.name, USER_NAME(s.principal_id), s.schema_id FROM sys.schemas AS s
             WHERE s.schema_id < 16384 AND s.name NOT IN (N'sys', N'INFORMATION_SCHEMA', N'guest')
             ORDER BY s.name",
        )
        .await?;
        self.db.schemas = rows.iter().map(|r| Schema { name: s(r, 0), owner: s(r, 1) }).collect();
        self.schema_names = rows.iter().map(|r| (i(r, 2), s(r, 0))).collect();
        Ok(())
    }

    /// Names of every object, so later queries can resolve ids without joins.
    async fn objects(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT o.object_id, RTRIM(o.type), SCHEMA_NAME(o.schema_id), o.name, o.parent_object_id FROM sys.objects AS o
             WHERE o.type NOT IN ('S', 'IT', 'SQ')",
        )
        .await?;
        for r in &rows {
            self.objects.insert(i(r, 0), (s(r, 1), ObjectName::new(&s(r, 2), &s(r, 3))));
            if i(r, 4) != 0 {
                self.parents.insert(i(r, 0), i(r, 4));
            }
        }
        Ok(())
    }

    fn name_of(&self, id: i32) -> Option<ObjectName> {
        self.objects.get(&id).map(|(_, n)| n.clone())
    }

    async fn types(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT SCHEMA_NAME(t.schema_id), t.name, t.is_table_type, t.is_assembly_type,
                    TYPE_NAME(t.system_type_id), CAST(t.max_length AS int), CAST(t.precision AS int),
                    CAST(t.scale AS int), t.is_nullable, tt.type_table_object_id, {mo},
                    a.name COLLATE DATABASE_DEFAULT + N'.' + at.assembly_class COLLATE DATABASE_DEFAULT
             FROM sys.types AS t
             LEFT JOIN sys.table_types AS tt ON tt.user_type_id = t.user_type_id
             LEFT JOIN sys.assembly_types AS at ON at.user_type_id = t.user_type_id
             LEFT JOIN sys.assemblies AS a ON a.assembly_id = at.assembly_id
             WHERE t.is_user_defined = 1
             ORDER BY 1, 2",
            mo = v.pick(SQL2014, "ISNULL(tt.is_memory_optimized, 0)", "CAST(0 AS bit)"),
        );
        for r in db::query(conn, &sql).await? {
            let mut ty = UserType { schema: s(&r, 0), name: s(&r, 1), nullable: flag(&r, 8), ..Default::default() };
            if flag(&r, 2) {
                ty.kind = UserTypeKind::Table;
                ty.memory_optimized = flag(&r, 10);
                ty.nullable = false;
                self.owners.insert(i(&r, 9), Owner::TableType(self.db.types.len()));
            } else if flag(&r, 3) {
                ty.kind = UserTypeKind::Clr;
                ty.clr_class = text(&r, 11);
            } else {
                ty.base_type = Some(system_type(&s(&r, 4), i(&r, 5), i(&r, 6), i(&r, 7)));
            }
            self.db.types.push(ty);
        }
        Ok(())
    }

    async fn tables(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT t.object_id, SCHEMA_NAME(t.schema_id), t.name, t.lock_escalation_desc,
                    ds.name, RTRIM(ds.type), lob.name, fs.name,
                    {mo}, {durability}, {temporal}, {history}, {retention}, {node}, {edge},
                    CASE WHEN ct.object_id IS NULL THEN NULL ELSE CAST(ct.is_track_columns_updated_on AS int) END
             FROM sys.tables AS t
             JOIN sys.objects AS o ON o.object_id = t.object_id
             OUTER APPLY (SELECT TOP (1) i.data_space_id FROM sys.indexes AS i
                          WHERE i.object_id = t.object_id AND i.index_id < 2) AS hi
             LEFT JOIN sys.data_spaces AS ds ON ds.data_space_id = hi.data_space_id
             LEFT JOIN sys.data_spaces AS lob ON lob.data_space_id = t.lob_data_space_id
             LEFT JOIN sys.data_spaces AS fs ON fs.data_space_id = t.filestream_data_space_id
             LEFT JOIN sys.change_tracking_tables AS ct ON ct.object_id = t.object_id
             WHERE {user} {external}
             ORDER BY 2, 3",
            mo = v.pick(SQL2014, "t.is_memory_optimized", "CAST(0 AS bit)"),
            durability = v.pick(SQL2014, "t.durability_desc", "CAST(NULL AS nvarchar(60))"),
            temporal = v.pick(SQL2016, "CAST(t.temporal_type AS int)", "0"),
            history = v.pick(SQL2016, "t.history_table_id", "CAST(NULL AS int)"),
            retention = v.pick(
                SQL2017,
                "CASE WHEN t.history_retention_period IS NULL OR t.history_retention_period = -1 THEN NULL \
                 ELSE CAST(t.history_retention_period AS nvarchar(20)) + N' ' + t.history_retention_period_unit_desc + N'S' END",
                "CAST(NULL AS nvarchar(80))",
            ),
            node = v.pick(SQL2017, "t.is_node", "CAST(0 AS bit)"),
            edge = v.pick(SQL2017, "t.is_edge", "CAST(0 AS bit)"),
            user = USER_OBJECT,
            external = v.pick(SQL2016, "AND t.is_external = 0", ""),
        );
        let mut history: Vec<(usize, i32)> = Vec::new();
        for r in db::query(conn, &sql).await? {
            let mut t = Table {
                schema: s(&r, 1),
                name: s(&r, 2),
                lock_escalation: s(&r, 3),
                text_filegroup: text(&r, 6),
                filestream_filegroup: text(&r, 7),
                memory_optimized: flag(&r, 8),
                ..Default::default()
            };
            t.storage.data_space = text(&r, 4);
            if t.memory_optimized {
                t.durability = text(&r, 9);
                t.storage.data_space = None;
            }
            match i(&r, 10) {
                1 => t.history_table = true,
                2 => {
                    history.push((self.db.tables.len(), i(&r, 11)));
                    t.temporal = Some(Temporal { retention: text(&r, 12), ..Default::default() });
                }
                _ => {}
            }
            if flag(&r, 13) {
                t.graph = Some("NODE".into());
            } else if flag(&r, 14) {
                t.graph = Some("EDGE".into());
            }
            if let Some(track) = int(&r, 15) {
                t.change_tracking = Some(ChangeTracking { track_columns_updated: track != 0 });
            }
            self.owners.insert(i(&r, 0), Owner::Table(self.db.tables.len()));
            self.db.tables.push(t);
        }
        for (table, id) in history {
            if let (Some(name), Some(temporal)) = (self.name_of(id), self.db.tables[table].temporal.as_mut()) {
                temporal.history_table = name;
            }
        }
        // Views are owners of columns and indexes too.
        let views = db::query(
            conn,
            &format!(
                "SELECT o.object_id, SCHEMA_NAME(o.schema_id), o.name FROM sys.views AS o WHERE {USER_OBJECT} ORDER BY 2, 3"
            ),
        )
        .await?;
        for r in views {
            self.owners.insert(i(&r, 0), Owner::View(self.db.views.len()));
            self.db.views.push(View { schema: s(&r, 1), name: s(&r, 2), ..Default::default() });
        }
        Ok(())
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

    async fn columns(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT c.object_id, c.column_id, c.name,
                    ty.name, CASE WHEN ty.is_user_defined = 1 THEN SCHEMA_NAME(ty.schema_id) END,
                    TYPE_NAME(c.system_type_id), CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int),
                    c.collation_name, c.is_nullable, c.is_rowguidcol, c.is_sparse, c.is_column_set, c.is_filestream,
                    c.is_xml_document, QUOTENAME(SCHEMA_NAME(xc.schema_id)) + N'.' + QUOTENAME(xc.name),
                    CONVERT(nvarchar(40), ic.seed_value), CONVERT(nvarchar(40), ic.increment_value),
                    ic.is_not_for_replication, c.is_identity,
                    cc.definition, cc.is_persisted,
                    dc.name, dc.definition, dc.is_system_named,
                    {generated}, {hidden}, {masked},
                    QUOTENAME(SCHEMA_NAME(bd.schema_id)) + N'.' + QUOTENAME(bd.name),
                    QUOTENAME(OBJECT_SCHEMA_NAME(NULLIF(c.rule_object_id, 0))) + N'.' + QUOTENAME(OBJECT_NAME(NULLIF(c.rule_object_id, 0)))
             FROM sys.columns AS c
             JOIN sys.types AS ty ON ty.user_type_id = c.user_type_id
             LEFT JOIN sys.xml_schema_collections AS xc ON xc.xml_collection_id = c.xml_collection_id AND c.xml_collection_id <> 0
             LEFT JOIN sys.identity_columns AS ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id
             LEFT JOIN sys.computed_columns AS cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
             LEFT JOIN sys.default_constraints AS dc ON dc.object_id = c.default_object_id AND dc.parent_object_id = c.object_id
             LEFT JOIN sys.objects AS bd ON bd.object_id = c.default_object_id AND bd.type = 'D' AND bd.parent_object_id = 0
             {masked_join}
             ORDER BY c.object_id, c.column_id",
            generated = v.pick(
                SQL2016,
                "CASE c.generated_always_type WHEN 1 THEN N'ROW START' WHEN 2 THEN N'ROW END' \
                 WHEN 7 THEN N'TRANSACTION_ID START' WHEN 8 THEN N'TRANSACTION_ID END' \
                 WHEN 9 THEN N'SEQUENCE_NUMBER START' WHEN 10 THEN N'SEQUENCE_NUMBER END' END",
                "CAST(NULL AS nvarchar(40))",
            ),
            hidden = v.pick(SQL2016, "c.is_hidden", "CAST(0 AS bit)"),
            masked = v.pick(SQL2016, "mc.masking_function", "CAST(NULL AS nvarchar(4000))"),
            masked_join = v.pick(
                SQL2016,
                "LEFT JOIN sys.masked_columns AS mc ON mc.object_id = c.object_id AND mc.column_id = c.column_id AND mc.is_masked = 1",
                "",
            ),
        );
        for r in db::query(conn, &sql).await? {
            let Some(&owner) = self.owners.get(&i(&r, 0)) else { continue };
            let schema = text(&r, 4);
            let mut data_type = if schema.is_some() {
                DataType { name: s(&r, 3), schema, ..Default::default() }
            } else {
                system_type(&s(&r, 3), i(&r, 6), i(&r, 7), i(&r, 8))
            };
            if data_type.name == "xml" {
                data_type.xml_collection = text(&r, 16);
                data_type.xml_document = flag(&r, 15);
            }
            // Alias types carry their own collation; COLLATE cannot be written on them.
            let character = data_type.schema.is_none()
                && matches!(s(&r, 5).as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname");
            let column = Column {
                name: s(&r, 2),
                ordinal: i(&r, 1) as u32,
                data_type,
                collation: if character { text(&r, 9) } else { None },
                nullable: flag(&r, 10),
                rowguidcol: flag(&r, 11),
                sparse: flag(&r, 12),
                column_set: flag(&r, 13),
                filestream: flag(&r, 14),
                identity: flag(&r, 20).then(|| Identity {
                    seed: s(&r, 17),
                    increment: s(&r, 18),
                    not_for_replication: flag(&r, 19),
                }),
                computed: text(&r, 21).map(|definition| Computed { definition, persisted: flag(&r, 22) }),
                default: text(&r, 23).map(|name| DefaultConstraint {
                    name,
                    definition: s(&r, 24),
                    system_named: flag(&r, 25),
                }),
                generated_always: text(&r, 26),
                hidden: flag(&r, 27),
                masking_function: text(&r, 28),
                bound_default: text(&r, 29),
                bound_rule: text(&r, 30),
            };
            self.columns_of(owner).push(column);
        }
        Ok(())
    }

    async fn indexes(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT i.object_id, i.index_id, i.name, CAST(i.type AS int), i.is_unique, i.is_primary_key,
                    i.is_unique_constraint, ISNULL(kc.is_system_named, 0), CAST(i.fill_factor AS int), i.is_padded,
                    i.ignore_dup_key, i.allow_row_locks, i.allow_page_locks, i.is_disabled, i.filter_definition,
                    ds.name, ISNULL(st.no_recompute, 0), {incremental}, {bucket}, {sequential}
             FROM sys.indexes AS i
             JOIN sys.objects AS o ON o.object_id = i.object_id AND o.type IN ('U', 'V', 'TT')
             LEFT JOIN sys.key_constraints AS kc ON kc.parent_object_id = i.object_id AND kc.unique_index_id = i.index_id
             LEFT JOIN sys.data_spaces AS ds ON ds.data_space_id = i.data_space_id
             LEFT JOIN sys.stats AS st ON st.object_id = i.object_id AND st.stats_id = i.index_id
             {hash_join}
             WHERE i.type > 0 AND i.is_hypothetical = 0
             ORDER BY i.object_id, i.index_id",
            incremental = v.pick(SQL2014, "ISNULL(st.is_incremental, 0)", "CAST(0 AS bit)"),
            bucket = v.pick(SQL2014, "CAST(hx.bucket_count AS bigint)", "CAST(NULL AS bigint)"),
            sequential = v.pick(SQL2019, "i.optimize_for_sequential_key", "CAST(0 AS bit)"),
            hash_join = v.pick(
                SQL2014,
                "LEFT JOIN sys.hash_indexes AS hx ON hx.object_id = i.object_id AND hx.index_id = i.index_id",
                "",
            ),
        );
        for r in db::query(conn, &sql).await? {
            let object = i(&r, 0);
            let Some(&owner) = self.owners.get(&object) else { continue };
            let kind = match i(&r, 3) {
                1 => IndexKind::Clustered,
                2 => IndexKind::Nonclustered,
                3 => IndexKind::Xml,
                4 => IndexKind::Spatial,
                5 => IndexKind::ClusteredColumnstore,
                6 => IndexKind::NonclusteredColumnstore,
                7 => IndexKind::NonclusteredHash,
                _ => continue,
            };
            let index = Index {
                name: s(&r, 2),
                kind,
                unique: flag(&r, 4),
                primary_key: flag(&r, 5),
                unique_constraint: flag(&r, 6),
                system_named: flag(&r, 7),
                fill_factor: i(&r, 8) as u8,
                pad_index: flag(&r, 9),
                ignore_dup_key: flag(&r, 10),
                allow_row_locks: flag(&r, 11),
                allow_page_locks: flag(&r, 12),
                disabled: flag(&r, 13),
                filter: text(&r, 14),
                storage: Storage { data_space: text(&r, 15), ..Default::default() },
                no_recompute: flag(&r, 16),
                incremental: flag(&r, 17),
                bucket_count: int(&r, 18),
                optimize_for_sequential_key: flag(&r, 19),
                ..Default::default()
            };
            let list = self.indexes_of(owner);
            list.push(index);
            let slot = list.len() - 1;
            self.index_slots.insert((object, i(&r, 1)), (owner, slot));
        }

        let rows = db::query(
            conn,
            "SELECT ic.object_id, ic.index_id, c.name, CAST(ic.key_ordinal AS int), ic.is_descending_key,
                    ic.is_included_column, CAST(ic.partition_ordinal AS int)
             FROM sys.index_columns AS ic
             JOIN sys.columns AS c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
             JOIN sys.objects AS o ON o.object_id = ic.object_id AND o.type IN ('U', 'V', 'TT')
             ORDER BY ic.object_id, ic.index_id, ic.key_ordinal, ic.index_column_id",
        )
        .await?;
        for r in rows {
            let (object, index_id) = (i(&r, 0), i(&r, 1));
            let name = s(&r, 2);
            if i(&r, 6) > 0 {
                // The partitioning column of the heap or clustered index partitions the table.
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
            let index = &mut self.indexes_of(owner)[slot];
            if flag(&r, 5) {
                index.included.push(name);
            } else if i(&r, 3) > 0 || matches!(index.kind, IndexKind::Xml | IndexKind::Spatial) {
                // Nonclustered columnstore columns are listed with key_ordinal 0 and no sort order.
                index.columns.push(IndexColumn { name, descending: flag(&r, 4) });
            } else if index.kind == IndexKind::NonclusteredColumnstore {
                index.columns.push(IndexColumn { name, descending: false });
            }
        }
        // Heaps: the table's own storage comes from index 0, which has no row in sys.indexes above.
        self.xml_and_spatial_indexes(conn, v).await
    }

    /// The parts of XML and spatial indexes that sys.indexes does not show.
    async fn xml_and_spatial_indexes(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        // Selective XML indexes came with SQL Server 2012 SP1.
        let selective = v.major >= SQL2014;
        let sql = format!(
            "SELECT x.object_id, x.index_id, {kind}, ui.name, x.secondary_type, {path}
             FROM sys.xml_indexes AS x
             LEFT JOIN sys.indexes AS ui ON ui.object_id = x.object_id AND ui.index_id = x.using_xml_index_id
             {path_join}",
            kind = if selective { "CAST(x.xml_index_type AS int)" } else { "CASE WHEN x.using_xml_index_id IS NULL THEN 0 ELSE 1 END" },
            path = if selective { "p.name" } else { "CAST(NULL AS sysname)" },
            path_join = if selective {
                "LEFT JOIN sys.selective_xml_index_paths AS p ON p.object_id = x.object_id AND p.index_id = x.using_xml_index_id AND p.path_id = x.path_id"
            } else {
                ""
            },
        );
        for r in db::query(conn, &sql).await? {
            let Some(&(owner, slot)) = self.index_slots.get(&(i(&r, 0), i(&r, 1))) else { continue };
            let kind = match i(&r, 2) {
                1 => XmlIndexKind::Secondary,
                2 => XmlIndexKind::Selective,
                3 => XmlIndexKind::SecondarySelective,
                _ => XmlIndexKind::Primary,
            };
            let secondary_type = match s(&r, 4).as_str() {
                "V" => Some("VALUE".to_string()),
                "P" => Some("PATH".to_string()),
                "R" => Some("PROPERTY".to_string()),
                _ => None,
            };
            let xml = XmlIndex { kind, secondary_type, using_index: text(&r, 3), using_path: text(&r, 5), ..Default::default() };
            self.indexes_of(owner)[slot].xml = Some(xml);
        }
        if selective {
            let rows = db::query(
                conn,
                "SELECT p.object_id, p.index_id, p.path_id, p.name, p.path, CAST(p.path_type AS int),
                        p.xquery_type_description, CAST(p.xquery_max_length AS int), p.is_node, p.is_singleton,
                        t.name, CAST(p.max_length AS int),
                        CAST(p.precision AS int), CAST(p.scale AS int)
                 FROM sys.selective_xml_index_paths AS p
                 LEFT JOIN sys.types AS t ON t.user_type_id = p.user_type_id
                 ORDER BY p.object_id, p.index_id, p.path_id",
            )
            .await?;
            for r in rows {
                let Some(&(owner, slot)) = self.index_slots.get(&(i(&r, 0), i(&r, 1))) else { continue };
                let sql = i(&r, 5) == 1;
                let path = SelectivePath {
                    id: i(&r, 2),
                    name: s(&r, 3),
                    path: s(&r, 4),
                    sql,
                    xquery_type: if sql { None } else { text(&r, 6) },
                    max_length: if sql { None } else { int(&r, 7).map(|n| n as i32).filter(|n| *n > 0) },
                    node_hint: flag(&r, 8),
                    singleton: flag(&r, 9),
                    data_type: (sql && !s(&r, 10).is_empty()).then(|| system_type(&s(&r, 10), i(&r, 11), i(&r, 12), i(&r, 13))),
                };
                if let Some(x) = self.indexes_of(owner)[slot].xml.as_mut() {
                    x.paths.push(path);
                }
            }
            let rows = db::query(
                conn,
                "SELECT n.object_id, n.index_id, n.is_default_uri, n.prefix, n.uri
                 FROM sys.selective_xml_index_namespaces AS n ORDER BY n.object_id, n.index_id, n.prefix",
            )
            .await?;
            for r in rows {
                let Some(&(owner, slot)) = self.index_slots.get(&(i(&r, 0), i(&r, 1))) else { continue };
                let ns = XmlNamespace { prefix: if flag(&r, 2) { None } else { text(&r, 3) }, uri: s(&r, 4) };
                if let Some(x) = self.indexes_of(owner)[slot].xml.as_mut() {
                    x.namespaces.push(ns);
                }
            }
        }
        let rows = db::query(
            conn,
            "SELECT si.object_id, si.index_id, CAST(si.spatial_index_type AS int), st.tessellation_scheme,
                    CONVERT(float, st.bounding_box_xmin), CONVERT(float, st.bounding_box_ymin),
                    CONVERT(float, st.bounding_box_xmax), CONVERT(float, st.bounding_box_ymax),
                    st.level_1_grid_desc, st.level_2_grid_desc, st.level_3_grid_desc, st.level_4_grid_desc,
                    CAST(st.cells_per_object AS int)
             FROM sys.spatial_indexes AS si
             LEFT JOIN sys.spatial_index_tessellations AS st ON st.object_id = si.object_id AND st.index_id = si.index_id",
        )
        .await?;
        for r in rows {
            let Some(&(owner, slot)) = self.index_slots.get(&(i(&r, 0), i(&r, 1))) else { continue };
            // 3 and 4 are the auto-grid variants of geometry and geography.
            let geography = matches!(i(&r, 2), 2 | 4);
            let bounding_box = if geography {
                Vec::new()
            } else {
                (4..8).filter_map(|c| float(&r, c)).map(|f| f.to_string()).collect()
            };
            let spatial = SpatialIndex {
                geography,
                tessellation: s(&r, 3),
                bounding_box,
                grids: (8..12).map(|c| text(&r, c).unwrap_or_else(|| "MEDIUM".into())).collect(),
                cells_per_object: int(&r, 12).unwrap_or(16) as i32,
            };
            self.indexes_of(owner)[slot].spatial = Some(spatial);
        }
        Ok(())
    }

    /// Data compression per partition of tables and indexes.
    async fn partitions(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT p.object_id, p.index_id, p.partition_number, p.data_compression_desc,
                    COUNT(*) OVER (PARTITION BY p.object_id, p.index_id)
             FROM sys.partitions AS p
             JOIN sys.objects AS o ON o.object_id = p.object_id AND o.type IN ('U', 'V')
             ORDER BY p.object_id, p.index_id, p.partition_number",
        )
        .await?;
        for r in rows {
            let (object, index_id, partitions) = (i(&r, 0), i(&r, 1), i(&r, 4));
            let compression = s(&r, 3);
            if partitions <= 1 && compression == "NONE" {
                continue;
            }
            let entry = PartitionCompression { partition: i(&r, 2) as u32, compression };
            if index_id <= 1
                && let Some(Owner::Table(t)) = self.owners.get(&object).copied()
            {
                self.db.tables[t].storage.compression.push(entry.clone());
            }
            if let Some(&(owner, slot)) = self.index_slots.get(&(object, index_id)) {
                self.indexes_of(owner)[slot].storage.compression.push(entry);
            }
        }
        // Keep one entry when every partition is compressed the same way.
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
        Ok(())
    }

    async fn foreign_keys(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT fk.object_id, fk.parent_object_id, fk.name, fk.is_system_named, fk.referenced_object_id,
                    fk.delete_referential_action_desc, fk.update_referential_action_desc,
                    fk.is_disabled, fk.is_not_trusted, fk.is_not_for_replication
             FROM sys.foreign_keys AS fk ORDER BY fk.object_id",
        )
        .await?;
        let mut slots: HashMap<i32, (usize, usize)> = HashMap::new();
        for r in rows {
            let Some(Owner::Table(t)) = self.owners.get(&i(&r, 1)).copied() else { continue };
            let Some(referenced) = self.name_of(i(&r, 4)) else { continue };
            let table = &mut self.db.tables[t];
            table.foreign_keys.push(ForeignKey {
                name: s(&r, 2),
                system_named: flag(&r, 3),
                referenced_table: referenced,
                on_delete: s(&r, 5),
                on_update: s(&r, 6),
                disabled: flag(&r, 7),
                not_trusted: flag(&r, 8),
                not_for_replication: flag(&r, 9),
                ..Default::default()
            });
            slots.insert(i(&r, 0), (t, table.foreign_keys.len() - 1));
        }
        let rows = db::query(
            conn,
            "SELECT fkc.constraint_object_id, pc.name, rc.name
             FROM sys.foreign_key_columns AS fkc
             JOIN sys.columns AS pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
             JOIN sys.columns AS rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
             ORDER BY fkc.constraint_object_id, fkc.constraint_column_id",
        )
        .await?;
        for r in rows {
            if let Some(&(t, k)) = slots.get(&i(&r, 0)) {
                let fk = &mut self.db.tables[t].foreign_keys[k];
                fk.columns.push(s(&r, 1));
                fk.referenced_columns.push(s(&r, 2));
            }
        }
        Ok(())
    }

    async fn check_constraints(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT cc.parent_object_id, cc.name, cc.is_system_named, cc.definition,
                    CASE WHEN cc.parent_column_id > 0 THEN COL_NAME(cc.parent_object_id, cc.parent_column_id) END,
                    cc.is_disabled, cc.is_not_trusted, cc.is_not_for_replication
             FROM sys.check_constraints AS cc ORDER BY cc.parent_object_id, cc.name",
        )
        .await?;
        for r in rows {
            let check = CheckConstraint {
                name: s(&r, 1),
                system_named: flag(&r, 2),
                definition: s(&r, 3),
                column: text(&r, 4),
                disabled: flag(&r, 5),
                not_trusted: flag(&r, 6),
                not_for_replication: flag(&r, 7),
            };
            match self.owners.get(&i(&r, 0)).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].check_constraints.push(check),
                Some(Owner::TableType(t)) => self.db.types[t].check_constraints.push(check),
                _ => {}
            }
        }
        Ok(())
    }

    async fn modules(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT o.object_id, RTRIM(o.type), SCHEMA_NAME(o.schema_id), o.name,
                    m.definition, ISNULL(m.uses_ansi_nulls, 1), ISNULL(m.uses_quoted_identifier, 1),
                    ISNULL(m.is_schema_bound, 0),
                    CASE WHEN m.execute_as_principal_id = -2 OR am.execute_as_principal_id = -2 THEN N'OWNER'
                         ELSE USER_NAME(COALESCE(m.execute_as_principal_id, am.execute_as_principal_id)) END,
                    {native},
                    CASE WHEN am.object_id IS NOT NULL THEN QUOTENAME(a.name) COLLATE DATABASE_DEFAULT + N'.' + QUOTENAME(am.assembly_class) COLLATE DATABASE_DEFAULT + N'.' + QUOTENAME(am.assembly_method) COLLATE DATABASE_DEFAULT END
             FROM sys.objects AS o
             LEFT JOIN sys.sql_modules AS m ON m.object_id = o.object_id
             LEFT JOIN sys.assembly_modules AS am ON am.object_id = o.object_id
             LEFT JOIN sys.assemblies AS a ON a.assembly_id = am.assembly_id
             WHERE o.type IN ('P', 'PC', 'FN', 'IF', 'TF', 'FS', 'FT', 'AF', 'V', 'TR', 'TA') AND {user}
             ORDER BY 3, 4",
            native = v.pick(SQL2014, "ISNULL(m.uses_native_compilation, 0)", "CAST(0 AS bit)"),
            user = USER_OBJECT,
        );
        for r in db::query(conn, &sql).await? {
            let id = i(&r, 0);
            let kind = s(&r, 1);
            let definition = text(&r, 4);
            let encrypted = definition.is_none() && !matches!(kind.as_str(), "PC" | "FS" | "FT" | "AF" | "TA");
            let settings = ModuleSettings { ansi_nulls: flag(&r, 5), quoted_identifier: flag(&r, 6) };
            match kind.as_str() {
                "V" => {
                    if let Some(Owner::View(vi)) = self.owners.get(&id).copied() {
                        let view = &mut self.db.views[vi];
                        view.definition = definition.clone().unwrap_or_default();
                        view.encrypted = encrypted;
                        view.schema_bound = flag(&r, 7);
                        view.settings = settings;
                    }
                }
                "TR" | "TA" => {}
                _ => {
                    let kind = match kind.as_str() {
                        "P" => RoutineKind::Procedure,
                        "PC" => RoutineKind::ClrProcedure,
                        "FN" => RoutineKind::ScalarFunction,
                        "IF" => RoutineKind::InlineTableFunction,
                        "TF" => RoutineKind::TableFunction,
                        "FS" => RoutineKind::ClrScalarFunction,
                        "FT" => RoutineKind::ClrTableFunction,
                        _ => RoutineKind::ClrAggregate,
                    };
                    self.db.routines.push(Routine {
                        schema: s(&r, 2),
                        name: s(&r, 3),
                        kind,
                        definition: definition.clone().unwrap_or_default(),
                        encrypted,
                        schema_bound: flag(&r, 7),
                        settings,
                        execute_as: text(&r, 8),
                        native_compilation: flag(&r, 9),
                        clr_method: text(&r, 10),
                    });
                }
            }
            self.modules.insert(id, Module { definition });
        }
        Ok(())
    }

    async fn triggers(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT tr.object_id, tr.parent_id, tr.name, tr.is_disabled, tr.is_instead_of_trigger,
                    tr.is_not_for_replication,
                    MAX(CASE WHEN te.type = 1 THEN 1 ELSE 0 END), MAX(CASE WHEN te.type = 2 THEN 1 ELSE 0 END),
                    MAX(CASE WHEN te.type = 3 THEN 1 ELSE 0 END),
                    STUFF((SELECT N',' + te2.type_desc COLLATE DATABASE_DEFAULT + N':' + CASE WHEN te2.is_first = 1 THEN N'FIRST' ELSE N'LAST' END
                           FROM sys.trigger_events AS te2
                           WHERE te2.object_id = tr.object_id AND (te2.is_first = 1 OR te2.is_last = 1)
                           ORDER BY te2.type FOR XML PATH('')), 1, 1, N'')
             FROM sys.triggers AS tr
             LEFT JOIN sys.trigger_events AS te ON te.object_id = tr.object_id
             WHERE tr.parent_class = 1 AND tr.is_ms_shipped = 0
             GROUP BY tr.object_id, tr.parent_id, tr.name, tr.is_disabled, tr.is_instead_of_trigger, tr.is_not_for_replication
             ORDER BY tr.parent_id, tr.name",
        )
        .await?;
        for r in rows {
            let definition = self.modules.get(&i(&r, 0)).and_then(|m| m.definition.clone());
            let mut events = Vec::new();
            for (col, event) in [(6, "INSERT"), (7, "UPDATE"), (8, "DELETE")] {
                if flag(&r, col) {
                    events.push(event.to_string());
                }
            }
            let order = s(&r, 9)
                .split(',')
                .filter_map(|p| p.split_once(':'))
                .map(|(e, o)| (e.to_string(), o.to_string()))
                .collect();
            let trigger = Trigger {
                name: s(&r, 2),
                encrypted: definition.is_none(),
                definition: definition.unwrap_or_default(),
                disabled: flag(&r, 3),
                instead_of: flag(&r, 4),
                not_for_replication: flag(&r, 5),
                events,
                order,
            };
            match self.owners.get(&i(&r, 1)).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].triggers.push(trigger),
                Some(Owner::View(v)) => self.db.views[v].triggers.push(trigger),
                _ => {}
            }
        }
        let rows = db::query(
            conn,
            "SELECT tr.name, OBJECT_DEFINITION(tr.object_id), tr.is_disabled
             FROM sys.triggers AS tr WHERE tr.parent_class = 0 AND tr.is_ms_shipped = 0 ORDER BY tr.name",
        )
        .await?;
        for r in rows {
            let definition = text(&r, 1);
            self.db.ddl_triggers.push(DdlTrigger {
                name: s(&r, 0),
                encrypted: definition.is_none(),
                definition: definition.unwrap_or_default(),
                disabled: flag(&r, 2),
            });
        }
        Ok(())
    }

    async fn sequences(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT SCHEMA_NAME(sq.schema_id), sq.name, TYPE_NAME(sq.user_type_id),
                    CASE WHEN ty.is_user_defined = 1 THEN SCHEMA_NAME(ty.schema_id) END,
                    CAST(sq.precision AS int), CAST(sq.scale AS int),
                    CONVERT(nvarchar(64), sq.start_value), CONVERT(nvarchar(64), sq.increment),
                    CONVERT(nvarchar(64), sq.minimum_value), CONVERT(nvarchar(64), sq.maximum_value),
                    sq.is_cycling, sq.is_cached, sq.cache_size
             FROM sys.sequences AS sq JOIN sys.types AS ty ON ty.user_type_id = sq.user_type_id
             ORDER BY 1, 2",
        )
        .await?;
        for r in rows {
            let schema = text(&r, 3);
            let data_type = if schema.is_some() {
                DataType { name: s(&r, 2), schema, ..Default::default() }
            } else {
                system_type(&s(&r, 2), 0, i(&r, 4), i(&r, 5))
            };
            self.db.sequences.push(Sequence {
                schema: s(&r, 0),
                name: s(&r, 1),
                data_type,
                start: s(&r, 6),
                increment: s(&r, 7),
                minimum: s(&r, 8),
                maximum: s(&r, 9),
                cycle: flag(&r, 10),
                cached: flag(&r, 11),
                cache_size: if flag(&r, 11) { int(&r, 12) } else { None },
            });
        }
        Ok(())
    }

    async fn synonyms(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT SCHEMA_NAME(o.schema_id), o.name, o.base_object_name FROM sys.synonyms AS o
             WHERE o.is_ms_shipped = 0 ORDER BY 1, 2",
        )
        .await?;
        for r in rows {
            self.db.synonyms.push(Synonym { schema: s(&r, 0), name: s(&r, 1), base_object: s(&r, 2) });
        }
        Ok(())
    }

    async fn extended_properties(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT ep.class, ep.major_id, ep.minor_id, ep.name, CONVERT(nvarchar(max), ep.value),
                    CASE ep.class WHEN 3 THEN SCHEMA_NAME(ep.major_id)
                                  WHEN 6 THEN (SELECT SCHEMA_NAME(t.schema_id) + NCHAR(1) + t.name FROM sys.types AS t WHERE t.user_type_id = ep.major_id)
                                  WHEN 7 THEN (SELECT x.name FROM sys.indexes AS x WHERE x.object_id = ep.major_id AND x.index_id = ep.minor_id)
                                  WHEN 2 THEN (SELECT p.name FROM sys.parameters AS p WHERE p.object_id = ep.major_id AND p.parameter_id = ep.minor_id)
                                  WHEN 1 THEN CASE WHEN ep.minor_id > 0 THEN COL_NAME(ep.major_id, ep.minor_id) END END
             FROM sys.extended_properties AS ep
             WHERE ep.class IN (0, 1, 2, 3, 6, 7)
             ORDER BY ep.class, ep.major_id, ep.minor_id, ep.name",
        )
        .await?;
        for r in rows {
            let mut p = ExtendedProperty { name: s(&r, 3), value: text(&r, 4), ..Default::default() };
            let (major, minor, extra) = (i(&r, 1), i(&r, 2), text(&r, 5));
            match i(&r, 0) {
                0 => p.class = "DATABASE".into(),
                3 => {
                    p.class = "SCHEMA".into();
                    p.schema = extra;
                }
                6 => {
                    let Some((schema, name)) = extra.as_deref().and_then(|e| e.split_once('\u{1}')) else { continue };
                    p.class = "TYPE".into();
                    p.schema = Some(schema.to_string());
                    p.object = Some(name.to_string());
                }
                class @ (1 | 2 | 7) => {
                    let Some((kind, name)) = self.objects.get(&major).cloned() else { continue };
                    if kind == "S" || kind == "IT" {
                        continue;
                    }
                    // Constraints and triggers hang off their table at level 2.
                    let parent = if matches!(kind.as_str(), "PK" | "UQ" | "F" | "C" | "D" | "TR" | "TA") {
                        self.parent_of(major)
                    } else {
                        None
                    };
                    if let Some((parent_kind, parent)) = parent {
                        p.class = level1_class(&parent_kind).into();
                        p.schema = Some(parent.schema);
                        p.object = Some(parent.name);
                        p.minor_type = Some(if kind.starts_with('T') { "TRIGGER" } else { "CONSTRAINT" }.into());
                        p.minor_name = Some(name.name);
                    } else {
                        p.class = level1_class(&kind).into();
                        p.schema = Some(name.schema);
                        p.object = Some(name.name);
                        match class {
                            1 if minor > 0 => {
                                p.minor_type = Some("COLUMN".into());
                                p.minor_name = extra;
                            }
                            2 => {
                                p.minor_type = Some("PARAMETER".into());
                                p.minor_name = extra;
                            }
                            7 => {
                                p.minor_type = Some("INDEX".into());
                                p.minor_name = extra;
                            }
                            _ => {}
                        }
                    }
                }
                _ => continue,
            }
            self.db.extended_properties.push(p);
        }
        Ok(())
    }

    async fn principals(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT p.name, p.type_desc, SUSER_SNAME(p.sid), p.default_schema_name, USER_NAME(p.owning_principal_id),
                    CASE WHEN p.type IN ('S', 'U', 'G') AND p.authentication_type = 0 THEN 1 ELSE 0 END
             FROM sys.database_principals AS p
             WHERE p.type IN ('S', 'U', 'G', 'E', 'X', 'C', 'K', 'R', 'A') AND p.is_fixed_role = 0
               AND p.name NOT IN (N'public', N'dbo', N'guest', N'sys', N'INFORMATION_SCHEMA') AND p.principal_id < 16384
             ORDER BY p.name",
        )
        .await?;
        for r in rows {
            let kind = s(&r, 1);
            let without_login = flag(&r, 5);
            let role = kind.ends_with("_ROLE");
            self.db.principals.push(Principal {
                name: s(&r, 0),
                login: if role || without_login { None } else { text(&r, 2) },
                default_schema: if role { None } else { text(&r, 3) },
                owner: if role { text(&r, 4) } else { None },
                kind,
                member_of: Vec::new(),
            });
        }
        let rows = db::query(
            conn,
            "SELECT m.name, r.name FROM sys.database_role_members AS rm
             JOIN sys.database_principals AS r ON r.principal_id = rm.role_principal_id
             JOIN sys.database_principals AS m ON m.principal_id = rm.member_principal_id
             ORDER BY m.name, r.name",
        )
        .await?;
        for r in rows {
            let (member, role) = (s(&r, 0), s(&r, 1));
            if let Some(p) = self.db.principals.iter_mut().find(|p| p.name == member) {
                p.member_of.push(role);
            }
        }
        Ok(())
    }

    async fn permissions(&mut self, conn: &mut Connection) -> Result<(), String> {
        // CONNECT for users comes with CREATE USER; column-level permissions are not modelled yet.
        let rows = db::query(
            conn,
            "SELECT p.class, p.major_id, p.permission_name, p.state_desc, g.name
             FROM sys.database_permissions AS p
             JOIN sys.database_principals AS g ON g.principal_id = p.grantee_principal_id
             WHERE p.class IN (0, 1, 3) AND p.minor_id = 0 AND g.name NOT IN (N'dbo', N'sys', N'INFORMATION_SCHEMA')
               AND NOT (p.class = 0 AND p.permission_name = 'CONNECT')
             ORDER BY p.class, p.major_id, g.name, p.permission_name",
        )
        .await?;
        for r in rows {
            let mut p = Permission { grantee: s(&r, 4), permission: s(&r, 2), state: s(&r, 3), ..Default::default() };
            match i(&r, 0) {
                0 => p.class = "DATABASE".into(),
                1 => {
                    let Some((kind, name)) = self.objects.get(&i(&r, 1)).cloned() else { continue };
                    if kind == "S" || kind == "IT" {
                        continue;
                    }
                    p.class = "OBJECT".into();
                    p.schema = Some(name.schema);
                    p.object = Some(name.name);
                }
                3 => {
                    let Some(schema) = self.schema_names.get(&i(&r, 1)).cloned() else { continue };
                    p.class = "SCHEMA".into();
                    p.schema = Some(schema);
                }
                _ => continue,
            }
            self.db.permissions.push(p);
        }
        Ok(())
    }

    fn parent_of(&self, id: i32) -> Option<(String, ObjectName)> {
        self.parents.get(&id).and_then(|p| self.objects.get(p)).cloned()
    }

    async fn dependencies(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT DISTINCT d.referencing_id, d.referenced_id, d.is_schema_bound_reference
             FROM sys.sql_expression_dependencies AS d
             WHERE d.referencing_class = 1 AND d.referenced_class = 1 AND d.referenced_id IS NOT NULL
               AND d.referencing_id <> d.referenced_id",
        )
        .await?;
        let mut deps = BTreeMap::new();
        for r in rows {
            // A constraint's or default's references belong to its table.
            let referencing = i(&r, 0);
            let from_id = match self.objects.get(&referencing) {
                Some((kind, _)) if matches!(kind.as_str(), "C" | "D" | "PK" | "UQ" | "F") => {
                    self.parents.get(&referencing).copied().unwrap_or(referencing)
                }
                _ => referencing,
            };
            let (Some(from), Some(to)) = (self.name_of(from_id), self.name_of(i(&r, 1))) else { continue };
            if from == to || from.schema == "sys" || to.schema == "sys" {
                continue;
            }
            let strong = flag(&r, 2);
            let entry = deps.entry((from, to)).or_insert(false);
            *entry |= strong;
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
        for syn in &self.db.synonyms {
            if let Some(target) = parse_synonym_target(&syn.base_object, &self.db.name) {
                deps.entry((ObjectName::new(&syn.schema, &syn.name), target)).or_insert(false);
            }
        }
        self.db.dependencies =
            deps.into_iter().map(|((from, to), strong)| Dependency { from, to, strong }).collect();
        Ok(())
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
            // TEXTIMAGE_ON only matters when there is a LOB column.
            if !t.columns.iter().any(is_lob) {
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

fn is_lob(c: &Column) -> bool {
    let t = &c.data_type;
    matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml")
        || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1))
        || t.schema.is_some()
}

/// A system type from catalog values; nchar/nvarchar lengths are stored in bytes.
fn system_type(name: &str, max_length: i32, precision: i32, scale: i32) -> DataType {
    let mut t = DataType { name: name.to_string(), ..Default::default() };
    match name {
        "char" | "varchar" | "binary" | "varbinary" => t.length = Some(max_length),
        "nchar" | "nvarchar" => t.length = Some(if max_length < 0 { -1 } else { max_length / 2 }),
        "decimal" | "numeric" => {
            t.precision = Some(precision as u8);
            t.scale = Some(scale as u8);
        }
        "datetime2" | "time" | "datetimeoffset" => t.scale = Some(scale as u8),
        "float" => t.precision = Some(precision as u8),
        _ => {}
    }
    t
}

/// The local object a synonym points at, when it is in this database.
fn parse_synonym_target(base: &str, database: &str) -> Option<ObjectName> {
    let parts = split_multipart(base);
    match parts.as_slice() {
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

    #[test]
    fn multipart_names() {
        assert_eq!(split_multipart("[dbo].[a.b]"), vec!["dbo", "a.b"]);
        assert_eq!(split_multipart("srv.db.[s]].x].t"), vec!["srv", "db", "s].x", "t"]);
        assert_eq!(parse_synonym_target("[Sales].[Orders]", "Shop"), Some(ObjectName::new("Sales", "Orders")));
        assert_eq!(parse_synonym_target("[shop].[dbo].[T]", "Shop"), Some(ObjectName::new("dbo", "T")));
        assert_eq!(parse_synonym_target("[Other].[dbo].[T]", "Shop"), None);
    }

    #[test]
    fn system_types() {
        assert_eq!(system_type("nvarchar", 200, 0, 0).length, Some(100));
        assert_eq!(system_type("nvarchar", -1, 0, 0).length, Some(-1));
        assert_eq!(system_type("decimal", 9, 18, 2).sql(), "decimal(18, 2)");
    }
}
