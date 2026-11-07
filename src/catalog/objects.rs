//! Model of the database objects beyond tables and modules: CLR assemblies, XML schema
//! collections, bound defaults and rules, partitioning, full-text search, Service Broker, keys
//! and certificates, row-level security and PolyBase objects.

use serde::{Deserialize, Serialize};

use super::model::{Column, DataType, ObjectName};

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_true(b: &bool) -> bool {
    *b
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct XmlSchemaCollection {
    pub schema: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// XML_SCHEMA_NAMESPACE as the server returns it.
    pub definition: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssemblyFile {
    pub name: String,
    /// The file's bytes in upper-case hex, without `0x`.
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Assembly {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The CLR name: `name, version=…, culture=…, publickeytoken=…`.
    pub clr_name: String,
    #[serde(skip_serializing_if = "is_true", default = "yes")]
    pub visible: bool,
    /// SAFE, EXTERNAL_ACCESS or UNSAFE.
    pub permission_set: String,
    /// The main image first.
    pub files: Vec<AssemblyFile>,
    /// User assemblies this one references.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

impl Default for Assembly {
    fn default() -> Self {
        Assembly { name: String::new(), owner: None, clr_name: String::new(), visible: true, permission_set: "SAFE".into(), files: Vec::new(), references: Vec::new() }
    }
}

/// A default or rule object made with CREATE DEFAULT / CREATE RULE (bound with sp_bind*).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BindableObject {
    pub schema: String,
    pub name: String,
    pub definition: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PartitionFunction {
    pub name: String,
    pub data_type: DataType,
    #[serde(skip_serializing_if = "is_false")]
    pub range_right: bool,
    /// Boundary values as T-SQL literals, in order.
    pub boundaries: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PartitionScheme {
    pub name: String,
    pub function: String,
    /// One filegroup per partition, then the NEXT USED filegroup when there is one.
    pub filegroups: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FullTextCatalog {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub is_default: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub accent_sensitive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Stopword {
    pub language: i32,
    pub word: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FullTextStoplist {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Sorted by language and word.
    pub words: Vec<Stopword>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchProperty {
    pub name: String,
    pub set_guid: String,
    pub int_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchPropertyList {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub properties: Vec<SearchProperty>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FullTextColumn {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_column: Option<String>,
    pub language: i32,
}

/// The full-text index of a table or indexed view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FullTextIndex {
    pub catalog: String,
    pub key_index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filegroup: Option<String>,
    /// AUTO, MANUAL or OFF.
    pub change_tracking: String,
    /// SYSTEM, OFF or the name of a stoplist.
    pub stoplist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_list: Option<String>,
    pub columns: Vec<FullTextColumn>,
}

/// A statistics object made with CREATE STATISTICS.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Statistic {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub no_recompute: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub incremental: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EdgeConstraint {
    pub name: String,
    #[serde(skip_serializing_if = "is_false")]
    pub system_named: bool,
    /// (from node table, to node table).
    pub connections: Vec<(ObjectName, ObjectName)>,
    #[serde(skip_serializing_if = "is_false")]
    pub on_delete_cascade: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub not_trusted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Sensitivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub information_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub information_type_id: Option<String>,
    /// LOW, MEDIUM, HIGH or CRITICAL (SQL Server 2019 and later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
}

// ---- Service Broker ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MessageType {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// NONE, EMPTY, WELL_FORMED_XML or VALID_XML.
    pub validation: String,
    /// `[schema].[collection]` for VALID_XML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xml_collection: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContractMessage {
    pub message_type: String,
    /// ANY, INITIATOR or TARGET.
    pub sent_by: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Contract {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub messages: Vec<ContractMessage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QueueActivation {
    pub enabled: bool,
    /// `[schema].[procedure]`.
    pub procedure: String,
    pub max_readers: i32,
    /// OWNER, SELF, a user name, or None for CALLER.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_as: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Queue {
    pub schema: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// STATUS: the queue takes messages.
    pub enabled: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub retention: bool,
    pub poison_message_handling: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation: Option<QueueActivation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filegroup: Option<String>,
    /// Event notifications ON QUEUE.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub event_notifications: Vec<EventNotification>,
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            schema: String::new(),
            name: String::new(),
            owner: None,
            enabled: true,
            retention: false,
            poison_message_handling: true,
            activation: None,
            filegroup: None,
            event_notifications: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub queue: ObjectName,
    pub contracts: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Route {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broker_instance: Option<String>,
    /// Expiry time (UTC, `yyyy-MM-dd HH:mm:ss.fff`) when the route has a LIFETIME.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_address: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceBinding {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub remote_service: String,
    pub user: String,
    #[serde(skip_serializing_if = "is_false")]
    pub anonymous: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EventNotification {
    pub name: String,
    /// The queue for ON QUEUE notifications; None for ON DATABASE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<ObjectName>,
    #[serde(skip_serializing_if = "is_false")]
    pub fan_in: bool,
    /// Event or event group names, sorted.
    pub events: Vec<String>,
    pub service: String,
    pub broker_instance: String,
}

// ---- Keys and certificates ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Certificate {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub subject: String,
    pub issuer: String,
    /// `0x…`.
    pub thumbprint: String,
    pub start_date: String,
    pub expiry_date: String,
    /// NA, MK, PW or SK (sys.certificates.pvt_key_encryption_type).
    pub private_key: String,
    #[serde(skip_serializing_if = "is_false")]
    pub active_for_dialog: bool,
    /// sys.certificates.cert_serial_number (space-separated hex bytes); not compared.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub serial_number: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AsymmetricKey {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub thumbprint: String,
    /// RSA_512, RSA_1024, RSA_2048...
    pub algorithm: String,
    pub key_length: i32,
    pub private_key: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SymmetricKey {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// AES_256, TRIPLE_DES...
    pub algorithm: String,
    pub key_length: i32,
    pub key_guid: String,
    pub create_date: String,
    pub modify_date: String,
}

// ---- Row-level security ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SecurityPredicate {
    /// FILTER or BLOCK.
    pub kind: String,
    /// `[schema].[function]`.
    pub function: String,
    /// The argument list with its parentheses: `([TenantId])`.
    pub arguments: String,
    pub table: ObjectName,
    /// AFTER INSERT, AFTER UPDATE, BEFORE UPDATE or BEFORE DELETE for block predicates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SecurityPolicy {
    pub schema: String,
    pub name: String,
    pub enabled: bool,
    pub schema_bound: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub not_for_replication: bool,
    pub predicates: Vec<SecurityPredicate>,
}

// ---- PolyBase ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalDataSource {
    pub name: String,
    pub location: String,
    /// HADOOP, RDBMS, SHARD_MAP_MANAGER, BLOB_STORAGE...; None for the generic kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_manager_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shard_map_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_options: Option<String>,
    /// PUSHDOWN (SQL Server 2019 and later); None when the server does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pushdown: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalFileFormat {
    pub name: String,
    /// DELIMITEDTEXT, RCFILE, ORC, PARQUET, JSON, DELTA.
    pub format_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_terminator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_delimiter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_row: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_format: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub use_type_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serde_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_compression: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalTable {
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
    pub data_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// VALUE or PERCENTAGE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_sample_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_object: Option<String>,
    /// SHARDED([col]), REPLICATED or ROUND_ROBIN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
}

/// Dependency-graph keys of objects that have no schema of their own: the schema part of the
/// key names the kind, so a contract and a message type of the same name stay apart.
pub const KEY_ASSEMBLY: &str = "<assembly>";
pub const KEY_MESSAGE_TYPE: &str = "<message type>";
pub const KEY_CONTRACT: &str = "<contract>";
pub const KEY_SERVICE: &str = "<service>";
pub const KEY_EVENT_NOTIFICATION: &str = "<event notification>";
pub const KEY_EXTERNAL_DATA_SOURCE: &str = "<external data source>";
