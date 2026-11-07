//! The snapshot records of the objects of `catalog::objects`: the mirror of `snp/objects.rs`.

use super::*;
use crate::schema::clr;
use crate::sources::events::event_code;
use crate::sources::snp::objects::{ASYMMETRIC_ALGORITHMS, BLOCK_OPERATIONS, DATA_SOURCE_TYPES, SYMMETRIC_ALGORITHMS, text_ticks};

/// Assembly records and their file entries start with this marker and a version.
const VERSIONED: u8 = 0xFF;

/// Ids handed out before the blob stores are written, for the records written after them.
#[derive(Default)]
pub(super) struct Early {
    pub(super) assemblies: Vec<i32>,
    pub(super) xml_collections: Vec<i32>,
    pub(super) ddl_triggers: Vec<i32>,
}

impl W<'_> {
    /// Ids of assemblies, XML schema collections and DDL triggers, and the DDL trigger texts:
    /// the comments and blob stores come before those records.
    pub(super) fn early_ids(&mut self) {
        let db = self.db;
        let mut early = Early::default();
        for _ in &db.assemblies {
            early.assemblies.push(self.id());
        }
        for _ in &db.xml_schema_collections {
            early.xml_collections.push(self.id());
        }
        for t in &db.ddl_triggers {
            let id = self.id();
            if t.clr_method.is_none() {
                // Named as SQL Compare writes it, without the trailing line break.
                let text = module_sql(&t.definition, "CREATE", &quote(&t.name));
                self.text(id, text.trim_end().to_string(), t.encrypted);
            }
            early.ddl_triggers.push(id);
        }
        self.early = early;
    }

    /// Assembly files under (assembly id, file number); XML schema collections under their id.
    pub(super) fn object_blobs(&mut self) {
        let db = self.db;
        let mut files: Vec<(u64, Vec<u8>)> = Vec::new();
        for (a, id) in db.assemblies.iter().zip(&self.early.assemblies) {
            for (i, f) in a.files.iter().enumerate() {
                files.push((file_key(*id, i), assembly_bytes(&f.content)));
            }
        }
        self.blob_store(&files);
        let xml: Vec<(u64, Vec<u8>)> =
            db.xml_schema_collections.iter().zip(&self.early.xml_collections).map(|(x, id)| (*id as i64 as u64, x.definition.as_bytes().to_vec())).collect();
        self.blob_store(&xml);
    }

    fn blob_store(&mut self, blobs: &[(u64, Vec<u8>)]) {
        self.f32(BLOBS);
        self.len(blobs.len());
        for (id, b) in blobs {
            self.u64(*id);
            self.len(b.len());
            self.b.b.extend(b);
        }
        self.i32(0); // none undecryptable
    }

    /// Permissions on a database-wide securable.
    fn class_perms(&mut self, class: &str, name: &str) {
        self.perms_on(class, None, Some(name));
    }

    /// Extended properties of a database-wide object, named at level 0.
    fn eprops0(&mut self, class: &str, name: &str) {
        let db = self.db;
        let props: Vec<&ExtendedProperty> = db.extended_properties.iter().filter(|p| p.class == class && p.schema.is_none() && p.object.as_deref() == Some(name)).collect();
        self.eprops_of(&props);
    }

    /// Event types as their codes, in a case-sensitive list as SQL Compare keeps them.
    fn events(&mut self, events: &[String]) {
        let codes: Vec<i32> = events.iter().filter_map(|e| event_code(e)).collect();
        self.coll_head(true, codes.len());
        for c in codes {
            self.i32(c);
        }
    }

    // --------------------------------------------------------------------------- bindables

    pub(super) fn bindable(&mut self, b: &BindableObject, class: &str) {
        let id = self.id();
        self.f32(VERSION);
        self.s(&b.name);
        self.s(&b.schema);
        self.i32(id);
        self.s(&module_sql(&b.definition, "CREATE", &qualified(&b.schema, &b.name)));
        self.eprops(class, &b.schema, &b.name);
        self.no_ss(1);
    }

    // --------------------------------------------------------------------------- parameters

    /// A parameter of a CLR module or partition function.
    fn param(&mut self, name: &str, ty: &DataType, output: bool, default: Option<&str>) {
        let f = facets(self.db, ty);
        self.f32(VERSION);
        self.i32(f.sql_type);
        self.s(name);
        self.i32(f.size);
        self.i32(f.scale);
        self.i32(f.precision);
        self.s(&f.type_name);
        self.s(&f.type_schema);
        self.bool(default.is_some());
        self.s(default.unwrap_or("NULL"));
        self.bool(output);
        self.bool(ty.xml_document);
        self.str(ty.xml_collection.as_deref());
        self.str(f.udt.as_deref());
    }

    /// Parameters, then the id map.
    pub(super) fn params(&mut self, list: &[Parameter]) {
        let cs = self.cs;
        self.coll_head(cs, list.len());
        for p in list {
            self.param(&p.name, &parse_type(&p.data_type), p.output, p.default.as_deref());
        }
        for (i, p) in list.iter().enumerate() {
            self.len(i + 1);
            self.s(&p.name);
        }
    }

    /// A function's return value, if it has a type of its own.
    pub(super) fn returns(&mut self, returns: Option<&str>) {
        self.bool(returns.is_some());
        if let Some(t) = returns {
            self.param("", &parse_type(t), true, None);
        }
    }

    /// Columns a CLR table-valued function returns.
    pub(super) fn result_columns(&mut self, r: &Routine) {
        self.len(usize::from(!r.result_columns.is_empty()));
        if !r.result_columns.is_empty() {
            let host = Table { schema: r.schema.clone(), name: r.name.clone(), columns: r.result_columns.clone(), ..Default::default() };
            let columns = sorted_columns(&host.columns);
            self.coll(&columns, |w, c| w.column(&host, c));
        }
    }

    // --------------------------------------------------------------------------- tables

    /// Statistics, listed with the indexes; SQL Compare files them with their table's filegroup.
    pub(super) fn statistic(&mut self, s: &Statistic, filegroup: Option<&str>) {
        let cs = self.cs;
        self.f32(VERSION);
        self.u8(1);
        self.bool(cs);
        self.s(&s.name);
        self.len(s.columns.len());
        for (i, c) in s.columns.iter().enumerate() {
            self.s(c);
            self.bool(false);
            self.bool(false);
            self.len(i + 1);
        }
        let mut flags = 0x4 | 0x8;
        if s.no_recompute {
            flags |= 0x2;
        }
        if s.incremental {
            flags |= 0x100;
        }
        self.i16(flags);
        self.str(filegroup);
        self.u8(0); // fill factor
        self.i32(0);
        self.bool(false); // system named
        self.no_ss(3);
        self.bool(false);
        self.f32(7.5);
        self.str(s.filter.as_deref());
    }

    pub(super) fn edge_constraint(&mut self, e: &EdgeConstraint) {
        let id = self.id();
        self.f32(VERSION);
        self.s(&e.name);
        self.bool(e.disabled);
        self.i32(id);
        self.bool(e.not_trusted);
        self.len(e.connections.len());
        for (from, to) in &e.connections {
            self.s(&from.quoted());
            self.s(&to.quoted());
        }
        self.bool(e.system_named);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- CLR and XML

    pub(super) fn assembly(&mut self, i: usize, a: &Assembly) {
        let cs = self.cs;
        let id = self.early.assemblies[i];
        self.u8(VERSIONED);
        self.f32(VERSION);
        self.s(&a.name);
        self.str(a.owner.as_deref());
        self.i32(id);
        self.bool(cs);
        self.eprops0("ASSEMBLY", &a.name);
        // The module version id of the main file.
        let mvid = a.files.first().and_then(|f| clr::surface(&assembly_bytes(&f.content))).map(|s| s.mvid).unwrap_or_default();
        self.b.b.extend(mvid);
        self.s(&a.clr_name);
        self.bool(a.visible);
        self.len(a.files.len());
        for (n, f) in a.files.iter().enumerate() {
            self.u8(VERSIONED);
            self.f32(VERSION);
            self.s(&f.name);
            self.u64(file_key(id, n));
            self.i32(0);
            self.no_ss(1);
        }
        self.i32(match a.permission_set.as_str() {
            "EXTERNAL_ACCESS" => 2,
            "UNSAFE" => 3,
            _ => 1,
        });
        self.class_perms("ASSEMBLY", &a.name);
        self.no_ss(3);
    }

    pub(super) fn xml_collection(&mut self, i: usize, x: &XmlSchemaCollection) {
        let cs = self.cs;
        let id = self.early.xml_collections[i];
        self.f32(VERSION);
        self.s(&x.name);
        self.s(&x.schema);
        self.i32(id);
        self.bool(cs);
        self.eprops("XML SCHEMA COLLECTION", &x.schema, &x.name);
        self.perms_on("XML SCHEMA COLLECTION", Some(&x.schema), Some(&x.name));
        self.str(x.owner.as_deref());
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- Service Broker

    pub(super) fn message_type(&mut self, m: &MessageType) {
        let db = self.db;
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&m.name);
        self.str(m.owner.as_deref());
        self.i32(id);
        self.bool(cs);
        self.i32(match m.validation.as_str() {
            "WELL_FORMED_XML" | "VALID_XML" => 1,
            "EMPTY" => 2,
            _ => 0,
        });
        self.str(m.xml_collection.as_deref());
        self.eprops0("MESSAGE TYPE", &m.name);
        self.class_perms("MESSAGE TYPE", &m.name);
        let contracts: Vec<String> = db.contracts.iter().filter(|k| k.messages.iter().any(|x| x.message_type == m.name)).map(|k| quote(&k.name)).collect();
        self.strs(&contracts);
        self.no_ss(2);
    }

    pub(super) fn contract(&mut self, k: &Contract) {
        let db = self.db;
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&k.name);
        self.str(k.owner.as_deref());
        self.i32(id);
        self.eprops0("CONTRACT", &k.name);
        self.bool(false); // system object
        self.bool(cs);
        self.usages(k);
        self.class_perms("CONTRACT", &k.name);
        let services: Vec<String> = db.services.iter().filter(|s| s.contracts.contains(&k.name)).map(|s| quote(&s.name)).collect();
        self.strs(&services);
        self.no_ss(2);
    }

    fn usages(&mut self, k: &Contract) {
        let cs = self.cs;
        self.coll_head(cs, k.messages.len());
        for m in &k.messages {
            self.s(&quote(&m.message_type));
            self.bool(m.sent_by != "TARGET");
            self.bool(m.sent_by != "INITIATOR");
        }
    }

    /// A contract as a service lists it: the user's, or a system one by name.
    fn service_contract(&mut self, name: &str) {
        let db = self.db;
        let cs = self.cs;
        self.f32(VERSION);
        self.s(name);
        self.str(Some("dbo"));
        self.i32(0);
        self.no_eprops();
        match db.contracts.iter().find(|k| k.name == name) {
            Some(k) => {
                self.bool(false);
                self.bool(cs);
                self.usages(k);
            }
            None => {
                self.bool(true);
                self.bool(cs);
                self.empty_coll();
            }
        }
        self.no_perms();
        self.i32(0);
        self.no_ss(2);
    }

    pub(super) fn queue(&mut self, q: &Queue) {
        let db = self.db;
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&q.name);
        self.s(&q.schema);
        self.i32(id);
        self.bool(false); // system object
        self.bool(cs);
        let a = q.activation.as_ref();
        self.i16(a.map_or(0, |a| i16::try_from(a.max_readers).unwrap_or(0)));
        self.str(a.map(|a| a.procedure.as_str()));
        self.execute_as(a.and_then(|a| a.execute_as.as_deref()));
        self.bool(a.is_some_and(|a| a.enabled));
        self.bool(q.enabled); // receive
        self.bool(q.enabled); // enqueue
        self.bool(q.retention);
        self.str(q.filegroup.as_deref());
        self.eprops("QUEUE", &q.schema, &q.name);
        self.coll(&q.event_notifications, W::event_notification);
        let me = ObjectName::new(&q.schema, &q.name);
        let services: Vec<String> = db.services.iter().filter(|s| s.queue == me).map(|s| quote(&s.name)).collect();
        self.strs(&services);
        self.str(q.owner.as_deref());
        self.perms_on("OBJECT", Some(&q.schema), Some(&q.name));
        self.bool(q.poison_message_handling);
        self.no_ss(2);
    }

    pub(super) fn event_notification(&mut self, e: &EventNotification) {
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&e.name);
        self.str(None);
        self.i32(id);
        self.bool(cs);
        self.s(&e.service);
        self.s(&e.broker_instance);
        self.bool(e.fan_in);
        self.events(&e.events);
        self.no_eprops();
        self.no_ss(1);
    }

    pub(super) fn service(&mut self, s: &Service) {
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&s.name);
        self.str(s.owner.as_deref());
        self.i32(id);
        self.bool(false); // system object
        self.bool(cs);
        self.s(&s.queue.quoted());
        self.coll_head(cs, s.contracts.len());
        for k in &s.contracts {
            self.service_contract(k);
        }
        self.eprops0("SERVICE", &s.name);
        self.class_perms("SERVICE", &s.name);
        self.empty_coll(); // event notifications
        self.no_ss(2);
    }

    pub(super) fn route(&mut self, r: &Route) {
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&r.name);
        self.str(r.owner.as_deref());
        self.i32(id);
        self.bool(cs);
        self.bool(false); // system object
        self.str(r.remote_service.as_deref());
        self.str(r.broker_instance.as_deref());
        self.s(&r.address);
        self.str(r.mirror_address.as_deref());
        self.i64(r.expires.as_deref().map_or(0, text_ticks));
        self.bool(r.expires.is_some());
        self.eprops0("ROUTE", &r.name);
        self.class_perms("ROUTE", &r.name);
        self.no_ss(2);
    }

    pub(super) fn service_binding(&mut self, b: &ServiceBinding) {
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&b.name);
        self.str(b.owner.as_deref());
        self.i32(id);
        self.bool(cs);
        self.s(&b.remote_service);
        self.s(&b.user);
        self.bool(b.anonymous);
        self.eprops0("REMOTE SERVICE BINDING", &b.name);
        self.class_perms("REMOTE SERVICE BINDING", &b.name);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- partitioning

    pub(super) fn partition_function(&mut self, f: &PartitionFunction) {
        let cs = self.cs;
        let id = self.id();
        self.f32(VERSION);
        self.s(&f.name);
        self.str(None);
        self.i32(id);
        self.bool(cs);
        self.bool(f.range_right);
        // One parameter, named "0".
        self.coll_head(cs, 1);
        self.param("0", &f.data_type, false, None);
        self.i32(0);
        self.s("0");
        self.eprops0("PARTITION FUNCTION", &f.name);
        self.len(f.boundaries.len());
        for b in &f.boundaries {
            let value = boundary_value(b);
            self.bool(value.is_none());
            if let Some(v) = value {
                self.s(&v);
            }
        }
        self.no_ss(1);
    }

    pub(super) fn partition_scheme(&mut self, s: &PartitionScheme) {
        let id = self.id();
        self.f32(VERSION);
        self.s(&s.name);
        self.str(None);
        self.i32(id);
        self.s(&quote(&s.function));
        self.eprops0("PARTITION SCHEME", &s.name);
        self.f32(VERSION);
        self.len(s.filegroups.len());
        for fg in &s.filegroups {
            self.f32(VERSION);
            self.s(fg);
        }
        self.no_ss(1);
    }

    // --------------------------------------------------------------------------- DDL triggers

    pub(super) fn ddl_trigger(&mut self, i: usize, t: &DdlTrigger) {
        let cs = self.cs;
        let id = self.early.ddl_triggers[i];
        let clr = t.clr_method.is_some();
        self.f32(VERSION);
        self.s(&t.name);
        self.str(None);
        self.i32(id);
        self.bool(cs);
        self.bool(t.disabled);
        self.bool(clr);
        self.bool(t.settings.quoted_identifier);
        self.bool(t.settings.ansi_nulls);
        self.i32(if clr { -1 } else { id });
        self.events(&t.events);
        self.clr_binding(t.clr_method.as_deref());
        self.execute_as(t.execute_as.as_deref().filter(|_| clr));
        self.eprops0("TRIGGER", &t.name);
        self.bool(t.encrypted);
        self.no_ss(1);
    }

    // --------------------------------------------------------------------------- keys

    pub(super) fn certificate(&mut self, c: &Certificate) {
        self.f32(VERSION);
        self.s(&c.name);
        self.str(c.owner.as_deref());
        self.i32(private_key_code(&c.private_key));
        self.bool(c.active_for_dialog);
        self.s(&c.issuer);
        self.s(&c.subject);
        self.i64(text_ticks(&c.start_date));
        self.i64(text_ticks(&c.expiry_date));
        self.str(Some(&c.serial_number).filter(|s| !s.is_empty()).map(String::as_str));
        let thumbprint = hex_bytes(&c.thumbprint);
        self.len(thumbprint.len());
        self.b.b.extend(thumbprint);
        self.class_perms("CERTIFICATE", &c.name);
        self.no_ss(2);
    }

    pub(super) fn symmetric_key(&mut self, k: &SymmetricKey) {
        self.f32(VERSION);
        self.s(&k.name);
        self.str(k.owner.as_deref());
        self.i64(text_ticks(&k.modify_date));
        self.i64(text_ticks(&k.create_date));
        self.b.b.extend(guid_bytes(&k.key_guid));
        self.i32(k.key_length);
        self.len(SYMMETRIC_ALGORITHMS.iter().position(|a| *a == k.algorithm).map_or(0, |i| i + 1));
        self.class_perms("SYMMETRIC KEY", &k.name);
        self.no_ss(2);
    }

    pub(super) fn asymmetric_key(&mut self, k: &AsymmetricKey) {
        self.f32(VERSION);
        self.s(&k.name);
        self.str(k.owner.as_deref());
        let thumbprint = hex_bytes(&k.thumbprint);
        self.len(thumbprint.len());
        self.b.b.extend(thumbprint);
        self.i32(k.key_length);
        self.len(ASYMMETRIC_ALGORITHMS.iter().position(|a| *a == k.algorithm).unwrap_or(0));
        self.i32(private_key_code(&k.private_key));
        self.class_perms("ASYMMETRIC KEY", &k.name);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- full-text

    pub(super) fn fulltext_catalog(&mut self, f: &FullTextCatalog) {
        let db = self.db;
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&f.name);
        self.bool(cs);
        self.str(f.owner.as_deref());
        self.bool(f.is_default);
        self.bool(f.accent_sensitive);
        let tables: Vec<String> = db.tables.iter().filter(|t| t.fulltext_index.as_ref().is_some_and(|x| x.catalog == f.name)).map(|t| t.object_name().quoted()).collect();
        self.strs(&tables);
        self.class_perms("FULLTEXT CATALOG", &f.name);
        self.no_ss(2);
    }

    pub(super) fn stoplist(&mut self, l: &FullTextStoplist) {
        let cs = self.cs;
        self.f32(VERSION);
        self.s(&l.name);
        self.str(l.owner.as_deref());
        self.bool(cs);
        self.f32(VERSION);
        self.bool(false); // not based on the system list
        let mut languages: Vec<i32> = l.words.iter().map(|w| w.language).collect();
        languages.dedup();
        languages.sort_unstable();
        languages.dedup();
        self.len(languages.len());
        for lang in languages {
            self.i32(lang);
            let words: Vec<&str> = l.words.iter().filter(|w| w.language == lang).map(|w| w.word.as_str()).collect();
            self.strs(&words);
        }
        self.no_ss(2);
    }

    pub(super) fn property_list(&mut self, l: &SearchPropertyList) {
        self.class_perms("SEARCH PROPERTY LIST", &l.name);
        self.f32(VERSION);
        self.s(&l.name);
        self.str(l.owner.as_deref());
        self.len(l.properties.len());
        for (i, p) in l.properties.iter().enumerate() {
            self.f32(VERSION);
            self.s(&p.name);
            self.s(&p.set_guid);
            self.i32(p.int_id);
            self.str(p.description.as_deref());
            self.len(i + 1);
        }
        self.i32(0);
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- security policies

    pub(super) fn security_policy(&mut self, p: &SecurityPolicy) {
        let id = self.id();
        self.f32(VERSION);
        self.i32(id);
        self.s(&p.schema);
        self.s(&p.name);
        self.str(p.owner.as_deref());
        self.bool(p.enabled);
        self.bool(p.schema_bound);
        self.bool(p.not_for_replication);
        self.len(p.predicates.len());
        for x in &p.predicates {
            self.s(&x.function);
            self.s(&x.arguments);
            self.s(&x.table.quoted());
            self.i32(i32::from(x.kind == "BLOCK"));
            self.len(x.operation.as_deref().and_then(|o| BLOCK_OPERATIONS.iter().position(|b| *b == o)).unwrap_or(0));
        }
        self.eprops("SECURITY POLICY", &p.schema, &p.name);
        self.perms_on("OBJECT", Some(&p.schema), Some(&p.name));
        self.no_ss(2);
    }

    // --------------------------------------------------------------------------- PolyBase

    pub(super) fn file_format(&mut self, f: &ExternalFileFormat) {
        let id = self.id();
        self.i32(id);
        self.str(None);
        self.s(&f.name);
        self.s(&f.format_type);
        self.str(f.field_terminator.as_deref());
        self.str(f.string_delimiter.as_deref());
        self.str(f.date_format.as_deref());
        self.bool(f.use_type_default);
        self.str(f.serde_method.as_deref());
        self.str(None);
        self.str(f.encoding.as_deref());
        self.str(f.data_compression.as_deref());
        self.u8(u8::from(f.first_row.is_some()));
        if let Some(n) = f.first_row {
            self.i32(n);
        }
        self.no_ss(1);
    }

    pub(super) fn data_source(&mut self, d: &ExternalDataSource) {
        self.s(&d.name);
        self.s(&d.location);
        self.len(d.kind.as_deref().and_then(|k| DATA_SOURCE_TYPES.iter().position(|t| *t == k)).unwrap_or(4));
        self.str(d.resource_manager_location.as_deref());
        self.str(d.credential.as_deref());
        self.str(d.database_name.as_deref());
        self.str(d.shard_map_name.as_deref());
        self.str(d.connection_options.as_deref());
        self.bool(d.pushdown.unwrap_or(true));
        self.no_ss(1);
    }

    pub(super) fn external_table(&mut self, t: &ExternalTable) {
        let cs = self.cs;
        let id = self.id();
        self.i32(id);
        self.s(&t.schema);
        self.s(&t.name);
        self.bool(cs);
        let host = Table { schema: t.schema.clone(), name: t.name.clone(), columns: t.columns.clone(), ..Default::default() };
        let columns = sorted_columns(&host.columns);
        self.coll(&columns, |w, c| w.column(&host, c));
        self.s(&t.data_source);
        self.str(t.file_format.as_deref());
        self.str(t.location.as_deref());
        self.i32(i32::from(t.reject_type.as_deref() == Some("PERCENTAGE")));
        let value = t.reject_value.as_deref().and_then(|v| v.parse::<f64>().ok());
        self.u8(u8::from(value.is_some()));
        if let Some(v) = value {
            self.f64(v);
        }
        let sample = t.reject_sample_value.as_deref().and_then(|v| v.parse::<i32>().ok());
        self.u8(u8::from(sample.is_some()));
        if let Some(v) = sample {
            self.i32(v);
        }
        self.str(t.remote_schema.as_deref());
        self.str(t.remote_object.as_deref());
        let (distribution, sharding) = match t.distribution.as_deref() {
            Some(d) if d.starts_with("SHARDED") => (Some(0), split_multipart(d.trim_start_matches("SHARDED").trim_matches(['(', ')', ' '])).into_iter().next()),
            Some("REPLICATED") => (Some(1), None),
            Some("ROUND_ROBIN") => (Some(2), None),
            _ => (None, None),
        };
        self.u8(u8::from(distribution.is_some()));
        if let Some(d) = distribution {
            self.i32(d);
        }
        self.str(sharding.as_deref());
        self.str(t.owner.as_deref());
        self.no_ss(1);
        self.perms_on("OBJECT", Some(&t.schema), Some(&t.name));
        self.eprops("TABLE", &t.schema, &t.name);
    }
}

