//! The schema model shared by schema compare, data compare, snapshots and scripting.
//!
//! Every collection is kept sorted by name so that two reads of the same database give the
//! same JSON. Catalog ids are not stored: objects are identified by schema and name.

use serde::{Deserialize, Serialize};

use super::objects::*;

/// Format version of the serialized model (snapshot files).
pub const MODEL_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Database {
    pub model_version: u32,
    pub server: ServerInfo,
    pub name: String,
    pub collation: String,
    pub compatibility_level: u32,
    /// The login that owns the database: the login of its `dbo` user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub schemas: Vec<Schema>,
    pub tables: Vec<Table>,
    pub views: Vec<View>,
    pub routines: Vec<Routine>,
    pub types: Vec<UserType>,
    pub sequences: Vec<Sequence>,
    pub synonyms: Vec<Synonym>,
    pub extended_properties: Vec<ExtendedProperty>,
    /// Users and roles (fixed roles and built-in users excluded).
    pub principals: Vec<Principal>,
    /// GRANT / DENY on objects, schemas and the database (column permissions not included).
    pub permissions: Vec<Permission>,
    /// Object-to-object references: views and modules to what they use, FKs, triggers, synonyms.
    pub dependencies: Vec<Dependency>,
    /// Database-level DDL triggers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ddl_triggers: Vec<DdlTrigger>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assemblies: Vec<Assembly>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub xml_schema_collections: Vec<XmlSchemaCollection>,
    /// CREATE DEFAULT objects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub defaults: Vec<BindableObject>,
    /// CREATE RULE objects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<BindableObject>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub partition_functions: Vec<PartitionFunction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub partition_schemes: Vec<PartitionScheme>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fulltext_catalogs: Vec<FullTextCatalog>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fulltext_stoplists: Vec<FullTextStoplist>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub search_property_lists: Vec<SearchPropertyList>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message_types: Vec<MessageType>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<Contract>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queues: Vec<Queue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<Service>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub service_bindings: Vec<ServiceBinding>,
    /// Event notifications ON DATABASE (queue ones belong to their queue).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub event_notifications: Vec<EventNotification>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub certificates: Vec<Certificate>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub asymmetric_keys: Vec<AsymmetricKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symmetric_keys: Vec<SymmetricKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security_policies: Vec<SecurityPolicy>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external_data_sources: Vec<ExternalDataSource>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external_file_formats: Vec<ExternalFileFormat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external_tables: Vec<ExternalTable>,
    /// Names of the database scoped credentials (not compared; external data sources need them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
}

/// A trigger ON DATABASE.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DdlTrigger {
    pub name: String,
    /// The CREATE TRIGGER text; empty when encrypted or CLR.
    pub definition: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Event types and groups, as sys.trigger_events lists them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    pub settings: ModuleSettings,
    /// CLR triggers: `[assembly].[class].[method]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clr_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_as: Option<String>,
}

impl Database {
    /// True when object names compare case-sensitively in this database.
    pub fn case_sensitive(&self) -> bool {
        let c = self.collation.to_ascii_uppercase();
        c.contains("_CS") || c.ends_with("_BIN") || c.ends_with("_BIN2") || c.contains("_BIN_") || c.contains("_BIN2_")
    }

