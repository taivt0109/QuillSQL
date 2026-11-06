//! Comparison of the objects beyond tables and modules (see `catalog::objects`), and of the
//! table parts they bring: statistics, full-text indexes, edge constraints and sensitivity
//! classification.

use super::compare::{Comparer, Diffs, ObjectType, PropDiff};
use super::options::Opt;
use super::text::{self, TextKind};
use crate::catalog::*;

static NO_SCHEMA: String = String::new();

fn owner(o: &Option<String>) -> &str {
    o.as_deref().unwrap_or("dbo")
}

/// The XML of a schema collection without the whitespace between elements.
pub(super) fn xml_key(x: &str) -> String {
    let mut out = String::with_capacity(x.len());
    let mut pending = String::new();
    let mut after_tag = false;
    for c in x.chars() {
        if c.is_whitespace() && after_tag {
            pending.push(c);
            continue;
        }
        if c != '<' {
            out.push_str(&pending);
        }
        pending.clear();
        out.push(c);
        after_tag = c == '>';
    }
    out
}

impl Comparer<'_> {
    /// Owner differences, unless owners are ignored.
    fn owner(&self, d: &mut Diffs, a: &Option<String>, b: &Option<String>) {
        if !self.opts.has(Opt::IgnoreOwners) && !self.same_name(owner(a), owner(b)) {
            d.add("Owner", Some(owner(a).into()), Some(owner(b).into()));
        }
    }

    /// Permissions on a securable that has no schema (`class` CERTIFICATE, SERVICE…).
    fn grants(&self, d: &mut Diffs, class: &str, a: &str, b: &str) {
        self.permissions(d, class, "", Some(a), "", Some(b));
        self.xprops(d, class, None, a, b);
    }

    fn names(&self, a: &[String], b: &[String]) -> bool {
        let key = |v: &[String]| {
            let mut v: Vec<String> = v.iter().map(|s| self.key(s)).collect();
            v.sort();
            v
        };
        key(a) == key(b)
    }

    pub(super) fn more_objects(&mut self) {
        let (db1, db2, o) = (self.db1, self.db2, self.opts);
        self.objects(ObjectType::Assembly, &db1.assemblies, &db2.assemblies, |a| (&NO_SCHEMA, &a.name), Comparer::assembly);
        self.objects(ObjectType::XmlSchemaCollection, &db1.xml_schema_collections, &db2.xml_schema_collections, |x| (&x.schema, &x.name), Comparer::xml_schema_collection);
        self.objects(ObjectType::Default, &db1.defaults, &db2.defaults, |x| (&x.schema, &x.name), |c, a, b| c.bindable(a, b, "DEFAULT"));
        self.objects(ObjectType::Rule, &db1.rules, &db2.rules, |x| (&x.schema, &x.name), |c, a, b| c.bindable(a, b, "RULE"));
        if !o.has(Opt::IgnoreTriggers) {
            self.objects(ObjectType::DdlTrigger, &db1.ddl_triggers, &db2.ddl_triggers, |t| (&NO_SCHEMA, &t.name), Comparer::ddl_trigger);
        }
        if !o.has(Opt::IgnoreFileGroups) {
            self.objects(ObjectType::PartitionFunction, &db1.partition_functions, &db2.partition_functions, |f| (&NO_SCHEMA, &f.name), Comparer::partition_function);
            self.objects(ObjectType::PartitionScheme, &db1.partition_schemes, &db2.partition_schemes, |s| (&NO_SCHEMA, &s.name), Comparer::partition_scheme);
        }
        if !o.has(Opt::IgnoreFullTextIndexing) {
            self.objects(ObjectType::FullTextCatalog, &db1.fulltext_catalogs, &db2.fulltext_catalogs, |c| (&NO_SCHEMA, &c.name), Comparer::fulltext_catalog);
            self.objects(ObjectType::FullTextStoplist, &db1.fulltext_stoplists, &db2.fulltext_stoplists, |c| (&NO_SCHEMA, &c.name), Comparer::stoplist);
        }
        self.objects(ObjectType::SearchPropertyList, &db1.search_property_lists, &db2.search_property_lists, |c| (&NO_SCHEMA, &c.name), Comparer::property_list);
        self.objects(ObjectType::MessageType, &db1.message_types, &db2.message_types, |m| (&NO_SCHEMA, &m.name), Comparer::message_type);
        self.objects(ObjectType::Contract, &db1.contracts, &db2.contracts, |c| (&NO_SCHEMA, &c.name), Comparer::contract);
        self.objects(ObjectType::Queue, &db1.queues, &db2.queues, |q| (&q.schema, &q.name), Comparer::queue);
        self.objects(ObjectType::Service, &db1.services, &db2.services, |s| (&NO_SCHEMA, &s.name), Comparer::service);
        self.objects(ObjectType::Route, &db1.routes, &db2.routes, |r| (&NO_SCHEMA, &r.name), Comparer::route);
        self.objects(ObjectType::ServiceBinding, &db1.service_bindings, &db2.service_bindings, |b| (&NO_SCHEMA, &b.name), Comparer::service_binding);
        self.objects(ObjectType::EventNotification, &db1.event_notifications, &db2.event_notifications, |e| (&NO_SCHEMA, &e.name), |c, a, b| {
            let mut d = Diffs::default();
            c.event_notification(&mut d, a, b);
            d.out
        });
        if !o.has(Opt::IgnoreCertificatesAndCryptoKeys) {
            self.objects(ObjectType::Certificate, &db1.certificates, &db2.certificates, |c| (&NO_SCHEMA, &c.name), Comparer::certificate);
            self.objects(ObjectType::AsymmetricKey, &db1.asymmetric_keys, &db2.asymmetric_keys, |k| (&NO_SCHEMA, &k.name), Comparer::asymmetric_key);
            self.objects(ObjectType::SymmetricKey, &db1.symmetric_keys, &db2.symmetric_keys, |k| (&NO_SCHEMA, &k.name), Comparer::symmetric_key);
        }
        self.objects(ObjectType::SecurityPolicy, &db1.security_policies, &db2.security_policies, |p| (&p.schema, &p.name), Comparer::security_policy);
        self.objects(ObjectType::ExternalDataSource, &db1.external_data_sources, &db2.external_data_sources, |x| (&NO_SCHEMA, &x.name), Comparer::external_data_source);
        self.objects(ObjectType::ExternalFileFormat, &db1.external_file_formats, &db2.external_file_formats, |x| (&NO_SCHEMA, &x.name), Comparer::external_file_format);
        self.objects(ObjectType::ExternalTable, &db1.external_tables, &db2.external_tables, |x| (&x.schema, &x.name), Comparer::external_table);
    }

    fn assembly(&self, a: &Assembly, b: &Assembly) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Visible", a.visible, b.visible);
        d.prop("Permission set", &a.permission_set, &b.permission_set);
        d.prop("Files", a.files.len(), b.files.len());
        for (i, f) in a.files.iter().enumerate() {
            match b.files.iter().find(|g| g.name.eq_ignore_ascii_case(&f.name)) {
                Some(g) if g.content == f.content => {}
                Some(_) => d.add(if i == 0 { "Assembly binary" } else { "File" }, Some(f.name.clone()), Some(f.name.clone())),
                None => d.only1("File", &f.name),
            }
        }
        self.grants(&mut d, "ASSEMBLY", &a.name, &b.name);
        d.out
    }

    fn xml_schema_collection(&self, a: &XmlSchemaCollection, b: &XmlSchemaCollection) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        if !self.opts.has(Opt::IgnoreSchemaObjectAuthorization) && !self.opts.has(Opt::IgnoreOwners) {
            d.opt_prop("Authorization", a.owner.as_deref(), b.owner.as_deref());
        }
        if xml_key(&a.definition) != xml_key(&b.definition) {
            d.add("Schema", Some(a.definition.clone()), Some(b.definition.clone()));
        }
        self.permissions(&mut d, "XML SCHEMA COLLECTION", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        self.xprops(&mut d, "XML SCHEMA COLLECTION", Some(&a.schema), &a.name, &b.name);
        d.out
    }

    fn bindable(&self, a: &BindableObject, b: &BindableObject, class: &str) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        // A statement terminator is not part of the definition.
        let text = |x: &str| x.trim_end().trim_end_matches(';').to_string();
        self.module_text(&mut d, &text(&a.definition), &text(&b.definition), false, false);
        self.xprops(&mut d, class, Some(&a.schema), &a.name, &b.name);
        d.out
    }

    fn ddl_trigger(&self, a: &DdlTrigger, b: &DdlTrigger) -> Vec<PropDiff> {
        let o = self.opts;
        let mut d = Diffs::default();
        d.prop("Disabled", a.disabled, b.disabled);
        d.prop("CLR", a.clr_method.is_some(), b.clr_method.is_some());
        if a.clr_method.is_some() && b.clr_method.is_some() {
            d.opt_prop("CLR method", a.clr_method.as_deref(), b.clr_method.as_deref());
            d.opt_prop("EXECUTE AS", a.execute_as.as_deref(), b.execute_as.as_deref());
            if !self.names(&a.events, &b.events) {
                d.add("Events", Some(a.events.join(", ")), Some(b.events.join(", ")));
            }
        } else {
            self.module_text(&mut d, &a.definition, &b.definition, a.encrypted, b.encrypted);
            if !o.has(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings) {
                self.settings(&mut d, &a.settings, &b.settings);
            }
        }
        self.xprops(&mut d, "TRIGGER", None, &a.name, &b.name);
        d.out
    }

    fn partition_function(&self, a: &PartitionFunction, b: &PartitionFunction) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        d.prop("Data type", a.data_type.sql(), b.data_type.sql());
        d.prop("Range", if a.range_right { "RIGHT" } else { "LEFT" }, if b.range_right { "RIGHT" } else { "LEFT" });
        if a.boundaries != b.boundaries {
            d.add("Boundary values", Some(a.boundaries.join(", ")), Some(b.boundaries.join(", ")));
        }
        self.xprops(&mut d, "PARTITION FUNCTION", None, &a.name, &b.name);
        d.out
    }

    fn partition_scheme(&self, a: &PartitionScheme, b: &PartitionScheme) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        if !self.same_name(&a.function, &b.function) {
            d.add("Partition function", Some(a.function.clone()), Some(b.function.clone()));
        }
        // One filegroup per partition counts; the NEXT USED one only when asked to.
        let partitions = |s: &PartitionScheme, db: &Database| {
            db.partition_functions.iter().find(|f| self.same_name(&f.name, &s.function)).map(|f| f.boundaries.len() + 1).unwrap_or(s.filegroups.len())
        };
        let (pa, pb) = (partitions(a, self.db1), partitions(b, self.db2));
        let n = pa.min(pb);
        let used = |s: &PartitionScheme, k: usize| s.filegroups.iter().take(k).map(|f| self.key(f)).collect::<Vec<_>>();
        let consider_next = self.opts.has(Opt::ConsiderNextFilegroupInPartitionSchemes);
        let (ka, kb) = if consider_next { (used(a, a.filegroups.len()), used(b, b.filegroups.len())) } else { (used(a, n), used(b, n)) };
        if ka != kb || (consider_next && pa != pb) {
            d.add("Filegroups", Some(a.filegroups.join(", ")), Some(b.filegroups.join(", ")));
        }
        self.xprops(&mut d, "PARTITION SCHEME", None, &a.name, &b.name);
        d.out
    }

    fn fulltext_catalog(&self, a: &FullTextCatalog, b: &FullTextCatalog) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Is default", a.is_default, b.is_default);
        d.prop("Is accent sensitive", a.accent_sensitive, b.accent_sensitive);
        self.grants(&mut d, "FULLTEXT CATALOG", &a.name, &b.name);
        d.out
    }

    fn stoplist(&self, a: &FullTextStoplist, b: &FullTextStoplist) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        for w in a.words.iter().filter(|w| !b.words.contains(w)) {
            d.add("Stopword", Some(format!("{} ({})", w.word, w.language)), None);
        }
        for w in b.words.iter().filter(|w| !a.words.contains(w)) {
            d.add("Stopword", None, Some(format!("{} ({})", w.word, w.language)));
        }
        self.grants(&mut d, "FULLTEXT STOPLIST", &a.name, &b.name);
        d.out
    }

    fn property_list(&self, a: &SearchPropertyList, b: &SearchPropertyList) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        for p in &a.properties {
            match b.properties.iter().find(|q| self.same_name(&q.name, &p.name)) {
                None => d.only1("Property", &p.name),
                Some(q) if q != p => d.add("Property", Some(p.name.clone()), Some(q.name.clone())),
                _ => {}
            }
        }
        for q in b.properties.iter().filter(|q| !a.properties.iter().any(|p| self.same_name(&p.name, &q.name))) {
            d.only2("Property", &q.name);
        }
        d.out
    }

    fn message_type(&self, a: &MessageType, b: &MessageType) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Validation", &a.validation, &b.validation);
        let collection = |m: &MessageType| m.xml_collection.as_ref().map(|c| self.key(c));
        if collection(a) != collection(b) {
            d.add("XML schema collection", a.xml_collection.clone(), b.xml_collection.clone());
        }
        self.grants(&mut d, "MESSAGE TYPE", &a.name, &b.name);
        d.out
    }

    fn contract(&self, a: &Contract, b: &Contract) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        let usages = |c: &Contract| {
            let mut v: Vec<String> = c.messages.iter().map(|m| format!("{} SENT BY {}", self.key(&m.message_type), m.sent_by)).collect();
            v.sort();
            v
        };
        if usages(a) != usages(b) {
            let text = |c: &Contract| c.messages.iter().map(|m| format!("[{}] SENT BY {}", m.message_type, m.sent_by)).collect::<Vec<_>>().join(", ");
            d.add("Message types", Some(text(a)), Some(text(b)));
        }
        self.grants(&mut d, "CONTRACT", &a.name, &b.name);
        d.out
    }

    fn queue(&self, a: &Queue, b: &Queue) -> Vec<PropDiff> {
        let o = self.opts;
        let mut d = Diffs::default();
        if !o.has(Opt::IgnoreSchemaObjectAuthorization) && !o.has(Opt::IgnoreOwners) {
            d.opt_prop("Authorization", a.owner.as_deref(), b.owner.as_deref());
        }
        d.prop("STATUS", a.enabled, b.enabled);
        d.prop("RETENTION", a.retention, b.retention);
        d.prop("POISON_MESSAGE_HANDLING", a.poison_message_handling, b.poison_message_handling);
        d.prop("Activation", a.activation.is_some(), b.activation.is_some());
        if let (Some(x), Some(y)) = (&a.activation, &b.activation) {
            d.prop("Activation status", x.enabled, y.enabled);
            if !self.same_name(&x.procedure, &y.procedure) {
                d.add("PROCEDURE_NAME", Some(x.procedure.clone()), Some(y.procedure.clone()));
            }
            d.prop("MAX_QUEUE_READERS", x.max_readers, y.max_readers);
            d.opt_prop("EXECUTE AS", x.execute_as.as_deref(), y.execute_as.as_deref());
        }
        if !o.has(Opt::IgnoreFileGroups) {
            d.opt_prop("Filegroup", a.filegroup.as_deref(), b.filegroup.as_deref());
        }
        if !o.has(Opt::IgnoreQueueEventNotifications) {
            for x in &a.event_notifications {
                match b.event_notifications.iter().find(|y| self.same_name(&x.name, &y.name)) {
                    Some(y) => {
                        d.enter("Event notification", Some(&x.name), Some(&y.name));
                        self.event_notification(&mut d, x, y);
                        d.leave();
                    }
                    None => d.only1("Event notification", &x.name),
                }
            }
            for y in b.event_notifications.iter().filter(|y| !a.event_notifications.iter().any(|x| self.same_name(&x.name, &y.name))) {
                d.only2("Event notification", &y.name);
            }
        }
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        self.xprops(&mut d, "QUEUE", Some(&a.schema), &a.name, &b.name);
        d.out
    }

    fn event_notification(&self, d: &mut Diffs, a: &EventNotification, b: &EventNotification) {
        d.prop("Service", &a.service, &b.service);
        d.prop("Broker instance", &a.broker_instance, &b.broker_instance);
        d.prop("FAN_IN", a.fan_in, b.fan_in);
        if !self.names(&a.events, &b.events) {
            d.add("Events", Some(a.events.join(", ")), Some(b.events.join(", ")));
        }
    }

    fn service(&self, a: &Service, b: &Service) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        if self.key(&a.queue.to_string()) != self.key(&b.queue.to_string()) {
            d.add("Queue", Some(a.queue.to_string()), Some(b.queue.to_string()));
        }
        if !self.names(&a.contracts, &b.contracts) {
            d.add("Contracts", Some(a.contracts.join(", ")), Some(b.contracts.join(", ")));
        }
        self.grants(&mut d, "SERVICE", &a.name, &b.name);
        d.out
    }

    fn route(&self, a: &Route, b: &Route) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.opt_prop("SERVICE_NAME", a.remote_service.as_deref(), b.remote_service.as_deref());
        d.opt_prop("BROKER_INSTANCE", a.broker_instance.as_deref(), b.broker_instance.as_deref());
        d.prop("ADDRESS", &a.address, &b.address);
        d.opt_prop("MIRROR_ADDRESS", a.mirror_address.as_deref(), b.mirror_address.as_deref());
        // The expiry moves on with the clock: only having a lifetime or not counts.
        d.prop("LIFETIME", a.expires.is_some(), b.expires.is_some());
        self.grants(&mut d, "ROUTE", &a.name, &b.name);
        d.out
    }

    fn service_binding(&self, a: &ServiceBinding, b: &ServiceBinding) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Remote service", &a.remote_service, &b.remote_service);
        d.prop("User", &a.user, &b.user);
        d.prop("ANONYMOUS", a.anonymous, b.anonymous);
        self.grants(&mut d, "REMOTE SERVICE BINDING", &a.name, &b.name);
        d.out
    }

    fn certificate(&self, a: &Certificate, b: &Certificate) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Active for dialog", a.active_for_dialog, b.active_for_dialog);
        d.prop("Key encryption type", &a.private_key, &b.private_key);
        d.prop("Issuer name", &a.issuer, &b.issuer);
        d.prop("Subject", &a.subject, &b.subject);
        d.prop("Thumb print", &a.thumbprint, &b.thumbprint);
        self.grants(&mut d, "CERTIFICATE", &a.name, &b.name);
        d.out
    }

    fn asymmetric_key(&self, a: &AsymmetricKey, b: &AsymmetricKey) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Thumb print", &a.thumbprint, &b.thumbprint);
        self.grants(&mut d, "ASYMMETRIC KEY", &a.name, &b.name);
        d.out
    }

    fn symmetric_key(&self, a: &SymmetricKey, b: &SymmetricKey) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        self.owner(&mut d, &a.owner, &b.owner);
        d.prop("Key algorithm", &a.algorithm, &b.algorithm);
        d.prop("Key length", a.key_length, b.key_length);
        self.grants(&mut d, "SYMMETRIC KEY", &a.name, &b.name);
        d.out
    }

    fn security_policy(&self, a: &SecurityPolicy, b: &SecurityPolicy) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        d.prop("STATE", a.enabled, b.enabled);
        d.prop("SCHEMABINDING", a.schema_bound, b.schema_bound);
        d.prop("NOT FOR REPLICATION", a.not_for_replication, b.not_for_replication);
        let args = |p: &SecurityPredicate| p.arguments.trim_matches(['(', ')']).split(',').map(|x| self.key(x.trim().trim_matches(['[', ']']))).collect::<Vec<_>>();
        let same_slot = |x: &SecurityPredicate, y: &SecurityPredicate| {
            x.kind == y.kind && x.operation == y.operation && self.key(&x.table.to_string()) == self.key(&y.table.to_string())
        };
        let label = |p: &SecurityPredicate| format!("{} {}{}", p.kind, p.table, p.operation.as_ref().map(|o| format!(" {o}")).unwrap_or_default());
        for x in &a.predicates {
            match b.predicates.iter().find(|y| same_slot(x, y)) {
                Some(y) if !self.same_name(&x.function, &y.function) || args(x) != args(y) => {
                    d.add("Security predicate", Some(format!("{}{}", x.function, x.arguments)), Some(format!("{}{}", y.function, y.arguments)))
                }
                Some(_) => {}
                None => d.only1("Security predicate", &label(x)),
            }
        }
        for y in b.predicates.iter().filter(|y| !a.predicates.iter().any(|x| same_slot(x, y))) {
            d.only2("Security predicate", &label(y));
        }
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    fn external_data_source(&self, a: &ExternalDataSource, b: &ExternalDataSource) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        d.prop("LOCATION", &a.location, &b.location);
        d.opt_prop("TYPE", a.kind.as_deref(), b.kind.as_deref());
        d.opt_prop("RESOURCE_MANAGER_LOCATION", a.resource_manager_location.as_deref(), b.resource_manager_location.as_deref());
        d.opt_prop("CREDENTIAL", a.credential.as_deref(), b.credential.as_deref());
        d.opt_prop("DATABASE_NAME", a.database_name.as_deref(), b.database_name.as_deref());
        d.opt_prop("SHARD_MAP_NAME", a.shard_map_name.as_deref(), b.shard_map_name.as_deref());
        d.opt_prop("CONNECTION_OPTIONS", a.connection_options.as_deref(), b.connection_options.as_deref());
        if let (Some(x), Some(y)) = (a.pushdown, b.pushdown) {
            d.prop("PUSHDOWN", x, y);
        }
        d.out
    }

    fn external_file_format(&self, a: &ExternalFileFormat, b: &ExternalFileFormat) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        if a != b {
            d.prop("FORMAT_TYPE", &a.format_type, &b.format_type);
            d.opt_prop("FIELD_TERMINATOR", a.field_terminator.as_deref(), b.field_terminator.as_deref());
            d.opt_prop("STRING_DELIMITER", a.string_delimiter.as_deref(), b.string_delimiter.as_deref());
            d.opt_prop("DATE_FORMAT", a.date_format.as_deref(), b.date_format.as_deref());
            d.prop("USE_TYPE_DEFAULT", a.use_type_default, b.use_type_default);
            d.opt_prop("FIRST_ROW", a.first_row.map(|n| n.to_string()).as_deref(), b.first_row.map(|n| n.to_string()).as_deref());
            d.opt_prop("SERDE_METHOD", a.serde_method.as_deref(), b.serde_method.as_deref());
            d.opt_prop("ENCODING", a.encoding.as_deref(), b.encoding.as_deref());
            d.opt_prop("DATA_COMPRESSION", a.data_compression.as_deref(), b.data_compression.as_deref());
        }
        d.out
    }

    fn external_table(&self, a: &ExternalTable, b: &ExternalTable) -> Vec<PropDiff> {
        let mut d = Diffs::default();
        if !self.same_name(&a.data_source, &b.data_source) {
            d.add("DATA_SOURCE", Some(a.data_source.clone()), Some(b.data_source.clone()));
        }
        d.opt_prop("FILE_FORMAT", a.file_format.as_deref(), b.file_format.as_deref());
        d.opt_prop("LOCATION", a.location.as_deref(), b.location.as_deref());
        d.opt_prop("REJECT_TYPE", a.reject_type.as_deref(), b.reject_type.as_deref());
        d.opt_prop("REJECT_VALUE", a.reject_value.as_deref(), b.reject_value.as_deref());
        d.opt_prop("REJECT_SAMPLE_VALUE", a.reject_sample_value.as_deref(), b.reject_sample_value.as_deref());
        d.opt_prop("SCHEMA_NAME", a.remote_schema.as_deref(), b.remote_schema.as_deref());
        d.opt_prop("OBJECT_NAME", a.remote_object.as_deref(), b.remote_object.as_deref());
        d.opt_prop("DISTRIBUTION", a.distribution.as_deref(), b.distribution.as_deref());
        let cols = |t: &ExternalTable| t.columns.iter().map(|c| format!("{} {} {}", self.key(&c.name), c.data_type.sql().to_lowercase(), c.nullable)).collect::<Vec<_>>();
        if cols(a) != cols(b) {
            let show = |t: &ExternalTable| t.columns.iter().map(|c| format!("[{}] {}", c.name, c.data_type.sql())).collect::<Vec<_>>().join(", ");
            d.add("Columns", Some(show(a)), Some(show(b)));
        }
        self.permissions(&mut d, "OBJECT", &a.schema, Some(&a.name), &b.schema, Some(&b.name));
        d.out
    }

    // ---- Parts of tables, views and modules ----

    /// Statistics, the full-text index and edge constraints of a table.
    pub(super) fn table_extras(&self, d: &mut Diffs, a: &Table, b: &Table) {
        self.statistics(d, &a.statistics, &b.statistics);
        self.fulltext_index(d, a.fulltext_index.as_ref(), b.fulltext_index.as_ref());
        let left: Vec<&EdgeConstraint> = a.edge_constraints.iter().collect();
        let right: Vec<&EdgeConstraint> = b.edge_constraints.iter().collect();
        let o = self.opts;
        let compare = |x: &EdgeConstraint, y: &EdgeConstraint, d: &mut Diffs| {
            let set = |e: &EdgeConstraint| {
                let mut v: Vec<String> = e.connections.iter().map(|(f, t)| self.key(&format!("{f} TO {t}"))).collect();
                v.sort();
                v
            };
            if set(x) != set(y) {
                let show = |e: &EdgeConstraint| e.connections.iter().map(|(f, t)| format!("{f} TO {t}")).collect::<Vec<_>>().join(", ");
                d.add("Connections", Some(show(x)), Some(show(y)));
            }
            if !o.has(Opt::IgnoreNocheckAndWithNocheck) {
                d.prop("NOCHECK", x.disabled, y.disabled);
                if !o.has(Opt::IgnoreWithNocheck) {
                    d.prop("WITH NOCHECK", x.not_trusted, y.not_trusted);
                }
            }
            d.prop("ON DELETE CASCADE", x.on_delete_cascade, y.on_delete_cascade);
        };
        self.pair_named(d, "Edge constraint", &left, &right, |e| (&e.name, e.system_named), compare);
    }

    pub(super) fn statistics(&self, d: &mut Diffs, a: &[Statistic], b: &[Statistic]) {
        let o = self.opts;
        if o.has(Opt::IgnoreStatistics) {
            return;
        }
        for x in a {
            match b.iter().find(|y| self.same_name(&x.name, &y.name)) {
                None => d.only1("Statistics", &x.name),
                Some(y) => {
                    d.enter("Statistics", Some(&x.name), Some(&y.name));
                    let cols = |s: &Statistic| {
                        let mut v: Vec<String> = s.columns.iter().map(|c| self.key(c)).collect();
                        if !o.has(Opt::ForceColumnOrder) {
                            v.sort();
                        }
                        v
                    };
                    if cols(x) != cols(y) {
                        d.add("Columns", Some(x.columns.join(", ")), Some(y.columns.join(", ")));
                    }
                    match (&x.filter, &y.filter) {
                        (Some(p), Some(q)) if text::compare(p, q, TextKind::Expression, o).is_some() => d.add("Filter", Some(p.clone()), Some(q.clone())),
                        (Some(_), Some(_)) => {}
                        (p, q) => d.opt_prop("Filter", p.as_deref(), q.as_deref()),
                    }
                    if !o.has(Opt::IgnoreStatisticsNorecompute) {
                        d.prop("No Recompute", x.no_recompute, y.no_recompute);
                    }
                    if !o.has(Opt::IgnoreStatisticsIncremental) {
                        d.prop("Incremental", x.incremental, y.incremental);
                    }
                    d.leave();
                }
            }
        }
        for y in b.iter().filter(|y| !a.iter().any(|x| self.same_name(&x.name, &y.name))) {
            d.only2("Statistics", &y.name);
        }
    }

    pub(super) fn fulltext_index(&self, d: &mut Diffs, a: Option<&FullTextIndex>, b: Option<&FullTextIndex>) {
        let o = self.opts;
        if o.has(Opt::IgnoreFullTextIndexing) {
            return;
        }
        let (x, y) = match (a, b) {
            (None, None) => return,
            (Some(x), None) => return d.only1("Full text index", &x.catalog),
            (None, Some(y)) => return d.only2("Full text index", &y.catalog),
            (Some(x), Some(y)) => (x, y),
        };
        d.enter("Full text index", Some(&x.catalog), Some(&y.catalog));
        if !self.same_name(&x.catalog, &y.catalog) {
            d.add("Catalog", Some(x.catalog.clone()), Some(y.catalog.clone()));
        }
        if !self.same_name(&x.key_index, &y.key_index) {
            d.add("Key index", Some(x.key_index.clone()), Some(y.key_index.clone()));
        }
        if !o.has(Opt::IgnoreFileGroups) {
            d.opt_prop("Filegroup", x.filegroup.as_deref(), y.filegroup.as_deref());
        }
        d.prop("Change tracking", &x.change_tracking, &y.change_tracking);
        if self.key(&x.stoplist) != self.key(&y.stoplist) {
            d.add("Stoplist", Some(x.stoplist.clone()), Some(y.stoplist.clone()));
        }
        d.opt_prop("Search property list", x.property_list.as_deref(), y.property_list.as_deref());
        for c in &x.columns {
            match y.columns.iter().find(|e| self.same_name(&e.name, &c.name)) {
                None => d.only1("Full text column", &c.name),
                Some(e) => {
                    d.prop("Language", c.language, e.language);
                    d.opt_prop("Type column", c.type_column.as_deref(), e.type_column.as_deref());
                }
            }
        }
        for e in y.columns.iter().filter(|e| !x.columns.iter().any(|c| self.same_name(&c.name, &e.name))) {
            d.only2("Full text column", &e.name);
        }
        d.leave();
    }

    pub(super) fn sensitivity(&self, d: &mut Diffs, a: &Column, b: &Column) {
        if self.opts.has(Opt::IgnoreSensitivityClassification) {
            return;
        }
        let none = Sensitivity::default();
        let (x, y) = (a.sensitivity.as_ref().unwrap_or(&none), b.sensitivity.as_ref().unwrap_or(&none));
        d.opt_prop("Sensitivity classification label", x.label.as_deref(), y.label.as_deref());
        d.opt_prop("Sensitivity classification label id", x.label_id.as_deref(), y.label_id.as_deref());
        d.opt_prop("Sensitivity classification information type", x.information_type.as_deref(), y.information_type.as_deref());
        d.opt_prop("Sensitivity classification information type id", x.information_type_id.as_deref(), y.information_type_id.as_deref());
        d.opt_prop("Sensitivity classification rank", x.rank.as_deref(), y.rank.as_deref());
    }

    pub(super) fn clr_signature(&self, d: &mut Diffs, a: &Routine, b: &Routine) {
        let params = |r: &Routine| {
            r.parameters
                .iter()
                .map(|p| format!("{} {}{}{}", p.name, p.data_type, p.default.as_ref().map(|v| format!("={v}")).unwrap_or_default(), if p.output { " OUTPUT" } else { "" }))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if self.key(&params(a)) != self.key(&params(b)) {
            d.add("Parameters", Some(params(a)), Some(params(b)));
        }
        d.opt_prop("Returns", a.returns.as_deref(), b.returns.as_deref());
        d.prop("RETURNS NULL ON NULL INPUT", a.null_on_null_input, b.null_on_null_input);
        let cols = |r: &Routine| r.result_columns.iter().map(|c| format!("[{}] {}", c.name, c.data_type.sql())).collect::<Vec<_>>().join(", ");
        if self.key(&cols(a)) != self.key(&cols(b)) {
            d.add("Result columns", Some(cols(a)), Some(cols(b)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names() {
        for (s, t) in [
            ("MessageType", ObjectType::MessageType),
            ("full text catalog", ObjectType::FullTextCatalog),
            ("DdlTrigger", ObjectType::DdlTrigger),
            ("xml schema collection", ObjectType::XmlSchemaCollection),
            ("ServiceBinding", ObjectType::ServiceBinding),
            ("ExternalTable", ObjectType::ExternalTable),
        ] {
            assert_eq!(ObjectType::parse(s), Some(t), "{s}");
        }
        assert_eq!(super::super::report::type_name(ObjectType::SearchPropertyList), "SearchPropertyList");
    }

    #[test]
    fn statistics_and_policies_compare() {
        let st = |cols: &[&str], nr: bool| Statistic { name: "S".into(), columns: cols.iter().map(|c| c.to_string()).collect(), no_recompute: nr, ..Default::default() };
        let table = |s: Statistic| Table { schema: "dbo".into(), name: "T".into(), statistics: vec![s], ..Default::default() };
        let db = |t: Table| Database { tables: vec![t], ..Default::default() };
        let opts = super::super::Options::default_set();
        let c = super::super::compare(&db(table(st(&["a", "b"], false))), &db(table(st(&["b", "a"], true))), &opts, &Default::default());
        let names: Vec<&str> = c.differences[0].properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["No Recompute"]);
        let c = super::super::compare(&db(table(st(&["a"], false))), &db(table(st(&["a"], true))), &opts.clone().with(Opt::IgnoreStatistics), &Default::default());
        assert!(c.identical());
    }

    #[test]
    fn xml_without_layout() {
        assert_eq!(xml_key("<a>\r\n  <b x=\"1\"/>\r\n</a>"), "<a><b x=\"1\"/></a>");
        assert_eq!(xml_key("<a> text </a>"), "<a> text </a>");
    }
}