/// Blob key of an assembly file: the assembly id above the file number.
fn file_key(assembly: i32, file: usize) -> u64 {
    (u64::from(assembly as u32) << 32) | (file as u64 + 1)
}

fn assembly_bytes(content: &str) -> Vec<u8> {
    hex_bytes(content)
}

/// Bytes of `0x…` or bare hex.
fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    clr::from_hex(s).unwrap_or_default()
}

fn private_key_code(p: &str) -> i32 {
    match p {
        "MK" => 1,
        "PW" => 2,
        "SK" => 3,
        _ => 0,
    }
}

/// .NET `Guid` bytes of `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`: the first three groups little-endian.
pub(super) fn guid_bytes(s: &str) -> [u8; 16] {
    let hex: String = s.chars().filter(char::is_ascii_hexdigit).collect();
    let mut b: [u8; 16] = clr::from_hex(&hex).and_then(|v| v.try_into().ok()).unwrap_or_default();
    b[..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    b
}

/// A boundary as the snapshot stores it: no quotes or braces; None for NULL.
fn boundary_value(b: &str) -> Option<String> {
    let b = b.trim();
    if b.eq_ignore_ascii_case("NULL") {
        return None;
    }
    let inner = b.strip_prefix('N').unwrap_or(b);
    if inner.len() >= 2 && inner.starts_with('\'') && inner.ends_with('\'') {
        return Some(inner[1..inner.len() - 1].replace("''", "'").trim_matches(['{', '}']).to_string());
    }
    Some(b.to_string())
}

/// A type as DDL writes it: `nvarchar(50)`, `decimal(18, 2)`, `xml(DOCUMENT [dbo].[S])`, `[dbo].[T]`.
fn parse_type(s: &str) -> DataType {
    let s = s.trim();
    let (head, args) = match s.find('(') {
        Some(i) if s.ends_with(')') => (&s[..i], Some(&s[i + 1..s.len() - 1])),
        _ => (s, None),
    };
    let parts = split_multipart(head.trim());
    if parts.len() >= 2 {
        return DataType { schema: Some(parts[parts.len() - 2].clone()), name: parts[parts.len() - 1].clone(), ..Default::default() };
    }
    let name = parts.first().map(|p| p.to_ascii_lowercase()).unwrap_or_default();
    let mut t = DataType { name: name.clone(), ..Default::default() };
    let Some(args) = args else { return t };
    let nums: Vec<&str> = args.split(',').map(str::trim).collect();
    let num = |i: usize| nums.get(i).and_then(|n| n.parse::<u8>().ok());
    match name.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => {
            t.length = Some(if args.trim().eq_ignore_ascii_case("max") { -1 } else { args.trim().parse().unwrap_or(1) });
        }
        "decimal" | "numeric" => {
            t.precision = num(0);
            t.scale = num(1);
        }
        "datetime2" | "time" | "datetimeoffset" => t.scale = num(0),
        "float" => t.precision = num(0),
        "xml" => {
            let a = args.trim();
            match a.get(..9).filter(|p| p.eq_ignore_ascii_case("DOCUMENT ")) {
                Some(_) => {
                    t.xml_document = true;
                    t.xml_collection = Some(a[9..].trim().to_string());
                }
                None => t.xml_collection = Some(a.trim_start_matches("CONTENT ").trim().to_string()),
            }
        }
        _ => {}
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_values() {
        assert_eq!(parse_type("nvarchar(max)").length, Some(-1));
        assert_eq!(parse_type("decimal(18, 2)").sql(), "decimal(18, 2)");
        assert_eq!(parse_type("[dbo].[Amount]").schema.as_deref(), Some("dbo"));
        let x = parse_type("xml(DOCUMENT [dbo].[S])");
        assert!(x.xml_document);
        assert_eq!(x.sql(), "xml(DOCUMENT [dbo].[S])");
        assert_eq!(boundary_value("N'it''s'").as_deref(), Some("it's"));
        assert_eq!(boundary_value("NULL"), None);
        assert_eq!(boundary_value("2020").as_deref(), Some("2020"));
        let g = "D8D63B6C-0000-4000-8000-000000000001";
        assert_eq!(crate::sources::snp::guid(guid_bytes(g)), g);
        assert_eq!(file_key(65_536, 0), 0x0001_0000_0000_0001);
    }
}