    pub fn table(&self, schema: &str, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.schema == schema && t.name == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerInfo {
    /// 11 = 2012, 12 = 2014, 13 = 2016, 14 = 2017, 15 = 2019, 16 = 2022, 17 = 2025.
    pub major_version: u32,
    pub product_version: String,
    pub edition: String,
    /// SERVERPROPERTY('EngineEdition'): 5 = Azure SQL Database, 8 = Azure SQL Managed Instance.
    pub engine_edition: u32,
}

impl ServerInfo {
    pub fn is_azure_sql_database(&self) -> bool {
        self.engine_edition == 5
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ObjectName {
    pub schema: String,
    pub name: String,
}

impl ObjectName {
    pub fn new(schema: &str, name: &str) -> Self {
        ObjectName { schema: schema.to_string(), name: name.to_string() }
    }

    /// `[schema].[name]`.
    pub fn quoted(&self) -> String {
        format!("{}.{}", crate::db::quote(&self.schema), crate::db::quote(&self.name))
    }
}

impl std::fmt::Display for ObjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.quoted())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Schema {
    pub name: String,
    pub owner: String,
}

/// A column type as declared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DataType {
    /// Type name: `int`, `nvarchar`, or the name of a user-defined type.
    pub name: String,
    /// Schema of a user-defined type; `None` for system types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Characters for char/nchar/varchar/nvarchar, bytes for binary/varbinary; -1 = max.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<u8>,
    /// Typed xml: `[schema].[collection]`, with DOCUMENT when `xml_document`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xml_collection: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub xml_document: bool,
}

impl DataType {
    /// The type as written in DDL: `nvarchar(100)`, `decimal(18, 2)`, `[dbo].[Phone]`.
    pub fn sql(&self) -> String {
        if let Some(schema) = &self.schema {
            return format!("{}.{}", crate::db::quote(schema), crate::db::quote(&self.name));
        }
        let name = self.name.as_str();
        let mut out = name.to_string();
        match name {
            "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => {
                if let Some(len) = self.length {
                    if len < 0 {
                        out.push_str("(max)");
                    } else {
                        out.push_str(&format!("({len})"));
                    }
                }
            }
            "decimal" | "numeric" => {
                out.push_str(&format!("({}, {})", self.precision.unwrap_or(18), self.scale.unwrap_or(0)));
            }
            "datetime2" | "time" | "datetimeoffset" => {
                if let Some(scale) = self.scale {
                    out.push_str(&format!("({scale})"));
                }
            }
            "float" => {
                if let Some(p) = self.precision.filter(|p| *p != 53) {
                    out.push_str(&format!("({p})"));
                }
            }
            "xml" => {
                if let Some(c) = &self.xml_collection {
                    out.push_str(&format!("({}{c})", if self.xml_document { "DOCUMENT " } else { "" }));
                }
            }
            _ => {}
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Identity {
    pub seed: String,
    pub increment: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_for_replication: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Computed {
    pub definition: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub persisted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DefaultConstraint {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub system_named: bool,
    pub definition: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    /// Position in the table, from 1. Kept for ForceColumnOrder; not a property in itself.
    pub ordinal: u32,
    pub data_type: DataType,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<Computed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultConstraint>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub rowguidcol: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sparse: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub column_set: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub filestream: bool,
    /// Temporal period column: `ROW START` or `ROW END` (also TRANSACTION_ID / SEQUENCE_NUMBER forms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_always: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Dynamic data masking function, e.g. `default()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub masking_function: Option<String>,
    /// Default object bound with sp_bindefault: `[schema].[name]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_default: Option<String>,
    /// Rule bound with sp_bindrule: `[schema].[name]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<Sensitivity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndexColumn {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IndexKind {
    Heap,
    Clustered,
    #[default]
    Nonclustered,
    ClusteredColumnstore,
    NonclusteredColumnstore,
    Xml,
    Spatial,
    NonclusteredHash,
}

/// Where a table or index is stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Storage {
    /// Filegroup name, or partition scheme name when `partition_column` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_column: Option<String>,
    /// NONE / ROW / PAGE / COLUMNSTORE / COLUMNSTORE_ARCHIVE, per partition when they differ.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub compression: Vec<PartitionCompression>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PartitionCompression {
    pub partition: u32,
    pub compression: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Index {
    pub name: String,
    pub kind: IndexKind,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub primary_key: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    /// A UNIQUE constraint rather than a unique index.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unique_constraint: bool,
    /// The PK/UQ constraint name was generated by SQL Server.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub system_named: bool,
    pub columns: Vec<IndexColumn>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub included: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// 0 = server default.
    pub fill_factor: u8,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pad_index: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ignore_dup_key: bool,
    pub allow_row_locks: bool,
    pub allow_page_locks: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_recompute: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub incremental: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub optimize_for_sequential_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket_count: Option<i64>,
    pub storage: Storage,
    /// Set for XML indexes (kind `Xml`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xml: Option<XmlIndex>,
    /// Set for spatial indexes (kind `Spatial`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spatial: Option<SpatialIndex>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum XmlIndexKind {
    #[default]
    Primary,
    Secondary,
    Selective,
    SecondarySelective,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct XmlIndex {
    pub kind: XmlIndexKind,
    /// Secondary indexes: VALUE, PATH or PROPERTY.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_type: Option<String>,
    /// The primary (or selective) XML index a secondary index is built on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub using_index: Option<String>,
    /// Secondary selective indexes: the path of the selective index they cover.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub using_path: Option<String>,
    /// Selective indexes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<SelectivePath>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<XmlNamespace>,
}

/// One promoted path of a selective XML index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SelectivePath {
    pub id: i32,
    pub name: String,
    pub path: String,
    /// `AS SQL <type>` rather than `AS XQUERY`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sql: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xquery_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<i32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub node_hint: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub singleton: bool,
    /// SQL paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<DataType>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct XmlNamespace {
    /// `None` for the DEFAULT namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    pub uri: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SpatialIndex {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub geography: bool,
    /// GEOMETRY_GRID, GEOGRAPHY_GRID, GEOMETRY_AUTO_GRID or GEOGRAPHY_AUTO_GRID.
    pub tessellation: String,
    /// xmin, ymin, xmax, ymax as written in DDL (geometry only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bounding_box: Vec<String>,
    /// LOW / MEDIUM / HIGH for levels 1 to 4.
    pub grids: Vec<String>,
    pub cells_per_object: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ForeignKey {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub system_named: bool,
    pub columns: Vec<String>,
    pub referenced_table: ObjectName,
    pub referenced_columns: Vec<String>,
    /// NO_ACTION / CASCADE / SET_NULL / SET_DEFAULT.
    pub on_delete: String,
    pub on_update: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Created or re-enabled WITH NOCHECK.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_trusted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_for_replication: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CheckConstraint {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub system_named: bool,
    pub definition: String,
    /// Set for a column-level constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_trusted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_for_replication: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Trigger {
    pub name: String,
    /// The CREATE TRIGGER text; empty when encrypted.
    pub definition: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub instead_of: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_for_replication: bool,
    /// INSERT / UPDATE / DELETE, in that order.
    pub events: Vec<String>,
    /// sp_settriggerorder: (event, FIRST|LAST).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<(String, String)>,
    pub settings: ModuleSettings,
    /// CLR triggers: `[assembly].[class].[method]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clr_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_as: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Temporal {
    pub history_table: ObjectName,
    pub period_start: String,
    pub period_end: String,
    /// e.g. `6 MONTHS`; `None` = infinite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Table {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// sys.objects.modify_date as `yyyy-MM-dd HH:mm:ss.fff`; kept for snapshots, not compared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub check_constraints: Vec<CheckConstraint>,
    pub triggers: Vec<Trigger>,
    /// Heap or clustered storage.
    pub storage: Storage,
    /// TEXTIMAGE_ON filegroup; only kept when the table has a LOB column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_filegroup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filestream_filegroup: Option<String>,
    /// TABLE / AUTO / DISABLE.
    pub lock_escalation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<Temporal>,
    /// This table is the history table of a system-versioned table.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_table: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub memory_optimized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability: Option<String>,
    /// Graph tables: NODE or EDGE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_tracking: Option<ChangeTracking>,
    /// CREATE STATISTICS objects (statistics of indexes and auto-created ones are not kept).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub statistics: Vec<Statistic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fulltext_index: Option<FullTextIndex>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edge_constraints: Vec<EdgeConstraint>,
    /// sp_tableoption 'large value types out of row'.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub large_value_types_out_of_row: bool,
    /// Set for a FileTable. Its fixed columns stay in `columns`; its system-defined constraints
    /// and indexes are left out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_table: Option<FileTable>,
    /// Stretch Database is enabled on the table (sys.tables.is_remote_data_archive_enabled).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub remote_data_archive: bool,
}

/// The FileTable options of a table (`AS FILETABLE WITH (…)`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileTable {
    /// FILETABLE_DIRECTORY.
    pub directory: String,
    /// FILETABLE_COLLATE_FILENAME.
    pub filename_collation: String,
    /// FILETABLE_PRIMARY_KEY_CONSTRAINT_NAME: the name of the system-defined primary key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<String>,
}

impl Table {
    pub fn object_name(&self) -> ObjectName {
        ObjectName::new(&self.schema, &self.name)
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn primary_key(&self) -> Option<&Index> {
        self.indexes.iter().find(|i| i.primary_key)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeTracking {
    pub track_columns_updated: bool,
}

/// Session settings a module was created with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleSettings {
    pub ansi_nulls: bool,
    pub quoted_identifier: bool,
}

impl Default for ModuleSettings {
    fn default() -> Self {
        ModuleSettings { ansi_nulls: true, quoted_identifier: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct View {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// sys.objects.modify_date as `yyyy-MM-dd HH:mm:ss.fff`; kept for snapshots, not compared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    pub definition: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub schema_bound: bool,
    pub settings: ModuleSettings,
    /// Output columns in order (for data compare and dependency checks).
    pub columns: Vec<Column>,
    /// Indexed views.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub statistics: Vec<Statistic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fulltext_index: Option<FullTextIndex>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoutineKind {
    #[default]
    Procedure,
    ScalarFunction,
    InlineTableFunction,
    TableFunction,
    ClrProcedure,
    ClrScalarFunction,
    ClrTableFunction,
    ClrAggregate,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Routine {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// sys.objects.modify_date as `yyyy-MM-dd HH:mm:ss.fff`; kept for snapshots, not compared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    pub kind: RoutineKind,
    /// The CREATE text; empty when encrypted or CLR.
    pub definition: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub schema_bound: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub native_compilation: bool,
    pub settings: ModuleSettings,
    /// EXECUTE AS: CALLER (None), OWNER, SELF, or a user name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_as: Option<String>,
    /// CLR: `[assembly].[class].[method]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clr_method: Option<String>,
    /// CLR modules: the parameters (T-SQL modules carry them in their text).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    /// CLR scalar functions and aggregates: the return type as written in DDL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
    /// CLR table-valued functions: the result columns.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub result_columns: Vec<Column>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub null_on_null_input: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Parameter {
    /// With the `@`.
    pub name: String,
    /// The type as written in DDL: `nvarchar(50)`, `[dbo].[Point]`.
    pub data_type: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub output: bool,
    /// The default as a T-SQL literal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserType {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Alias types: the base type; table types: `table`; CLR types: `clr`.
    pub kind: UserTypeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_type: Option<DataType>,
    pub nullable: bool,
    /// Table types.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<Column>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_constraints: Vec<CheckConstraint>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub memory_optimized: bool,
    /// CLR: `[assembly].[class]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clr_class: Option<String>,
    /// Alias types: default object bound with sp_bindefault, `[schema].[name]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_default: Option<String>,
    /// Alias types: rule bound with sp_bindrule, `[schema].[name]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_rule: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UserTypeKind {
    #[default]
    Alias,
    Table,
    Clr,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Sequence {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub data_type: DataType,
    pub start: String,
    pub increment: String,
    pub minimum: String,
    pub maximum: String,
    pub cycle: bool,
    /// False = NO CACHE.
    pub cached: bool,
    /// `None` with `cached` = CACHE with the server's default size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_size: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Synonym {
    pub schema: String,
    pub name: String,
    /// Explicit owner set with ALTER AUTHORIZATION; `None` = owned by the schema's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// As stored: `[server].[db].[schema].[object]`, parts optional.
    pub base_object: String,
}

/// An extended property; `level1`/`level2` follow sp_addextendedproperty's naming.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtendedProperty {
    /// DATABASE, SCHEMA, or the level-1 type: TABLE / VIEW / PROCEDURE / FUNCTION / SEQUENCE / SYNONYM / TYPE.
    pub class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// COLUMN / INDEX / CONSTRAINT / TRIGGER / PARAMETER.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor_name: Option<String>,
    pub name: String,
    pub value: Option<String>,
    /// The sql_variant base type of the value (`nvarchar`, `int`, `varbinary`…). `None` when not
    /// known (older snapshots, scripts); it then matches any type. Binary values are `0x…` hex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
}

impl ExtendedProperty {
    /// SQL Clone's marker on a cloned database (`IsSQLCloneDatabase`): not part of the schema.
    pub fn is_clone_marker(&self, case_sensitive: bool) -> bool {
        const NAME: &str = "IsSQLCloneDatabase";
        self.class == "DATABASE" && if case_sensitive { self.name == NAME } else { self.name.eq_ignore_ascii_case(NAME) }
    }

    /// SQL Source Control's link on a live database (`SQLSourceControl…`): not part of the schema.
    pub fn is_source_control_marker(&self) -> bool {
        self.class == "DATABASE" && self.name.get(..16).is_some_and(|p| p.eq_ignore_ascii_case("SQLSourceControl"))
    }
}

/// A database user or role.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Principal {
    pub name: String,
    /// SQL_USER, WINDOWS_USER, WINDOWS_GROUP, EXTERNAL_USER, EXTERNAL_GROUP, CERTIFICATE_MAPPED_USER,
    /// ASYMMETRIC_KEY_MAPPED_USER, DATABASE_ROLE or APPLICATION_ROLE.
    pub kind: String,
    /// The server login of a user; `None` for users without a login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_schema: Option<String>,
    /// Owner of a role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Roles this principal is a member of (fixed roles included).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub member_of: Vec<String>,
    /// Users mapped to a certificate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    /// Users mapped to an asymmetric key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asymmetric_key: Option<String>,
    /// A user without the CONNECT permission CREATE USER gives (revoked). DENY CONNECT and
    /// GRANT CONNECT WITH GRANT OPTION are kept as permissions.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_connect: bool,
}

impl Principal {
    pub fn is_role(&self) -> bool {
        self.kind.ends_with("_ROLE")
    }
}

/// One GRANT or DENY.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Permission {
    /// OBJECT, SCHEMA, DATABASE, or the class keyword of another securable: TYPE, ASSEMBLY,
    /// XML SCHEMA COLLECTION, MESSAGE TYPE, CONTRACT, SERVICE, ROUTE, REMOTE SERVICE BINDING,
    /// FULLTEXT CATALOG, FULLTEXT STOPLIST, SEARCH PROPERTY LIST, CERTIFICATE, ASYMMETRIC KEY,
    /// SYMMETRIC KEY.
    pub class: String,
    /// The schema of schema-scoped securables (OBJECT, TYPE, XML SCHEMA COLLECTION; SCHEMA itself).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// Column-level permission on a table or view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    pub grantee: String,
    /// SELECT, EXECUTE, ALTER...
    pub permission: String,
    /// GRANT, DENY or GRANT_WITH_GRANT_OPTION.
    pub state: String,
}

impl Permission {
    /// ` ON OBJECT::[s].[o]`, ` ON SCHEMA::[s]`, ` ON CERTIFICATE::[c]`, or nothing for the database.
    pub fn securable(&self) -> String {
        let q = |s: &Option<String>| crate::db::quote(s.as_deref().unwrap_or_default());
        match self.class.as_str() {
            "SCHEMA" => format!(" ON SCHEMA::{}", q(&self.schema)),
            _ if self.object.is_none() => String::new(),
            class if self.schema.is_some() => format!(" ON {class}::{}.{}", q(&self.schema), q(&self.object)),
            class => format!(" ON {class}::{}", q(&self.object)),
        }
    }

    /// The permission with its column list: `SELECT ([c])`.
    pub fn action(&self) -> String {
        match &self.column {
            Some(c) => format!("{} ({})", self.permission, crate::db::quote(c)),
            None => self.permission.clone(),
        }
    }

    /// `GRANT SELECT ON OBJECT::[dbo].[T] TO [app]`.
    pub fn grant_sql(&self) -> String {
        let (verb, with) = match self.state.as_str() {
            "DENY" => ("DENY", ""),
            "GRANT_WITH_GRANT_OPTION" => ("GRANT", " WITH GRANT OPTION"),
            _ => ("GRANT", ""),
        };
        format!("{verb} {}{} TO {}{with}", self.action(), self.securable(), crate::db::quote(&self.grantee))
    }

    /// `REVOKE SELECT ON OBJECT::[dbo].[T] FROM [app]` (CASCADE for grants that were passed on).
    pub fn revoke_sql(&self) -> String {
        let cascade = if self.state == "GRANT_WITH_GRANT_OPTION" { " CASCADE" } else { "" };
        format!("REVOKE {}{} FROM {}{cascade}", self.action(), self.securable(), crate::db::quote(&self.grantee))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Dependency {
    pub from: ObjectName,
    pub to: ObjectName,
    /// The referencing object cannot exist without the referenced one (schema binding, FK,
    /// trigger on its table, typed column).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strong: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_sql() {
        let t = |name: &str, length, precision, scale| DataType {
            name: name.into(),
            length,
            precision,
            scale,
            ..Default::default()
        };
        assert_eq!(t("nvarchar", Some(-1), None, None).sql(), "nvarchar(max)");
        assert_eq!(t("varchar", Some(50), None, None).sql(), "varchar(50)");
        assert_eq!(t("decimal", None, Some(18), Some(2)).sql(), "decimal(18, 2)");
        assert_eq!(t("datetime2", None, None, Some(7)).sql(), "datetime2(7)");
        assert_eq!(t("float", None, Some(53), None).sql(), "float");
        assert_eq!(t("float", None, Some(24), None).sql(), "float(24)");
        let udt = DataType { name: "Phone".into(), schema: Some("dbo".into()), ..Default::default() };
        assert_eq!(udt.sql(), "[dbo].[Phone]");
    }

    #[test]
    fn case_sensitivity_from_collation() {
        let db = |c: &str| Database { collation: c.into(), ..Default::default() };
        assert!(!db("SQL_Latin1_General_CP1_CI_AS").case_sensitive());
        assert!(db("Latin1_General_CS_AS").case_sensitive());
        assert!(db("Latin1_General_BIN2").case_sensitive());
        assert!(db("Latin1_General_100_BIN2_UTF8").case_sensitive());
    }
}
