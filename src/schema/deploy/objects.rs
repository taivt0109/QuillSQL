//! Deployment of the objects beyond tables and modules, and of the table parts they bring.
//! Where each kind goes in the script follows SQL Compare: certificates, keys and full-text
//! work before the transaction, Service Broker objects and security policies among the
//! dependency-ordered core objects, the rest in their own phases.

use std::collections::{BTreeSet, HashSet};

use super::*;
use crate::schema::warnings::objects as owarn;

/// Planning state of this module.
#[derive(Default)]
pub(super) struct State {
    /// Service Broker objects (by dependency key) that have to be dropped and created again
    /// because something they use is.
    rebuild: BTreeSet<ObjectName>,
    /// Service Broker objects already scripted.
    done: HashSet<ObjectName>,
    /// Assemblies created or altered (for DBCC FREESYSTEMCACHE).
    assemblies: Vec<String>,
    /// DDL triggers created by the script, enabled in the source.
    ddl_created: Vec<String>,
    /// Full-text index columns are added (the High in-transaction warning).
    fulltext_columns: bool,
    /// New tables whose primary key is part of their CREATE TABLE.
    pub(super) inline_key: HashSet<ObjectName>,
}

/// The class of an object's extended properties (sp_addextendedproperty's level 0 or 1 type).
pub(super) fn xprop_class(t: ObjectType) -> Option<&'static str> {
    Some(match t {
        ObjectType::Assembly => "ASSEMBLY",
        ObjectType::XmlSchemaCollection => "XML SCHEMA COLLECTION",
        ObjectType::Default => "DEFAULT",
        ObjectType::Rule => "RULE",
        ObjectType::DdlTrigger => "TRIGGER",
        ObjectType::PartitionFunction => "PARTITION FUNCTION",
        ObjectType::PartitionScheme => "PARTITION SCHEME",
        ObjectType::MessageType => "MESSAGE TYPE",
        ObjectType::Contract => "CONTRACT",
        ObjectType::Queue => "QUEUE",
        ObjectType::Service => "SERVICE",
        ObjectType::Route => "ROUTE",
        ObjectType::ServiceBinding => "REMOTE SERVICE BINDING",
        _ => return None,
    })
}

/// The class keyword in GRANT … ON <class>:: for database-wide securables.
pub(super) fn securable_class(t: ObjectType) -> Option<&'static str> {
    Some(match t {
        ObjectType::Assembly => "ASSEMBLY",
        ObjectType::MessageType => "MESSAGE TYPE",
        ObjectType::Contract => "CONTRACT",
        ObjectType::Service => "SERVICE",
        ObjectType::Route => "ROUTE",
        ObjectType::ServiceBinding => "REMOTE SERVICE BINDING",
        ObjectType::FullTextCatalog => "FULLTEXT CATALOG",
        ObjectType::FullTextStoplist => "FULLTEXT STOPLIST",
        ObjectType::Certificate => "CERTIFICATE",
        ObjectType::AsymmetricKey => "ASYMMETRIC KEY",
        ObjectType::SymmetricKey => "SYMMETRIC KEY",
        _ => return None,
    })
}

fn named<'x, T>(list: &'x [T], name: &str, f: impl Fn(&T) -> &str) -> Option<&'x T> {
    list.iter().find(|x| f(x) == name)
}

fn scoped<'x, T>(list: &'x [T], n: &ObjectName, f: impl Fn(&T) -> (&str, &str)) -> Option<&'x T> {
    list.iter().find(|x| f(x) == (n.schema.as_str(), n.name.as_str()))
}

fn alter_authorization(class: &str, name: &str, owner: &Option<String>) -> String {
    format!("ALTER AUTHORIZATION ON {class}::{} TO {}", quote(name), quote(owner.as_deref().unwrap_or("dbo")))
}

/// Retention as days, for telling whether it gets shorter.
fn retention_days(r: &Option<String>) -> Option<i64> {
    let r = r.as_deref()?;
    let mut parts = r.split_whitespace();
    let n: i64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?.to_ascii_uppercase();
    Some(n * match unit.trim_end_matches('S') {
        "DAY" => 1,
        "WEEK" => 7,
        "MONTH" => 30,
        "YEAR" => 365,
        _ => 1,
    })
}

impl<'a> Planner<'a> {
    fn owners_ignored(&self) -> bool {
        self.opts.has(Opt::IgnoreOwners)
    }

    /// A step that ranks with `key` in a dependency-ordered phase.
    fn core(&mut self, phase: Phase, print: String, key: ObjectName, statements: Vec<String>) {
        self.push(Step { object: Some(key), ..Step::new(phase, print, statements) });
    }

    fn ranked_step(&mut self, phase: Phase, rank: usize, print: impl Into<String>, statements: Vec<String>, group: bool) {
        self.push(Step { rank, group, ..Step::new(phase, print, statements) });
    }

    pub(super) fn more_object(&mut self, d: &'a Difference) {
        match d.object_type {
            ObjectType::Assembly => self.assembly(d),
            ObjectType::XmlSchemaCollection => self.xml_schema_collection(d),
            ObjectType::Default | ObjectType::Rule => self.bindable(d),
            ObjectType::DdlTrigger => self.ddl_trigger(d),
            ObjectType::PartitionFunction => self.partition_function(d),
            ObjectType::PartitionScheme => self.partition_scheme(d),
            ObjectType::FullTextCatalog => self.fulltext_catalog(d),
            ObjectType::FullTextStoplist => self.stoplist(d),
            ObjectType::SearchPropertyList => self.property_list(d),
            ObjectType::MessageType
            | ObjectType::Contract
            | ObjectType::Queue
            | ObjectType::Service
            | ObjectType::ServiceBinding
            | ObjectType::EventNotification => self.broker(d),
            ObjectType::Route => self.route(d),
            ObjectType::Certificate | ObjectType::AsymmetricKey | ObjectType::SymmetricKey => self.crypto(d),
            ObjectType::SecurityPolicy => self.security_policy(d),
            ObjectType::ExternalDataSource | ObjectType::ExternalFileFormat | ObjectType::ExternalTable => self.external(d),
            _ => {}
        }
    }

    /// Work that needs every difference planned first.
    pub(super) fn finish_objects(&mut self) {
        // Assemblies re-created because one they reference is.
        let (src, tgt) = (self.src, self.tgt);
        for name in self.prepared.rebuilt_assemblies.iter() {
            if self.cmp.differences.iter().any(|d| d.object_type == ObjectType::Assembly && d.selected && d.kind != DiffKind::Equal && self.same(&d.name.name, name)) {
                continue;
            }
            if let Some(t) = named(&tgt.assemblies, name, |a| &a.name) {
                self.drop_assembly(t, &ObjectName::new(KEY_ASSEMBLY, name));
                if let Some(s) = named(&src.assemblies, name, |a| &a.name) {
                    self.create_assembly(s);
                }
            }
        }
        self.recreate_clr_types();
        self.rebind_by_rebuild();
        // Service Broker objects that use something re-created come back too.
        let pending: Vec<ObjectName> = self.objects.rebuild.iter().filter(|k| !self.objects.done.contains(*k)).cloned().collect();
        for key in pending {
            self.broker_rebuild(&key);
        }
        if self.has(Feature::FreeSystemCache) {
            let changed: Vec<&String> = self.objects.assemblies.iter().collect();
            let referenced = self.src.assemblies.iter().any(|a| changed.contains(&&a.name) && a.references.iter().any(|r| changed.contains(&r)));
            if referenced {
                self.ranked_step(Phase::DropAssemblies, 9, "", vec!["DBCC FREESYSTEMCACHE ('ALL')".into()], false);
            }
        }
        if self.opts.has(Opt::DisableAndReenableDdlTriggers) && !self.objects.ddl_created.is_empty() {
            let enable: Vec<String> = self.objects.ddl_created.iter().map(|t| format!("ENABLE TRIGGER {} ON DATABASE", quote(t))).collect();
            self.group(Phase::EnableDdlTriggers, "Re-enabling DDL triggers", enable);
        }
        if self.objects.fulltext_columns && !self.opts.has(Opt::NoTransactions) && !self.opts.has(Opt::AssumeAlreadyInTransaction) {
            self.warn(owarn::fulltext_in_transaction());
        }
        if !self.opts.has(Opt::NoTransactions) {
            let level = self.settings.transaction_isolation_level.to_ascii_uppercase();
            if !matches!(level.as_str(), "READ COMMITTED" | "READ UNCOMMITTED") {
                let selected = self.selected();
                let creates = |t: ObjectType| selected.iter().any(|d| d.object_type == t && d.kind != DiffKind::OnlyIn2);
                let wanted: Vec<bool> = [ObjectType::ExternalFileFormat, ObjectType::ExternalDataSource, ObjectType::ExternalTable].map(creates).to_vec();
                for (i, what) in ["external file formats", "external data sources", "external tables"].iter().enumerate() {
                    if wanted[i] {
                        self.warn(owarn::polybase_isolation(what, &level));
                    }
                }
            }
        }
    }

    /// Objects other than modules that must go when what they are bound to changes: security
    /// policies on a function or table that is dropped. Returns whether `dep` was one.
    pub(super) fn object_dependant(&mut self, dep: &ObjectName) -> bool {
        let (src, tgt) = (self.src, self.tgt);
        if scoped(&tgt.security_policies, dep, |p| (&p.schema, &p.name)).is_none() {
            return false;
        }
        if self.dropped.insert(dep.clone()) {
            self.core(Phase::DropObjects, format!("Dropping {dep}"), dep.clone(), vec![self.ex.object(dep, "SP", Check::Drop, &format!("DROP SECURITY POLICY {dep}"))]);
            if !self.recreate.contains(dep)
                && let Some(s) = scoped(&src.security_policies, dep, |p| (&p.schema, &p.name))
            {
                self.create_security_policy(dep, s);
            }
        }
        true
    }

    /// A table whose check constraints, defaults or computed columns call a function that is
    /// dropped: those parts go before the drop and come back (from the source when it has the
    /// table) once the function is there again. Returns whether `dep` was such a table.
    pub(super) fn function_user(&mut self, function: &ObjectName, dep: &ObjectName) -> bool {
        let (src, tgt) = (self.src, self.tgt);
        let Some(t) = tgt.table(&dep.schema, &dep.name) else { return false };
        if !self.dropped.contains(function) || !tgt.routines.iter().any(|r| r.schema == function.schema && r.name == function.name) {
            return true;
        }
        if self.dropped.contains(dep) || self.recreate.contains(dep) {
            return true;
        }
        let needle = format!("[{}].[{}]", function.schema.to_lowercase(), function.name.to_lowercase());
        let calls = |text: &str| text.to_lowercase().contains(&needle);
        let s = src.table(&dep.schema, &dep.name);
        let table = dep.quoted();
        for ck in t.check_constraints.iter().filter(|c| calls(&c.definition)) {
            self.drop_check(dep, &ck.name);
            let ck = s.and_then(|s| s.check_constraints.iter().find(|c| self.same(&c.name, &ck.name))).unwrap_or(ck);
            self.add_check(dep, ck);
        }
        for c in &t.columns {
            let sc = s.and_then(|s| s.columns.iter().find(|x| self.same(&x.name, &c.name))).unwrap_or(c);
            if let Some(df) = c.default.as_ref().filter(|df| calls(&df.definition)) {
                self.drop_default(dep, &c.name, df);
                if let Some(df) = &sc.default {
                    let sql = self.ex.default_constraint(dep, &sc.name, &df.name, Check::Create, &ddl::add_default(&table, &sc.name, df));
                    self.step(Phase::CreateDefaults, format!("Altering {dep}"), vec![sql]);
                }
            }
            if c.computed.as_ref().is_some_and(|x| calls(&x.definition)) {
                self.release_column(t, &c.name, false);
                let drop = self.ex.columns(dep, &[&c.name], Check::Drop, &format!("ALTER TABLE {table} DROP COLUMN {}", quote(&c.name)));
                self.step(Phase::DropDefaults, format!("Altering {dep}"), vec![drop]);
                if sc.computed.is_some() {
                    let def = ddl::column_definition_with(sc, true, &self.r, t.memory_optimized);
                    let add = self.ex.columns(dep, &[&sc.name], Check::Create, &format!("ALTER TABLE {table} ADD {def}"));
                    self.step(Phase::CreateDefaults, format!("Altering {dep}"), vec![add]);
                }
            }
        }
        true
    }

    // ---- CLR assemblies ----

    fn assembly(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.assemblies, &d.name.name, |a| &a.name);
        let t = named(&tgt.assemblies, &self.target_name(d).name, |a| &a.name);
        let key = d.key();
        match (s, t) {
            (Some(s), None) => self.create_assembly(s),
            (None, Some(t)) => self.drop_assembly(t, &key),
            (Some(s), Some(t)) => {
                // The same public surface is altered in place; anything else is re-created.
                let rebuild = self.prepared.rebuilt_assemblies.contains(&t.name) || super::unbind::assembly_rebuilt(s, t, self.opts);
                if rebuild {
                    self.drop_assembly(t, &key);
                    self.create_assembly(s);
                } else {
                    let mut statements = Vec::new();
                    if s.owner != t.owner && !self.owners_ignored() {
                        statements.push(alter_authorization("ASSEMBLY", &s.name, &s.owner));
                    }
                    statements.extend(ddl_objects::alter_assembly(s, t));
                    self.objects.assemblies.push(s.name.clone());
                    self.ranked_step(Phase::CreateAssemblies, 1, "Altering CLR assemblies", statements, true);
                }
            }
            _ => {}
        }
    }

    pub(super) fn create_assembly(&mut self, s: &Assembly) {
        self.objects.assemblies.push(s.name.clone());
        self.recreate.insert(ObjectName::new("", &s.name));
        let mut statements = ddl_objects::create_assembly(s);
        let check = format!("sys.assemblies WHERE name = {}", ddl::sql_string(&s.name));
        statements[0] = self.ex.listed(&check, Check::Create, &statements[0]);
        // Referenced assemblies first.
        let rank = self.assembly_depth(&s.name);
        self.push(Step { rank, group: true, ..Step::new(Phase::CreateAssemblies, "Creating CLR assemblies", statements) });
    }

    fn assembly_depth(&self, name: &str) -> usize {
        let mut depth = 0;
        let mut current = vec![name.to_string()];
        while depth < 32 {
            let next: Vec<String> = self.src.assemblies.iter().filter(|a| current.contains(&a.name)).flat_map(|a| a.references.clone()).collect();
            if next.is_empty() {
                break;
            }
            current = next;
            depth += 1;
        }
        // Planning order within one phase is the rank: deeper references go first.
        100 - depth.min(99)
    }

    pub(super) fn drop_assembly(&mut self, t: &Assembly, key: &ObjectName) {
        // CLR modules, triggers and types built on it go first and come back from the source.
        self.disturbed.insert(key.clone());
        let tgt = self.tgt;
        for ty in tgt.types.iter().filter(|ty| ty.clr_class.as_deref().is_some_and(|c| c.starts_with(&format!("{}.", quote(&t.name))))) {
            // Tables that can't go through the two-step rebuild (temporal, memory-optimized).
            let users = tgt.tables.iter().any(|tab| {
                tab.columns.iter().any(|c| c.data_type.schema.as_deref() == Some(&ty.schema) && c.data_type.name == ty.name) && self.unbound_copy(&tab.object_name()).is_none()
            });
            if users {
                self.warn(owarn::clr_type_unbind(&ObjectName::new(&ty.schema, &ty.name).to_string()));
            }
        }
        let sql = if t.visible {
            let check = format!("sys.assemblies WHERE name = {}", ddl::sql_string(&t.name));
            self.ex.listed(&check, Check::Drop, &format!("DROP ASSEMBLY {}", quote(&t.name)))
        } else {
            format!(" -- Assembly {} will be dropped automatically by SQL Server when it is no longer referenced.", quote(&t.name))
        };
        // Referencing assemblies are dropped first.
        let depth = tgt.assemblies.iter().filter(|a| a.references.contains(&t.name)).count();
        self.push(Step { rank: 10 - depth.min(9), group: true, ..Step::new(Phase::DropAssemblies, "Dropping CLR assemblies", vec![sql]) });
    }

    // ---- XML schema collections ----

    fn xml_schema_collection(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let tn = self.target_name(d);
        let s = scoped(&src.xml_schema_collections, &d.name, |x| (&x.schema, &x.name));
        let t = scoped(&tgt.xml_schema_collections, &tn, |x| (&x.schema, &x.name));
        let test = |n: &ObjectName| format!("sys.xml_schema_collections WHERE name = {} AND schema_id = SCHEMA_ID({})", ddl::sql_string(&n.name), ddl::sql_string(&n.schema));
        if let Some(t) = t {
            if s.is_some() {
                self.unbind_xml_columns(t);
            }
            // Modules with parameters of the collection go first and come back.
            self.disturbed.insert(tn.clone());
            // Message types that validate against it are dropped first and come back.
            let collection = tn.quoted();
            for m in tgt.message_types.iter().filter(|m| m.xml_collection.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&collection))) {
                self.objects.rebuild.insert(ObjectName::new(KEY_MESSAGE_TYPE, &m.name));
            }
            let sql = self.ex.listed(&test(&tn), Check::Drop, &format!("DROP XML SCHEMA COLLECTION {tn}"));
            self.ranked_step(Phase::XmlSchemaCollections, 0, "Dropping XML schema collections", vec![sql], true);
        }
        if let Some(s) = s {
            self.recreate.insert(d.name.clone());
            let sql = self.ex.listed(&test(&d.name), Check::Create, &ddl_objects::create_xml_schema_collection(s));
            self.ranked_step(Phase::XmlSchemaCollections, 1, "Creating XML schema collections", vec![sql], true);
        }
    }

    /// Typed xml columns of tables that stay are made untyped while their collection is
    /// re-created, and typed again afterwards.
    fn unbind_xml_columns(&mut self, x: &XmlSchemaCollection) {
        let collection = ObjectName::new(&x.schema, &x.name).quoted();
        let tgt = self.tgt;
        let staying: Vec<&Table> = tgt.tables.iter().filter(|t| !self.dropped.contains(&t.object_name()) && !self.recreate.contains(&t.object_name())).collect();
        for t in staying {
            for c in t.columns.iter().filter(|c| c.data_type.xml_collection.as_deref().is_some_and(|xc| xc.eq_ignore_ascii_case(&collection))) {
                let table = t.object_name();
                if !self.opts.has(Opt::IncludeDependencies) {
                    self.warn(warn::strong_dependencies(&collection, &table.to_string()));
                    continue;
                }
                self.warn(owarn::xml_collection_rebuild(&collection, &c.name, &table.to_string()));
                let null = if c.nullable { "NULL" } else { "NOT NULL" };
                self.step(
                    Phase::UnbindXml,
                    format!("Unbind XML schema collection {collection} from column {} in table {table}", c.name),
                    vec![format!("ALTER TABLE {table} ALTER COLUMN {} XML {null}", quote(&c.name))],
                );
                let doc = if c.data_type.xml_document { "DOCUMENT " } else { "" };
                self.step(
                    Phase::RebindXml,
                    format!("Bind XML schema collection {collection} to column {} in table {table}", c.name),
                    vec![format!("ALTER TABLE {table} ALTER COLUMN {} XML ({doc}{collection}) {null}", quote(&c.name))],
                );
            }
        }
    }

    // ---- Defaults and rules ----

    fn bindable(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let rule = d.object_type == ObjectType::Rule;
        let (sl, tl) = if rule { (&src.rules, &tgt.rules) } else { (&src.defaults, &tgt.defaults) };
        let tn = self.target_name(d);
        let s = scoped(sl, &d.name, |x| (&x.schema, &x.name));
        let t = scoped(tl, &tn, |x| (&x.schema, &x.name));
        let (kind, code, plural) = if rule { ("RULE", "R", "rules") } else { ("DEFAULT", "D", "defaults") };
        if t.is_some() {
            // Columns bound to it let go first; they are bound again from the source.
            let bound = tn.quoted();
            for tab in tgt.tables.iter() {
                for c in &tab.columns {
                    let b = if rule { &c.bound_rule } else { &c.bound_default };
                    if b.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&bound)) {
                        self.unbind(&tab.object_name(), c);
                        if s.is_some()
                            && let Some(st) = src.table(&tab.schema, &tab.name)
                            && !self.recreate.contains(&st.object_name())
                        {
                            let only = [c.name.as_str()];
                            self.bind_columns(st, Some(&only), !rule);
                        }
                    }
                }
            }
            let sql = self.ex.object(&tn, code, Check::Drop, &format!("DROP {kind} {tn}"));
            self.ranked_step(Phase::DropBindables, usize::from(!rule), format!("Dropping {plural}"), vec![sql], true);
        }
        if let Some(s) = s {
            self.recreate.insert(d.name.clone());
            let sql = self.ex.object(&d.name, code, Check::Create, &ddl_objects::create_bindable(s));
            self.push(Step { rank: usize::from(rule), group: true, settings: Some(ModuleSettings::default()), ..Step::new(Phase::CreateBindables, format!("Creating {plural}"), vec![sql]) });
        }
    }

    // ---- DDL triggers ----

    fn ddl_trigger(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.ddl_triggers, &d.name.name, |t| &t.name);
        let t = named(&tgt.ddl_triggers, &self.target_name(d).name, |t| &t.name);
        let disable_all = self.opts.has(Opt::DisableAndReenableDdlTriggers);
        let label = quote(&d.name.name);
        if s.is_some_and(|s| s.encrypted) || t.is_some_and(|t| t.encrypted) && s.is_some() {
            self.warn(warn::nondecryptable("DDL trigger", &label));
            return;
        }
        let drop = |p: &Self, t: &DdlTrigger| p.ex.ddl_trigger(&t.name, Check::Drop, &format!("DROP TRIGGER {} ON DATABASE", quote(&t.name)));
        match (s, t) {
            (None, Some(t)) => {
                let sql = drop(self, t);
                self.group(Phase::DropDdlTriggers, "Dropping DDL triggers", vec![sql]);
            }
            (Some(s), t) => {
                let rebuild = t.is_some_and(|t| t.clr_method.is_some() != s.clr_method.is_some()) || (t.is_some() && self.opts.has(Opt::DropAndCreateInsteadOfAlter));
                if let Some(t) = t.filter(|_| rebuild) {
                    let sql = drop(self, t);
                    self.group(Phase::DropDdlTriggers, "Dropping DDL triggers", vec![sql]);
                } else if t.is_none() && self.rerunnable() {
                    let sql = drop(self, s);
                    self.group(Phase::DropDdlTriggers, "Dropping DDL triggers", vec![sql]);
                }
                let create = t.is_none() || rebuild;
                if create {
                    self.recreate.insert(ObjectName::new("", &s.name));
                }
                let mut statements = vec![if create {
                    self.ex.ddl_trigger(&s.name, Check::Create, &ddl_objects::ddl_trigger(s, self.ddl_verb("CREATE")))
                } else {
                    self.ex.ddl_trigger(&s.name, Check::Alter, &ddl_objects::ddl_trigger(s, self.ddl_verb("ALTER")))
                }];
                let was_disabled = if create { false } else { t.is_some_and(|t| t.disabled) };
                if s.disabled || (create && disable_all) {
                    statements.push(format!("DISABLE TRIGGER {label} ON DATABASE"));
                } else if was_disabled && !disable_all {
                    statements.push(format!("ENABLE TRIGGER {label} ON DATABASE"));
                }
                if create && disable_all && !s.disabled {
                    self.objects.ddl_created.push(s.name.clone());
                }
                let print = if create { "Creating DDL triggers" } else { "Altering DDL triggers" };
                self.push(Step { rank: usize::from(!create), group: true, settings: Some(s.settings.clone()), ..Step::new(Phase::DdlTriggers, print, statements) });
            }
            _ => {}
        }
    }

    // ---- Partition functions and schemes ----

    fn partition_function(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.partition_functions, &d.name.name, |f| &f.name);
        let t = named(&tgt.partition_functions, &self.target_name(d).name, |f| &f.name);
        let test = |n: &str| format!("sys.partition_functions WHERE name = {}", ddl::sql_string(n));
        let create = |p: &mut Self, s: &PartitionFunction| {
            p.recreate.insert(ObjectName::new("", &s.name));
            let sql = p.ex.listed(&test(&s.name), Check::Create, &ddl_objects::create_partition_function(s));
            p.ranked_step(Phase::Partitioning, 2, "Creating partition functions", vec![sql], true);
        };
        let drop = |p: &mut Self, t: &PartitionFunction| {
            let sql = p.ex.listed(&test(&t.name), Check::Drop, &format!("DROP PARTITION FUNCTION {}", quote(&t.name)));
            p.ranked_step(Phase::Partitioning, 1, "Dropping partition functions", vec![sql], true);
        };
        match (s, t) {
            (Some(s), None) => create(self, s),
            (None, Some(t)) => drop(self, t),
            (Some(s), Some(t)) if s.range_right != t.range_right || s.data_type.sql() != t.data_type.sql() => {
                // Its schemes go with it and come back.
                let schemes: Vec<&PartitionScheme> = tgt.partition_schemes.iter().filter(|ps| self.same(&ps.function, &t.name)).collect();
                for ps in schemes {
                    self.drop_partition_scheme(ps);
                    if let Some(sp) = src.partition_schemes.iter().find(|x| self.same(&x.name, &ps.name)) {
                        self.create_partition_scheme(sp);
                    }
                }
                drop(self, t);
                create(self, s);
            }
            (Some(s), Some(t)) => {
                let statements = self.split_and_merge(s, t);
                self.ranked_step(Phase::Partitioning, 3, "Altering partition functions and schemes", statements, true);
            }
            _ => {}
        }
    }

    /// MERGE the boundaries the source lacks, then SPLIT in the new ones, each after NEXT USED
    /// on the schemes of the function.
    fn split_and_merge(&self, s: &PartitionFunction, t: &PartitionFunction) -> Vec<String> {
        let mut out: Vec<String> = t.boundaries.iter().filter(|v| !s.boundaries.contains(v)).map(|v| ddl_objects::merge_range(&s.name, v)).collect();
        for (i, v) in s.boundaries.iter().enumerate() {
            if t.boundaries.contains(v) {
                continue;
            }
            for ps in self.src.partition_schemes.iter().filter(|ps| self.same(&ps.function, &s.name)) {
                let at = if s.range_right { i + 1 } else { i };
                if let Some(fg) = ps.filegroups.get(at) {
                    out.push(ddl_objects::next_used(&ps.name, fg));
                }
            }
            out.push(ddl_objects::split_range(&s.name, v));
        }
        out
    }

    fn partition_scheme(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.partition_schemes, &d.name.name, |f| &f.name);
        let t = named(&tgt.partition_schemes, &self.target_name(d).name, |f| &f.name);
        match (s, t) {
            (Some(s), None) => self.create_partition_scheme(s),
            (None, Some(t)) => self.drop_partition_scheme(t),
            (Some(s), Some(t)) => {
                let function_altered = src.partition_functions.iter().any(|f| self.same(&f.name, &s.function))
                    && self.cmp.differences.iter().any(|x| x.object_type == ObjectType::PartitionFunction && x.selected && self.same(&x.name.name, &s.function));
                let right = src.partition_functions.iter().find(|f| self.same(&f.name, &s.function)).is_some_and(|f| f.range_right);
                let partitions = src.partition_functions.iter().find(|f| self.same(&f.name, &s.function)).map(|f| f.boundaries.len() + 1).unwrap_or(s.filegroups.len());
                if super::unbind::scheme_recreated(s, t, src, tgt) {
                    self.drop_partition_scheme(t);
                    self.create_partition_scheme(s);
                } else if !function_altered {
                    // A partition moves to another filegroup by merging it away and splitting it back.
                    let mut statements = Vec::new();
                    if let Some(f) = src.partition_functions.iter().find(|f| self.same(&f.name, &s.function)) {
                        for (i, v) in f.boundaries.iter().enumerate() {
                            let at = if right { i + 1 } else { i };
                            if s.filegroups.get(at) != t.filegroups.get(at)
                                && let Some(fg) = s.filegroups.get(at)
                            {
                                statements.push(ddl_objects::merge_range(&f.name, v));
                                statements.push(ddl_objects::next_used(&s.name, fg));
                                statements.push(ddl_objects::split_range(&f.name, v));
                            }
                        }
                    }
                    if self.opts.has(Opt::ConsiderNextFilegroupInPartitionSchemes)
                        && let Some(fg) = s.filegroups.get(partitions)
                    {
                        statements.push(ddl_objects::next_used(&s.name, fg));
                    }
                    self.ranked_step(Phase::Partitioning, 4, "Altering partition schemes", statements, true);
                }
            }
            _ => {}
        }
    }

    fn create_partition_scheme(&mut self, s: &PartitionScheme) {
        self.recreate.insert(ObjectName::new("", &s.name));
        for fg in s.filegroups.iter().filter(|f| !f.eq_ignore_ascii_case("PRIMARY")) {
            self.object_filegroups.insert(fg.clone());
        }
        let test = format!("sys.partition_schemes WHERE name = {}", ddl::sql_string(&s.name));
        let sql = self.ex.listed(&test, Check::Create, &ddl_objects::create_partition_scheme(s));
        self.ranked_step(Phase::Partitioning, 5, "Creating partition schemes", vec![sql], true);
    }

    fn drop_partition_scheme(&mut self, t: &PartitionScheme) {
        let tgt = self.tgt;
        let users: Vec<String> = tgt
            .tables
            .iter()
            .filter(|tab| !self.dropped.contains(&tab.object_name()) && !self.recreate.contains(&tab.object_name()) && self.unbound_copy(&tab.object_name()).is_none())
            .filter(|tab| {
                let on = |st: &Storage| st.partition_column.is_some() && st.data_space.as_deref().is_some_and(|ds| self.same(ds, &t.name));
                on(&tab.storage) || tab.indexes.iter().any(|ix| on(&ix.storage))
            })
            .map(|tab| tab.object_name().to_string())
            .collect();
        if !users.is_empty() {
            self.warn(owarn::partitioning_in_use(&quote(&t.name), &users.join(", ")));
        }
        let test = format!("sys.partition_schemes WHERE name = {}", ddl::sql_string(&t.name));
        let sql = self.ex.listed(&test, Check::Drop, &format!("DROP PARTITION SCHEME {}", quote(&t.name)));
        self.ranked_step(Phase::Partitioning, 0, "Dropping partition schemes", vec![sql], true);
    }

    // ---- Full-text catalogs, stoplists and search property lists ----

    fn fulltext_catalog(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.fulltext_catalogs, &d.name.name, |c| &c.name);
        let t = named(&tgt.fulltext_catalogs, &self.target_name(d).name, |c| &c.name);
        let test = |n: &str| format!("sys.fulltext_catalogs WHERE name = {}", ddl::sql_string(n));
        match (s, t) {
            (Some(s), None) => {
                let sql = self.ex.listed(&test(&s.name), Check::Create, &ddl_objects::create_fulltext_catalog(s));
                self.ranked_step(Phase::PreFullText, 9, "Creating full text catalogs", vec![sql], true);
            }
            (None, Some(t)) => {
                // Full-text indexes in it go first.
                let users: Vec<ObjectName> =
                    tgt.tables.iter().filter(|tab| tab.fulltext_index.as_ref().is_some_and(|f| self.same(&f.catalog, &t.name))).map(Table::object_name).collect();
                for tab in users {
                    self.drop_fulltext_index(&tab);
                }
                let sql = self.ex.listed(&test(&t.name), Check::Drop, &format!("DROP FULLTEXT CATALOG {}", quote(&t.name)));
                self.ranked_step(Phase::PreFullText, 8, format!("Dropping full text catalog {}", quote(&t.name)), vec![sql], false);
            }
            (Some(s), Some(t)) => {
                let name = quote(&s.name);
                let mut statements = Vec::new();
                if s.accent_sensitive != t.accent_sensitive {
                    statements.push(format!("ALTER FULLTEXT CATALOG {name}\r\nREBUILD WITH ACCENT_SENSITIVITY = {}", if s.accent_sensitive { "ON" } else { "OFF" }));
                }
                if s.is_default && !t.is_default {
                    statements.push(format!("ALTER FULLTEXT CATALOG {name}\r\nAS DEFAULT"));
                }
                if s.owner != t.owner && !self.owners_ignored() {
                    statements.push(alter_authorization("FULLTEXT CATALOG", &s.name, &s.owner));
                }
                self.ranked_step(Phase::PreFullText, 6, format!("Altering full text catalog {name}"), statements, false);
            }
            _ => {}
        }
    }

    fn stoplist(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.fulltext_stoplists, &d.name.name, |c| &c.name);
        let t = named(&tgt.fulltext_stoplists, &self.target_name(d).name, |c| &c.name);
        let test = |n: &str| format!("sys.fulltext_stoplists WHERE name = {}", ddl::sql_string(n));
        match (s, t) {
            (Some(s), None) => {
                let mut statements = ddl_objects::create_stoplist(s);
                statements[0] = self.ex.listed(&test(&s.name), Check::Create, &statements[0]);
                self.ranked_step(Phase::PreFullText, 2, "Creating full text stoplists", statements, true);
            }
            (None, Some(t)) => {
                let sql = self.ex.listed(&test(&t.name), Check::Drop, &format!("DROP FULLTEXT STOPLIST {};", quote(&t.name)));
                self.ranked_step(Phase::PreFullText, 10, format!("Dropping full text stoplist {}", quote(&t.name)), vec![sql], false);
            }
            (Some(s), Some(t)) => {
                let mut statements: Vec<String> = t.words.iter().filter(|w| !s.words.contains(w)).map(|w| ddl_objects::stopword(&s.name, w, false)).collect();
                statements.extend(s.words.iter().filter(|w| !t.words.contains(w)).map(|w| ddl_objects::stopword(&s.name, w, true)));
                if s.owner != t.owner && !self.owners_ignored() {
                    statements.push(alter_authorization("FULLTEXT STOPLIST", &s.name, &s.owner));
                }
                self.ranked_step(Phase::PreFullText, 3, format!("Altering full text stoplist {}", quote(&s.name)), statements, false);
            }
            _ => {}
        }
    }

    fn property_list(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.search_property_lists, &d.name.name, |c| &c.name);
        let t = named(&tgt.search_property_lists, &self.target_name(d).name, |c| &c.name);
        let test = |n: &str| format!("sys.registered_search_property_lists WHERE name = {}", ddl::sql_string(n));
        if s.is_some() && !self.has(Feature::SearchPropertyLists) {
            self.warn(owarn::unsupported("search property list", &quote(&d.name.name)));
        }
        match (s, t) {
            (Some(s), None) => {
                let mut statements = ddl_objects::create_property_list(s);
                statements[0] = self.ex.listed(&test(&s.name), Check::Create, &statements[0]);
                if let Some(o) = s.owner.as_ref().filter(|_| !self.owners_ignored()) {
                    statements.push(alter_authorization("SEARCH PROPERTY LIST", &s.name, &Some(o.clone())));
                }
                self.ranked_step(Phase::PreFullText, 0, "Creating search property lists", statements, true);
            }
            (None, Some(t)) => {
                let sql = self.ex.listed(&test(&t.name), Check::Drop, &format!("DROP SEARCH PROPERTY LIST {};", quote(&t.name)));
                self.ranked_step(Phase::PreFullText, 11, "Dropping search property lists", vec![sql], true);
            }
            (Some(s), Some(t)) => {
                let mut statements = Vec::new();
                for p in &t.properties {
                    if !s.properties.contains(p) {
                        statements.push(format!("ALTER SEARCH PROPERTY LIST {} DROP {};", quote(&s.name), literal(&p.name)));
                    }
                }
                for p in &s.properties {
                    if !t.properties.contains(p) {
                        statements.push(ddl_objects::add_search_property(&s.name, p));
                    }
                }
                if s.owner != t.owner && !self.owners_ignored() {
                    statements.push(alter_authorization("SEARCH PROPERTY LIST", &s.name, &s.owner));
                }
                self.ranked_step(Phase::PreFullText, 1, "Altering search property lists", statements, true);
            }
            _ => {}
        }
    }

    fn fulltext_index_test(&self, table: &ObjectName, check: Check, sql: &str) -> String {
        self.ex.listed(&format!("sys.fulltext_indexes WHERE object_id = OBJECT_ID({})", ddl::sql_string(&table.quoted())), check, sql)
    }

    /// A table that is dropped or rebuilt loses its full-text index before the transaction.
    pub(super) fn drop_fulltext_before(&mut self, t: &Table) {
        if t.fulltext_index.is_some() && !self.opts.has(Opt::IgnoreFullTextIndexing) {
            self.drop_fulltext_index(&t.object_name());
        }
    }

    fn drop_fulltext_index(&mut self, table: &ObjectName) {
        if !self.dropped_sub.insert((table.clone(), "\u{1}fulltext".into())) {
            return;
        }
        let sql = self.fulltext_index_test(table, Check::Drop, &format!("DROP FULLTEXT INDEX ON {table}"));
        self.ranked_step(Phase::PreFullText, 5, format!("Dropping full text indexing from {table}"), vec![sql], false);
    }

    fn create_fulltext_index(&mut self, table: &ObjectName, f: &FullTextIndex) {
        let filegroups = !self.opts.has(Opt::IgnoreFileGroups) && self.has(Feature::Filegroups);
        let mut statements =
            ddl_objects::create_fulltext_index(table, f, filegroups, self.opts.has(Opt::AddNoPopulation), self.has(Feature::SearchPropertyLists), self.opts.has(Opt::InlineFulltextFields));
        statements[0] = self.fulltext_index_test(table, Check::Create, &statements[0]);
        self.objects.fulltext_columns |= !f.columns.is_empty();
        if self.opts.has(Opt::NoTransactions) {
            self.ranked(Phase::CreateObjects, format!("Adding full text indexing to {table}"), table, statements);
        } else {
            self.step(Phase::PostFullText, format!("Adding full text indexing to {table}"), statements);
        }
    }

    // ---- Table parts: statistics, full-text index, edge constraints, sensitivity ----

    pub(super) fn drop_statistics(&mut self, t: &Table, st: &Statistic) {
        let name = t.object_name();
        if !self.dropped_sub.insert((name.clone(), self.key(&format!("\u{1}stats {}", st.name)))) {
            return;
        }
        let sql = self.ex.index(&name, &st.name, Check::Drop, &ddl_objects::drop_statistics(&name, &st.name));
        self.push(Step { rank: 0, ..Step::new(Phase::DropIndexes, format!("Dropping statistics [{}] from {name}", st.name), vec![sql]) });
    }

    fn create_statistics(&mut self, name: &ObjectName, st: &Statistic, view: bool) {
        self.warn(owarn::statistics_sample(&name.to_string(), &st.name));
        if st.incremental && !self.opts.has(Opt::IgnoreStatisticsIncremental) {
            if !self.has(Feature::IncrementalStatistics) {
                self.warn(warn::target::incremental_statistics(&name.to_string(), &st.name));
            } else if self.opts.has(Opt::IgnoreFileGroups) {
                self.warn(warn::incremental_with_ignored_filegroups(&name.to_string(), &st.name));
            }
        }
        let incremental = self.has(Feature::IncrementalStatistics) && !view && !self.opts.has(Opt::IgnoreStatisticsIncremental);
        let sql = self.ex.index(name, &st.name, Check::Create, &ddl_objects::create_statistics(name, st, incremental));
        self.ranked(Phase::CreateObjects, format!("Creating statistics [{}] on {name}", st.name), name, vec![sql]);
    }

    /// Statistics, the full-text index, edge constraints and sensitivity classification of a
    /// table that is created or rebuilt (`t` None) or altered in place.
    pub(super) fn table_parts(&mut self, s: &'a Table, t: Option<&'a Table>) {
        let name = s.object_name();
        let o = self.opts;
        if !o.has(Opt::IgnoreStatistics) {
            for st in &s.statistics {
                let existing = t.and_then(|t| t.statistics.iter().find(|x| self.same(&x.name, &st.name)));
                let released = self.dropped_sub.contains(&(name.clone(), self.key(&format!("\u{1}stats {}", st.name))));
                let same = existing.is_some_and(|x| {
                    x.columns.iter().map(|c| self.key(c)).eq(st.columns.iter().map(|c| self.key(c)))
                        && x.filter == st.filter
                        && (o.has(Opt::IgnoreStatisticsNorecompute) || x.no_recompute == st.no_recompute)
                        && (o.has(Opt::IgnoreStatisticsIncremental) || x.incremental == st.incremental)
                });
                if same && !released {
                    continue;
                }
                if let (Some(t), Some(x)) = (t, existing)
                    && !released
                {
                    self.drop_statistics(t, x);
                }
                self.create_statistics(&name, st, false);
            }
            if let Some(t) = t {
                let gone: Vec<&Statistic> = t.statistics.iter().filter(|x| !s.statistics.iter().any(|st| self.same(&st.name, &x.name))).collect();
                for x in gone {
                    self.drop_statistics(t, x);
                }
            }
        }
        if !o.has(Opt::IgnoreFullTextIndexing) {
            self.table_fulltext(&name, s.fulltext_index.as_ref(), t.and_then(|t| t.fulltext_index.as_ref()), t.is_none());
        }
        // Edge constraints.
        let ignore_nocheck = o.has(Opt::IgnoreNocheckAndWithNocheck) || o.has(Opt::IgnoreWithNocheck);
        for e in &s.edge_constraints {
            let existing = t.and_then(|t| t.edge_constraints.iter().find(|x| self.same(&x.name, &e.name)));
            if existing.is_some_and(|x| x.connections == e.connections && x.on_delete_cascade == e.on_delete_cascade && (ignore_nocheck || x.not_trusted == e.not_trusted)) {
                continue;
            }
            if !self.has(Feature::EdgeConstraints) {
                self.warn(owarn::edge_constraint_unsupported(&name.to_string(), &e.name));
                continue;
            }
            if let (Some(t), Some(x)) = (t, existing) {
                self.drop_check(&t.object_name(), &x.name);
            }
            let sql = self.ex.listed(
                &format!("sys.edge_constraints WHERE object_id = OBJECT_ID({}, 'EC') AND parent_object_id = OBJECT_ID({}, 'U')", ddl::sql_string(&ObjectName::new(&s.schema, &e.name).quoted()), ddl::sql_string(&name.quoted())),
                Check::Create,
                &ddl_objects::add_edge_constraint(&name, e, e.not_trusted && !ignore_nocheck),
            );
            self.group(Phase::CreateChecks, format!("Adding constraints to {name}"), vec![sql]);
            if e.disabled && !o.has(Opt::IgnoreNocheckAndWithNocheck) {
                self.group(Phase::CreateChecks, format!("Disabling constraints on {name}"), vec![format!("ALTER TABLE {name} NOCHECK CONSTRAINT {}", quote(&e.name))]);
            }
        }
        if let Some(t) = t {
            let gone: Vec<&EdgeConstraint> = t.edge_constraints.iter().filter(|x| !s.edge_constraints.iter().any(|e| self.same(&e.name, &x.name))).collect();
            for x in gone {
                self.drop_check(&t.object_name(), &x.name);
            }
        }
        if !o.has(Opt::IgnoreSensitivityClassification) {
            for c in &s.columns {
                let old = t.and_then(|t| t.columns.iter().find(|x| self.same(&x.name, &c.name)));
                let before = old.and_then(|x| x.sensitivity.as_ref());
                if before == c.sensitivity.as_ref() {
                    continue;
                }
                match &c.sensitivity {
                    Some(x) => self.add_sensitivity(&name, &c.name, x),
                    None => self.drop_sensitivity(&name, &c.name, before),
                }
            }
        }
    }

    fn table_fulltext(&mut self, name: &ObjectName, s: Option<&'a FullTextIndex>, t: Option<&'a FullTextIndex>, fresh: bool) {
        match (s, t) {
            (Some(s), None) => self.create_fulltext_index(name, s),
            (None, Some(_)) => self.drop_fulltext_index(name),
            (Some(s), Some(_)) if fresh => self.create_fulltext_index(name, s),
            (Some(s), Some(t)) => {
                let o = self.opts;
                let rebuild = !self.same(&s.catalog, &t.catalog)
                    || !self.same(&s.key_index, &t.key_index)
                    || (!o.has(Opt::IgnoreFileGroups) && s.filegroup != t.filegroup)
                    || self.dropped_sub.contains(&(name.clone(), self.key(&t.key_index)));
                if rebuild {
                    self.drop_fulltext_index(name);
                    self.create_fulltext_index(name, s);
                    return;
                }
                let table = name.quoted();
                let mut alters = Vec::new();
                if s.change_tracking != t.change_tracking {
                    alters.push(format!("ALTER FULLTEXT INDEX ON {table} SET CHANGE_TRACKING {}", s.change_tracking));
                }
                if !self.same(&s.stoplist, &t.stoplist) {
                    let to = if matches!(s.stoplist.as_str(), "SYSTEM" | "OFF") { s.stoplist.clone() } else { quote(&s.stoplist) };
                    alters.push(format!("ALTER FULLTEXT INDEX ON {table} SET STOPLIST {to}"));
                }
                if s.property_list != t.property_list && self.has(Feature::SearchPropertyLists) {
                    alters.push(format!("ALTER FULLTEXT INDEX ON {table} SET SEARCH PROPERTY LIST {}", s.property_list.as_deref().map(quote).unwrap_or_else(|| "OFF".into())));
                }
                self.ranked_step(Phase::PreFullText, 7, format!("Altering full text indexing on {name}"), alters, false);
                // Columns: dropped ones before the transaction, changed and new ones after.
                let column_test = |c: &str| {
                    format!(
                        "sys.fulltext_index_columns JOIN sys.columns ON fulltext_index_columns.object_id = sys.columns.object_id AND fulltext_index_columns.column_id = sys.columns.column_id WHERE sys.columns.object_id = OBJECT_ID({}) AND sys.columns.name = {}",
                        ddl::sql_string(&table),
                        ddl::sql_string(c)
                    )
                };
                let mut adds = Vec::new();
                for c in &t.columns {
                    let keep = s.columns.iter().find(|x| self.same(&x.name, &c.name));
                    if keep != Some(c) {
                        let sql = self.ex.listed(&column_test(&c.name), Check::Drop, &format!("ALTER FULLTEXT INDEX ON {table} DROP ({})", quote(&c.name)));
                        self.ranked_step(Phase::PreFullText, 4, format!("Dropping full text indexing from column [{}] on {name}", c.name), vec![sql], false);
                    }
                }
                for c in &s.columns {
                    if t.columns.iter().find(|x| self.same(&x.name, &c.name)) != Some(c) {
                        adds.push(self.ex.listed(&column_test(&c.name), Check::Create, &format!("ALTER FULLTEXT INDEX ON {table} ADD ({})", ddl_objects::fulltext_column(c))));
                    }
                }
                if !adds.is_empty() {
                    self.objects.fulltext_columns = true;
                    let phase = if self.opts.has(Opt::NoTransactions) { Phase::CreateObjects } else { Phase::PostFullText };
                    self.group(phase, "Adding full text indexing to columns", adds);
                }
            }
            _ => {}
        }
    }

    fn add_sensitivity(&mut self, table: &ObjectName, column: &str, x: &Sensitivity) {
        let label = format!("{table}.{}", quote(column));
        if x.label.is_none() && x.label_id.is_some() {
            self.warn(owarn::label_without_name(&label, false));
        }
        if x.information_type.is_none() && x.information_type_id.is_some() {
            self.warn(owarn::label_without_name(&label, true));
        }
        if self.has(Feature::SensitivitySyntax) {
            self.step(Phase::AddSensitivity, format!("Adding sensitivity classification to {label}"), vec![ddl_objects::add_sensitivity(table, column, x)]);
            return;
        }
        // Older targets keep classification in extended properties.
        let levels = format!("'SCHEMA', {}, 'TABLE', {}, 'COLUMN', {}", literal(&table.schema), literal(&table.name), literal(column));
        let mut statements = Vec::new();
        for (prop, value) in [
            ("sys_sensitivity_label_name", &x.label),
            ("sys_sensitivity_label_id", &x.label_id),
            ("sys_information_type_name", &x.information_type),
            ("sys_information_type_id", &x.information_type_id),
        ] {
            if let Some(v) = value {
                let p = ExtendedProperty { name: prop.into(), value: Some(v.clone()), ..Default::default() };
                statements.push(ddl::add_xprop(&p, &levels));
            }
        }
        self.push(Step { group: true, try_catch: true, ..Step::new(Phase::ExtendedProperties, "Creating extended properties", statements) });
    }

    fn drop_sensitivity(&mut self, table: &ObjectName, column: &str, before: Option<&Sensitivity>) {
        let label = format!("{table}.{}", quote(column));
        if self.has(Feature::SensitivitySyntax) {
            self.step(Phase::DropSensitivity, format!("Dropping sensitivity classification from {label}"), vec![ddl_objects::drop_sensitivity(table, column)]);
            return;
        }
        let Some(x) = before else { return };
        let levels = format!("'SCHEMA', {}, 'TABLE', {}, 'COLUMN', {}", literal(&table.schema), literal(&table.name), literal(column));
        let mut statements = Vec::new();
        for (prop, value) in [
            ("sys_sensitivity_label_name", &x.label),
            ("sys_sensitivity_label_id", &x.label_id),
            ("sys_information_type_name", &x.information_type),
            ("sys_information_type_id", &x.information_type_id),
        ] {
            if value.is_some() {
                let p = ExtendedProperty { name: prop.into(), ..Default::default() };
                statements.push(ddl::drop_xprop(&p, &levels));
            }
        }
        self.push(Step { group: true, try_catch: true, ..Step::new(Phase::DropExtendedProperties, "Dropping extended properties", statements) });
    }

    // ---- Alias types used by columns ----

    /// The columns of tables that stay go to the type's base type before the type is dropped,
    /// and back to the type once it is created again.
    pub(super) fn unbind_type(&mut self, t: &'a UserType, s: Option<&'a UserType>) {
        let tgt = self.tgt;
        let Some(base) = &t.base_type else { return };
        for tab in tgt.tables.iter() {
            let name = tab.object_name();
            if self.dropped.contains(&name) || self.recreate.contains(&name) {
                continue;
            }
            let users: Vec<&Column> = tab.columns.iter().filter(|c| c.data_type.schema.as_deref() == Some(&t.schema) && c.data_type.name == t.name && c.computed.is_none()).collect();
            if users.is_empty() {
                continue;
            }
            for c in &users {
                self.release_column(tab, &c.name, false);
                let null = if c.nullable { "NULL" } else { "NOT NULL" };
                let mut statements = Vec::new();
                if c.rowguidcol {
                    statements.push(format!("ALTER TABLE {name} ALTER COLUMN {} DROP ROWGUIDCOL", quote(&c.name)));
                }
                statements.push(self.ex.columns(&name, &[&c.name], Check::Drop, &format!("ALTER TABLE {name} ALTER COLUMN {} {} {null}", quote(&c.name), type_sql(base))));
                self.group(Phase::UnbindTypes, "Unbinding types from columns", statements);
                if s.is_some() {
                    let mut col = (*c).clone();
                    col.identity = None;
                    col.default = None;
                    let mut statements = vec![format!("ALTER TABLE {name} ALTER COLUMN {}", ddl::column_definition_with(&col, false, &self.r, tab.memory_optimized))];
                    if c.rowguidcol {
                        statements.push(format!("ALTER TABLE {name} ALTER COLUMN {} ADD ROWGUIDCOL", quote(&c.name)));
                    }
                    self.group(Phase::RebindTypes, "Rebinding types to columns", statements);
                }
            }
            // What was dropped with the columns comes back as it was.
            self.restore_released(tab);
        }
    }

    /// Indexes, constraints and statistics of a table that is not deployed itself, dropped
    /// because a column changed type, come back from the target's definitions.
    pub(super) fn restore_released(&mut self, t: &'a Table) {
        if self.cmp.differences.iter().any(|d| d.object_type == ObjectType::Table && d.selected && self.target_name(d) == t.object_name()) {
            return;
        }
        self.sub_objects(t, t);
        let name = t.object_name();
        for c in &t.columns {
            if let Some(df) = &c.default
                && self.dropped_sub.contains(&(name.clone(), self.key(&df.name)))
            {
                let sql = self.ex.default_constraint(&name, &c.name, &df.name, Check::Create, &ddl::add_default(&name.quoted(), &c.name, df));
                self.step(Phase::CreateDefaults, format!("Altering {name}"), vec![sql]);
            }
        }
        let only: Vec<&str> = t.columns.iter().filter(|c| self.unbound.iter().any(|(tab, col, _)| *tab == name && *col == self.key(&c.name))).map(|c| c.name.as_str()).collect();
        if !only.is_empty() {
            self.bind_columns(t, Some(&only), true);
        }
        self.table_parts(t, Some(t));
    }

    // ---- System-versioned tables ----

    pub(super) fn retention_warning(&mut self, s: &Table) {
        if s.temporal.as_ref().is_some_and(|x| x.retention.is_some()) && !self.has(Feature::TemporalRetention) {
            self.warn(warn::target::retention(&s.object_name().to_string()));
        }
    }

    fn versioning_off(&mut self, t: &Table) {
        let name = t.object_name();
        self.step(Phase::DisableChangeTracking, "", vec![format!("ALTER TABLE {name} SET (SYSTEM_VERSIONING = OFF)")]);
    }

    /// What an in-place alter does to a table's versioning: whether versioning goes off first,
    /// and the PERIOD statements before and after the column changes.
    pub(super) fn versioning_change(&mut self, s: &'a Table, t: &'a Table) -> (bool, (Vec<String>, Vec<String>)) {
        let name = t.object_name();
        let (mut before, mut after) = (Vec::new(), Vec::new());
        if !self.has(Feature::Temporal) {
            return (false, (before, after));
        }
        match (&s.temporal, &t.temporal) {
            (None, Some(_)) => {
                self.versioning_off(t);
                if t.columns.iter().any(|c| c.generated_always.is_some()) {
                    before.push(format!("ALTER TABLE {name} DROP PERIOD FOR SYSTEM_TIME"));
                }
                (true, (before, after))
            }
            (Some(x), None) => {
                // Period columns the table has already become the period.
                let kept = |c: &str| t.columns.iter().any(|tc| self.same(&tc.name, c));
                if kept(&x.period_start) && kept(&x.period_end) {
                    after.push(format!("ALTER TABLE {name} ADD PERIOD FOR SYSTEM_TIME ({}, {})", quote(&x.period_start), quote(&x.period_end)));
                }
                for c in s.columns.iter().filter(|c| c.hidden) {
                    if t.columns.iter().any(|tc| self.same(&tc.name, &c.name) && !tc.hidden) {
                        after.push(format!("ALTER TABLE {name} ALTER COLUMN {} ADD HIDDEN", quote(&c.name)));
                    }
                }
                (false, (before, after))
            }
            (Some(x), Some(y)) => {
                let history_moved = self.key(&x.history_table.to_string()) != self.key(&y.history_table.to_string());
                let pk_dropped = t.indexes.iter().any(|i| i.primary_key && self.dropped_sub.contains(&(name.clone(), self.key(&i.name))));
                for c in s.columns.iter() {
                    if let Some(tc) = t.columns.iter().find(|tc| self.same(&tc.name, &c.name))
                        && c.hidden != tc.hidden
                    {
                        after.push(format!("ALTER TABLE {name} ALTER COLUMN {} {} HIDDEN", quote(&c.name), if c.hidden { "ADD" } else { "DROP" }));
                    }
                }
                if history_moved || pk_dropped {
                    self.versioning_off(t);
                    return (true, (before, after));
                }
                (false, (before, after))
            }
            (None, None) => (false, (before, after)),
        }
    }

    /// SET (SYSTEM_VERSIONING = ON …) after an in-place alter, and the old history table when
    /// versioning ends or moves to another history table.
    pub(super) fn versioning_on(&mut self, s: &'a Table, t: &'a Table, was_switched_off: bool) {
        if !self.has(Feature::Temporal) {
            if s.temporal.is_some() && t.temporal.is_none() {
                self.warn(warn::target::temporal(&s.object_name().to_string()));
            }
            return;
        }
        let name = t.object_name();
        match (&s.temporal, &t.temporal) {
            (Some(x), None) => {
                self.retention_warning(s);
                self.create_history(s);
                self.step(Phase::Versioning, "", vec![format!("ALTER TABLE {name} SET ({})", ddl::system_versioning(x, &self.r))]);
            }
            (None, Some(y)) => self.drop_old_history(&y.history_table),
            (Some(x), Some(y)) => {
                if was_switched_off {
                    self.create_history(s);
                    self.step(Phase::Versioning, "", vec![format!("ALTER TABLE {name} SET ({})", ddl::system_versioning(x, &self.r))]);
                    if self.key(&x.history_table.to_string()) != self.key(&y.history_table.to_string()) {
                        self.drop_old_history(&y.history_table);
                    }
                } else if x.retention != y.retention && self.has(Feature::TemporalRetention) {
                    if retention_days(&x.retention).unwrap_or(i64::MAX) < retention_days(&y.retention).unwrap_or(i64::MAX) {
                        self.warn(owarn::retention_shortened(&name.to_string()));
                    }
                    let period = x.retention.clone().unwrap_or_else(|| "INFINITE".into());
                    self.step(Phase::Versioning, "", vec![format!("ALTER TABLE {name} SET (SYSTEM_VERSIONING = ON (HISTORY_RETENTION_PERIOD = {period}))")]);
                }
            }
            (None, None) => {}
        }
    }

    /// The source's history table of a table that becomes system-versioned (or changes history
    /// table), when the target does not have it: SQL Server would make one without its indexes.
    pub(super) fn create_history(&mut self, s: &'a Table) {
        let src = self.src;
        let Some(x) = s.temporal.as_ref().filter(|_| self.has(Feature::Temporal)) else { return };
        let Some(h) = src.table(&x.history_table.schema, &x.history_table.name) else { return };
        if self.tgt.table(&h.schema, &h.name).is_some() {
            return;
        }
        let name = h.object_name();
        let mut statements = vec![ddl::create_table_with(h, None, false, &self.r)];
        for ix in &h.indexes {
            statements.extend(ddl::create_index_with(&name.quoted(), ix, &self.r, &Host::of_table(h)));
        }
        self.recreate.insert(name.clone());
        self.module_step(Phase::CreateObjects, format!("Creating {name}"), statements, &ModuleSettings::default());
    }

    fn drop_old_history(&mut self, h: &ObjectName) {
        if self.src.table(&h.schema, &h.name).is_some() || !self.dropped.insert(h.clone()) {
            return;
        }
        self.warn(warn::table_drop(&h.to_string()));
        self.step(Phase::Versioning, format!("Dropping {h}"), vec![format!("DROP TABLE {h}")]);
    }

    /// A rebuild of a table that is or becomes system-versioned: versioning goes off, the main
    /// table (and its history table) are rebuilt, and versioning comes back.
    pub(super) fn rebuild_temporal(&mut self, s: &'a Table, t: &'a Table, pairs: &[(Option<&'a Column>, Option<&'a Column>)]) {
        let (src, tgt) = (self.src, self.tgt);
        if t.temporal.is_some() {
            self.versioning_off(t);
        }
        self.rebuild_table(s, t, pairs);
        let name = s.object_name();
        match (&s.temporal, &t.temporal) {
            (Some(x), Some(y)) => {
                let hs = src.table(&x.history_table.schema, &x.history_table.name);
                let ht = tgt.table(&y.history_table.schema, &y.history_table.name);
                if let (Some(hs), Some(ht)) = (hs, ht)
                    && self.key(&x.history_table.to_string()) == self.key(&y.history_table.to_string())
                {
                    let hp = self.column_pairs(hs, ht);
                    self.rebuild_table(hs, ht, &hp);
                } else {
                    self.drop_old_history(&y.history_table);
                }
            }
            (None, Some(y)) => self.drop_old_history(&y.history_table),
            _ => {}
        }
        if let Some(x) = s.temporal.as_ref().filter(|_| self.has(Feature::Temporal)) {
            self.retention_warning(s);
            self.create_history(s);
            self.step(Phase::Versioning, "", vec![format!("ALTER TABLE {name} SET ({})", ddl::system_versioning(x, &self.r))]);
        }
    }

    // ---- Service Broker ----

    fn broker_unsupported(&mut self, d: &Difference) -> bool {
        if self.has(Feature::ServiceBroker) || d.kind == DiffKind::OnlyIn2 {
            return false;
        }
        let kind = match d.object_type {
            ObjectType::MessageType => "message type",
            ObjectType::Contract => "contract",
            ObjectType::Queue => "queue",
            ObjectType::Service => "service",
            ObjectType::ServiceBinding => "service binding",
            _ => "event notification",
        };
        let label = if d.object_type.schema_scoped() { d.name.to_string() } else { quote(&d.name.name) };
        self.warn(owarn::unsupported(kind, &label));
        // Bindings are still created, as SQL Compare does.
        d.object_type != ObjectType::ServiceBinding
    }

    fn broker(&mut self, d: &Difference) {
        if self.broker_unsupported(d) {
            return;
        }
        let key = d.key();
        let tkey = d.target_key();
        let (src, tgt) = (self.src, self.tgt);
        self.objects.done.insert(key.clone());
        let rebuild_forced = self.objects.rebuild.contains(&tkey);
        let n = &d.name.name;
        let tn = self.target_name(d);
        let label = if d.object_type.schema_scoped() { d.name.to_string() } else { quote(n) };
        match d.object_type {
            ObjectType::MessageType => {
                let (s, t) = (named(&src.message_types, n, |m| &m.name), named(&tgt.message_types, &tn.name, |m| &m.name));
                match (s, t) {
                    (Some(s), Some(t)) if !rebuild_forced => {
                        let mut statements = Vec::new();
                        if s.validation != t.validation || s.xml_collection != t.xml_collection {
                            statements.push(ddl_objects::alter_message_type(s));
                        }
                        if s.owner != t.owner && !self.owners_ignored() {
                            statements.push(alter_authorization("MESSAGE TYPE", n, &s.owner));
                        }
                        self.core(Phase::CreateObjects, format!("Altering {label}"), key, statements);
                    }
                    (s, t) => {
                        if t.is_some() {
                            self.drop_broker(ObjectType::MessageType, &tkey);
                        }
                        if s.is_some() {
                            self.create_broker(ObjectType::MessageType, &key);
                        }
                    }
                }
            }
            ObjectType::Contract => {
                let (s, t) = (named(&src.contracts, n, |m| &m.name), named(&tgt.contracts, &tn.name, |m| &m.name));
                match (s, t) {
                    (Some(s), Some(t)) if !rebuild_forced && s.messages.len() == t.messages.len() && s.messages.iter().all(|m| t.messages.contains(m)) => {
                        if s.owner != t.owner && !self.owners_ignored() {
                            self.core(Phase::CreateObjects, format!("Altering {label}"), key, vec![alter_authorization("CONTRACT", n, &s.owner)]);
                        }
                    }
                    (s, t) => {
                        if t.is_some() {
                            self.drop_broker(ObjectType::Contract, &tkey);
                        }
                        if s.is_some() {
                            self.create_broker(ObjectType::Contract, &key);
                        }
                    }
                }
            }
            ObjectType::Queue => {
                let (s, t) = (scoped(&src.queues, &d.name, |q| (&q.schema, &q.name)), scoped(&tgt.queues, &tn, |q| (&q.schema, &q.name)));
                let moved = |s: &Queue, t: &Queue| !self.opts.has(Opt::IgnoreFileGroups) && s.filegroup != t.filegroup;
                match (s, t) {
                    (Some(s), Some(t)) if !rebuild_forced && !moved(s, t) => {
                        let mut statements = Vec::new();
                        let ignore_en = self.opts.has(Opt::IgnoreQueueEventNotifications);
                        if !ignore_en {
                            for e in t.event_notifications.iter().filter(|e| s.event_notifications.iter().find(|x| self.same(&x.name, &e.name)) != Some(e)) {
                                statements.push(ddl_objects::drop_event_notification(e));
                            }
                        }
                        statements.extend(ddl_objects::alter_queue(s, t, self.has(Feature::PoisonMessageHandling)));
                        if s.owner != t.owner && !self.opts.has(Opt::IgnoreSchemaObjectAuthorization) && !self.owners_ignored() {
                            statements.push(match &s.owner {
                                Some(o) => format!("ALTER AUTHORIZATION ON {} TO {}", d.name, quote(o)),
                                None => format!("ALTER AUTHORIZATION ON {} TO SCHEMA OWNER", d.name),
                            });
                        }
                        if !ignore_en {
                            for e in s.event_notifications.iter().filter(|e| t.event_notifications.iter().find(|x| self.same(&x.name, &e.name)) != Some(e)) {
                                statements.push(ddl_objects::create_event_notification(e));
                            }
                        }
                        self.core(Phase::CreateObjects, format!("Altering {label}"), key, statements);
                    }
                    (s, t) => {
                        if t.is_some() {
                            self.drop_broker(ObjectType::Queue, &tkey);
                        }
                        if s.is_some() {
                            self.create_broker(ObjectType::Queue, &key);
                        }
                    }
                }
            }
            ObjectType::Service => {
                let (s, t) = (named(&src.services, n, |m| &m.name), named(&tgt.services, &tn.name, |m| &m.name));
                match (s, t) {
                    (Some(s), Some(t)) if !rebuild_forced => {
                        let mut statements = ddl_objects::alter_service(s, t);
                        if s.owner != t.owner && !self.owners_ignored() {
                            statements.push(alter_authorization("SERVICE", n, &s.owner));
                        }
                        self.core(Phase::CreateObjects, format!("Altering {label}"), key, statements);
                    }
                    (s, t) => {
                        if t.is_some() {
                            self.drop_broker(ObjectType::Service, &tkey);
                        }
                        if s.is_some() {
                            self.create_broker(ObjectType::Service, &key);
                        }
                    }
                }
            }
            ObjectType::ServiceBinding => {
                let (s, t) = (named(&src.service_bindings, n, |m| &m.name), named(&tgt.service_bindings, &tn.name, |m| &m.name));
                match (s, t) {
                    (Some(s), Some(t)) if s.remote_service == t.remote_service => {
                        let mut statements: Vec<String> = ddl_objects::alter_service_binding(s, t).into_iter().collect();
                        if s.owner != t.owner && !self.owners_ignored() {
                            statements.push(alter_authorization("REMOTE SERVICE BINDING", n, &s.owner));
                        }
                        self.core(Phase::CreateObjects, format!("Altering {label}"), key, statements);
                    }
                    (s, t) => {
                        if t.is_some() {
                            self.drop_broker(ObjectType::ServiceBinding, &tkey);
                        }
                        if s.is_some() {
                            self.create_broker(ObjectType::ServiceBinding, &key);
                        }
                    }
                }
            }
            _ => {
                if named(&tgt.event_notifications, &tn.name, |m| &m.name).is_some() {
                    self.drop_broker(ObjectType::EventNotification, &tkey);
                }
                if named(&src.event_notifications, n, |m| &m.name).is_some() {
                    self.create_broker(ObjectType::EventNotification, &key);
                }
            }
        }
    }

    /// The target objects that use a Service Broker object.
    fn broker_users(&self, ty: ObjectType, key: &ObjectName) -> Vec<(ObjectType, ObjectName)> {
        let tgt = self.tgt;
        let n = &key.name;
        match ty {
            ObjectType::MessageType => tgt
                .contracts
                .iter()
                .filter(|c| c.messages.iter().any(|m| self.same(&m.message_type, n)))
                .map(|c| (ObjectType::Contract, ObjectName::new(KEY_CONTRACT, &c.name)))
                .collect(),
            ObjectType::Contract => tgt
                .services
                .iter()
                .filter(|s| s.contracts.iter().any(|c| self.same(c, n)))
                .map(|s| (ObjectType::Service, ObjectName::new(KEY_SERVICE, &s.name)))
                .collect(),
            ObjectType::Queue => tgt
                .services
                .iter()
                .filter(|s| self.same(&s.queue.schema, &key.schema) && self.same(&s.queue.name, &key.name))
                .map(|s| (ObjectType::Service, ObjectName::new(KEY_SERVICE, &s.name)))
                .collect(),
            ObjectType::Service => tgt
                .event_notifications
                .iter()
                .filter(|e| self.same(&e.service, n))
                .map(|e| (ObjectType::EventNotification, ObjectName::new(KEY_EVENT_NOTIFICATION, &e.name)))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn drop_broker(&mut self, ty: ObjectType, key: &ObjectName) {
        if !self.dropped.insert(key.clone()) {
            return;
        }
        for (uty, user) in self.broker_users(ty, key) {
            self.objects.rebuild.insert(user.clone());
            self.drop_broker(uty, &user);
        }
        let tgt = self.tgt;
        let n = &key.name;
        let lit = ddl::sql_string(n);
        let (label, sql) = match ty {
            ObjectType::MessageType => (quote(n), self.ex.listed(&format!("sys.service_message_types WHERE name = {lit}"), Check::Drop, &format!("DROP MESSAGE TYPE {}", quote(n)))),
            ObjectType::Contract => (quote(n), self.ex.listed(&format!("sys.service_contracts WHERE name = {lit}"), Check::Drop, &format!("DROP CONTRACT {}", quote(n)))),
            ObjectType::Queue => (key.to_string(), self.ex.object(key, "SQ", Check::Drop, &format!("DROP QUEUE {key}"))),
            ObjectType::Service => (quote(n), self.ex.listed(&format!("sys.services WHERE name = {lit}"), Check::Drop, &format!("DROP SERVICE {}", quote(n)))),
            ObjectType::ServiceBinding => {
                (quote(n), self.ex.listed(&format!("sys.remote_service_bindings WHERE name = {lit}"), Check::Drop, &format!("DROP REMOTE SERVICE BINDING {}", quote(n))))
            }
            _ => {
                let Some(e) = named(&tgt.event_notifications, n, |e| &e.name) else { return };
                (quote(n), self.ex.listed(&format!("sys.event_notifications WHERE name = {lit} AND parent_id = 0"), Check::Drop, &ddl_objects::drop_event_notification(e)))
            }
        };
        self.core(Phase::DropObjects, format!("Dropping {label}"), key.clone(), vec![sql]);
    }

    fn create_broker(&mut self, ty: ObjectType, key: &ObjectName) {
        if !self.recreate.insert(key.clone()) {
            return;
        }
        let src = self.src;
        let n = &key.name;
        let lit = ddl::sql_string(n);
        let filegroups = !self.opts.has(Opt::IgnoreFileGroups) && self.has(Feature::Filegroups);
        let (label, statements) = match ty {
            ObjectType::MessageType => {
                let Some(m) = named(&src.message_types, n, |m| &m.name) else { return };
                (quote(n), vec![self.ex.listed(&format!("sys.service_message_types WHERE name = {lit}"), Check::Create, &ddl_objects::create_message_type(m))])
            }
            ObjectType::Contract => {
                let Some(c) = named(&src.contracts, n, |m| &m.name) else { return };
                (quote(n), vec![self.ex.listed(&format!("sys.service_contracts WHERE name = {lit}"), Check::Create, &ddl_objects::create_contract(c))])
            }
            ObjectType::Queue => {
                let Some(q) = scoped(&src.queues, key, |q| (&q.schema, &q.name)) else { return };
                if let Some(fg) = q.filegroup.as_ref().filter(|f| filegroups && !f.eq_ignore_ascii_case("PRIMARY")) {
                    self.object_filegroups.insert(fg.clone());
                }
                let mut statements = vec![self.ex.object(key, "SQ", Check::Create, &ddl_objects::create_queue(q, self.has(Feature::PoisonMessageHandling), filegroups))];
                if let Some(o) = q.owner.as_ref().filter(|_| !self.opts.has(Opt::IgnoreSchemaObjectAuthorization) && !self.owners_ignored()) {
                    statements.push(format!("ALTER AUTHORIZATION ON {key} TO {}", quote(o)));
                }
                if !self.opts.has(Opt::IgnoreQueueEventNotifications) {
                    statements.extend(q.event_notifications.iter().map(ddl_objects::create_event_notification));
                }
                (key.to_string(), statements)
            }
            ObjectType::Service => {
                let Some(s) = named(&src.services, n, |m| &m.name) else { return };
                (quote(n), vec![self.ex.listed(&format!("sys.services WHERE name = {lit}"), Check::Create, &ddl_objects::create_service(s))])
            }
            ObjectType::ServiceBinding => {
                let Some(b) = named(&src.service_bindings, n, |m| &m.name) else { return };
                let test = format!("sys.remote_service_bindings WHERE name = {lit} AND remote_service_name = {}", ddl::sql_string(&b.remote_service));
                (quote(n), vec![self.ex.listed(&test, Check::Create, &ddl_objects::create_service_binding(b))])
            }
            _ => {
                let Some(e) = named(&src.event_notifications, n, |m| &m.name) else { return };
                (quote(n), vec![self.ex.listed(&format!("sys.event_notifications WHERE name = {lit} AND parent_id = 0"), Check::Create, &ddl_objects::create_event_notification(e))])
            }
        };
        // Grants on a re-created object are made again.
        let name = ObjectName::new(if ty.schema_scoped() { &key.schema } else { "" }, n);
        self.recreate.insert(name.clone());
        self.core(Phase::CreateObjects, format!("Creating {label}"), key.clone(), statements);
        self.regrant(ty, &name);
    }

    /// The source's permissions on an object re-created only because something it uses is
    /// (selected objects get theirs with the other permissions).
    pub(super) fn regrant(&mut self, ty: ObjectType, name: &ObjectName) {
        if self.cmp.differences.iter().any(|d| d.object_type == ty && d.selected && d.kind != DiffKind::Equal && d.name == *name) {
            return;
        }
        let (class, schema, object) = match securable_class(ty) {
            Some(class) => (class, None, name.name.as_str()),
            None => ("OBJECT", Some(name.schema.as_str()), name.name.as_str()),
        };
        let grants: Vec<String> = self
            .src
            .permissions
            .iter()
            .filter(|p| p.class == class && p.schema.as_deref() == schema && p.object.as_deref() == Some(object) && !self.opts.has(Opt::IgnorePermissions))
            .map(Permission::grant_sql)
            .collect();
        let label = if schema.is_some() { name.to_string() } else { format!("{class}:: {}", quote(object)) };
        self.step(Phase::Permissions, format!("Altering permissions on {label}"), grants);
        if let Some(xclass) = xprop_class(ty).filter(|_| !self.opts.has(Opt::IgnoreExtendedProperties)) {
            let props: Vec<&ExtendedProperty> =
                self.src.extended_properties.iter().filter(|p| p.class == xclass && p.schema.as_deref() == schema && p.object.as_deref() == Some(object)).collect();
            for p in props {
                self.add_xprop(p, &ddl::xprop_levels(p, |s| s.to_string()));
            }
        }
    }

    /// A Service Broker object that is not deployed itself but uses one that is re-created.
    fn broker_rebuild(&mut self, key: &ObjectName) {
        self.objects.done.insert(key.clone());
        let ty = match key.schema.as_str() {
            KEY_MESSAGE_TYPE => ObjectType::MessageType,
            KEY_CONTRACT => ObjectType::Contract,
            KEY_SERVICE => ObjectType::Service,
            KEY_EVENT_NOTIFICATION => ObjectType::EventNotification,
            _ => return,
        };
        self.drop_broker(ty, key);
        self.create_broker(ty, key);
    }

    fn route(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let s = named(&src.routes, &d.name.name, |r| &r.name);
        let t = named(&tgt.routes, &self.target_name(d).name, |r| &r.name);
        if let Some(s) = s {
            if !self.has(Feature::ServiceBroker) {
                self.warn(owarn::unsupported("route", &quote(&s.name)));
            }
            if self.has(Feature::LocalRoutesOnly) && !s.address.eq_ignore_ascii_case("local") {
                self.warn(owarn::non_local_route(&quote(&s.name)));
            }
        }
        let test = |n: &str| format!("sys.routes WHERE name = {}", ddl::sql_string(n));
        let rebuild = |s: &Route, t: &Route| {
            (s.mirror_address.is_none() && t.mirror_address.is_some()) || (s.broker_instance.is_none() && t.broker_instance.is_some()) || s.expires.is_some() != t.expires.is_some()
        };
        if let Some(t) = t
            && s.is_none_or(|s| rebuild(s, t))
        {
            let sql = self.ex.listed(&test(&t.name), Check::Drop, &format!("DROP ROUTE {}", quote(&t.name)));
            self.group(Phase::DropRoutes, "Dropping routes", vec![sql]);
        }
        match (s, t) {
            (Some(s), Some(t)) if !rebuild(s, t) => {
                let mut statements: Vec<String> = ddl_objects::alter_route(s, t).into_iter().collect();
                if s.owner != t.owner && !self.owners_ignored() {
                    statements.push(alter_authorization("ROUTE", &s.name, &s.owner));
                }
                self.ranked_step(Phase::Routes, 0, "Altering routes", statements, true);
            }
            (Some(s), _) => {
                self.recreate.insert(ObjectName::new("", &s.name));
                let create = ddl_objects::create_route(s);
                let sql = if s.expires.is_some() { create } else { self.ex.listed(&test(&s.name), Check::Create, &create) };
                self.ranked_step(Phase::Routes, 1, "Creating routes", vec![sql], true);
            }
            _ => {}
        }
    }

    // ---- Keys and certificates ----

    fn crypto(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let n = &d.name.name;
        let tn = self.target_name(d).name;
        let label = quote(n);
        let (kind, class, id, plural, description, owners) = match d.object_type {
            ObjectType::Certificate => {
                let (s, t) = (named(&src.certificates, n, |c| &c.name), named(&tgt.certificates, &tn, |c| &c.name));
                ("certificate", "CERTIFICATE", "CERT_ID", "certificates", s.map(ddl_objects::describe_certificate), s.zip(t).map(|(s, t)| (s.owner.clone(), t.owner.clone())))
            }
            ObjectType::AsymmetricKey => {
                let (s, t) = (named(&src.asymmetric_keys, n, |c| &c.name), named(&tgt.asymmetric_keys, &tn, |c| &c.name));
                ("asymmetric key", "ASYMMETRIC KEY", "ASYMKEY_ID", "asymmetric keys", s.map(ddl_objects::describe_asymmetric_key), s.zip(t).map(|(s, t)| (s.owner.clone(), t.owner.clone())))
            }
            _ => {
                let (s, t) = (named(&src.symmetric_keys, n, |c| &c.name), named(&tgt.symmetric_keys, &tn, |c| &c.name));
                ("symmetric key", "SYMMETRIC KEY", "KEY_ID", "symmetric keys", s.map(ddl_objects::describe_symmetric_key), s.zip(t).map(|(s, t)| (s.owner.clone(), t.owner.clone())))
            }
        };
        let rank = match d.object_type {
            ObjectType::Certificate => 0,
            ObjectType::SymmetricKey => 2,
            _ => 4,
        };
        let title = |verb: &str| format!("{verb} {plural}");
        match d.kind {
            DiffKind::OnlyIn1 => {
                if let Some(text) = description {
                    self.ranked_step(Phase::PreCrypto, rank, title("Creating"), vec![text], true);
                }
            }
            DiffKind::OnlyIn2 => {
                let sql = self.ex.id(id, &tn, Check::Drop, &format!("DROP {class} {}", quote(&tn)));
                let phase = if d.object_type == ObjectType::Certificate { Phase::DropCertificates } else { Phase::DropCryptoKeys };
                self.ranked_step(phase, rank, title("Dropping"), vec![sql], true);
            }
            DiffKind::Different => {
                if d.object_type == ObjectType::SymmetricKey {
                    self.warn(owarn::symmetric_key_contents(&label));
                }
                // Only the owner can change in place; key material can't be read to re-create it.
                let only_owner = d.properties.iter().all(|p| p.name == "Owner" || p.name == "Permission");
                if !only_owner {
                    self.warn(owarn::cannot_be_recreated(kind, &label));
                    if let Some(text) = description {
                        self.ranked_step(Phase::PreCrypto, rank, title("Creating"), vec![text], true);
                    }
                }
                if let Some((so, to)) = owners
                    && so != to
                    && !self.owners_ignored()
                {
                    self.ranked_step(Phase::PreCrypto, rank + 1, title("Altering"), vec![alter_authorization(class, n, &so)], true);
                }
            }
            DiffKind::Equal => {}
        }
    }

    // ---- Security policies ----

    fn security_policy(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let tn = self.target_name(d);
        let s = scoped(&src.security_policies, &d.name, |p| (&p.schema, &p.name));
        let t = scoped(&tgt.security_policies, &tn, |p| (&p.schema, &p.name));
        if s.is_some() && !self.has(Feature::RowLevelSecurity) {
            self.warn(owarn::unsupported("security policy", &d.name.to_string()));
            return;
        }
        match (s, t) {
            (Some(s), Some(t)) if s.schema_bound == t.schema_bound && !self.dropped.contains(&tn) => {
                let statements: Vec<String> = ddl_objects::alter_security_policy(s, t).into_iter().map(|x| self.ex.object(&tn, "SP", Check::Alter, &x)).collect();
                self.core(Phase::CreateObjects, format!("Altering {}", d.name), d.name.clone(), statements);
            }
            (s, t) => {
                if t.is_some() && self.dropped.insert(tn.clone()) {
                    self.core(Phase::DropObjects, format!("Dropping {tn}"), tn.clone(), vec![self.ex.object(&tn, "SP", Check::Drop, &format!("DROP SECURITY POLICY {tn}"))]);
                }
                if let Some(s) = s {
                    self.create_security_policy(&d.name, s);
                }
            }
        }
    }

    fn create_security_policy(&mut self, name: &ObjectName, s: &SecurityPolicy) {
        if !self.recreate.insert(name.clone()) {
            return;
        }
        let sql = self.ex.object(name, "SP", Check::Create, &ddl_objects::create_security_policy(s));
        self.core(Phase::CreateObjects, format!("Creating {name}"), name.clone(), vec![sql]);
    }

    // ---- PolyBase ----

    fn external(&mut self, d: &Difference) {
        let (src, tgt) = (self.src, self.tgt);
        let tn = self.target_name(d);
        let n = &d.name.name;
        let azure_mismatch = src.server.is_azure_sql_database() != tgt.server.is_azure_sql_database();
        match d.object_type {
            ObjectType::ExternalFileFormat => {
                let s = named(&src.external_file_formats, n, |x| &x.name);
                if s.is_some() && (!self.has(Feature::PolyBase) || self.r.features.is_azure()) {
                    self.warn(owarn::polybase_unsupported("file formats", &quote(n)));
                }
                let test = |n: &str| format!("sys.external_file_formats WHERE name = {}", ddl::sql_string(n));
                if named(&tgt.external_file_formats, &tn.name, |x| &x.name).is_some() {
                    // External tables that use it go and come back.
                    let users: Vec<&ExternalTable> = tgt.external_tables.iter().filter(|t| t.file_format.as_deref().is_some_and(|f| self.same(f, &tn.name))).collect();
                    for t in users {
                        self.rebuild_external_table(t);
                    }
                    let sql = self.ex.listed(&test(&tn.name), Check::Drop, &format!("DROP EXTERNAL FILE FORMAT {}", quote(&tn.name)));
                    self.ranked_step(Phase::DropExternal, 2, "Dropping external file formats", vec![sql], true);
                }
                if let Some(s) = s {
                    let sql = self.ex.listed(&test(n), Check::Create, &ddl_objects::create_external_file_format(s));
                    self.ranked_step(Phase::CreateExternalFormats, 0, "Creating external file formats", vec![sql], true);
                }
            }
            ObjectType::ExternalDataSource => {
                let s = named(&src.external_data_sources, n, |x| &x.name);
                if let Some(s) = s {
                    if !self.has(Feature::PolyBase) {
                        self.warn(owarn::polybase_unsupported("data sources", &quote(n)));
                    }
                    if azure_mismatch {
                        self.warn(owarn::polybase_platform(&quote(n)));
                    }
                    if let Some(c) = &s.credential
                        && !tgt.credentials.iter().any(|x| self.same(x, c))
                    {
                        self.warn(owarn::missing_credential(&quote(n), c));
                    }
                }
                let test = |n: &str| format!("sys.external_data_sources WHERE name = {}", ddl::sql_string(n));
                if named(&tgt.external_data_sources, &tn.name, |x| &x.name).is_some() {
                    let users: Vec<&ExternalTable> = tgt.external_tables.iter().filter(|t| self.same(&t.data_source, &tn.name)).collect();
                    for t in users {
                        self.rebuild_external_table(t);
                    }
                    let sql = self.ex.listed(&test(&tn.name), Check::Drop, &format!("DROP EXTERNAL DATA SOURCE {}", quote(&tn.name)));
                    self.ranked_step(Phase::DropExternal, 1, "Dropping external data sources", vec![sql], true);
                }
                if let Some(s) = s {
                    let sql = self.ex.listed(&test(n), Check::Create, &ddl_objects::create_external_data_source(s));
                    self.ranked_step(Phase::CreateExternalFormats, 1, "Creating external data sources", vec![sql], true);
                }
            }
            _ => {
                let s = scoped(&src.external_tables, &d.name, |x| (&x.schema, &x.name));
                if s.is_some() {
                    if !self.has(Feature::PolyBase) {
                        self.warn(owarn::polybase_unsupported("tables", &d.name.to_string()));
                    }
                    if azure_mismatch {
                        self.warn(owarn::polybase_platform(&d.name.to_string()));
                    }
                }
                if let Some(t) = scoped(&tgt.external_tables, &tn, |x| (&x.schema, &x.name)) {
                    self.drop_external_table(t);
                }
                if let Some(s) = s {
                    self.create_external_table(s);
                }
            }
        }
    }

    fn drop_external_table(&mut self, t: &ExternalTable) {
        let name = ObjectName::new(&t.schema, &t.name);
        if !self.dropped.insert(name.clone()) {
            return;
        }
        let test = format!("sys.external_tables WHERE name = {} AND schema_id = SCHEMA_ID({})", ddl::sql_string(&t.name), ddl::sql_string(&t.schema));
        let sql = self.ex.listed(&test, Check::Drop, &format!("DROP EXTERNAL TABLE {name}"));
        self.ranked_step(Phase::DropExternal, 0, "Dropping external tables", vec![sql], true);
    }

    fn create_external_table(&mut self, s: &ExternalTable) {
        let name = ObjectName::new(&s.schema, &s.name);
        if !self.recreate.insert(name.clone()) {
            return;
        }
        let test = format!("sys.external_tables WHERE name = {} AND schema_id = SCHEMA_ID({})", ddl::sql_string(&s.name), ddl::sql_string(&s.schema));
        let sql = self.ex.listed(&test, Check::Create, &ddl_objects::create_external_table(s));
        self.group(Phase::CreateExternalTables, "Creating external tables", vec![sql]);
    }

    /// An external table that is not deployed itself but whose data source or file format is re-created.
    fn rebuild_external_table(&mut self, t: &ExternalTable) {
        self.drop_external_table(t);
        let src = self.src;
        if let Some(s) = src.external_tables.iter().find(|x| x.schema == t.schema && x.name == t.name) {
            self.create_external_table(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::compare::compare;

    fn script(src: &Database, tgt: &Database, opts: &Options) -> DeploymentScript {
        let c = compare(src, tgt, opts, &Mappings::default());
        let settings = ScriptSettings {
            transaction_isolation_level: "SERIALIZABLE".into(),
            source: "s".into(),
            target: "t".into(),
            target_database: "t".into(),
            target_major_version: 0,
        };
        generate(&c, src, tgt, opts, &Mappings::default(), &settings).unwrap()
    }

    fn in_order(sql: &str, parts: &[&str]) {
        let pos: Vec<usize> = parts.iter().map(|p| sql.find(p).unwrap_or_else(|| panic!("{p}\n{sql}"))).collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "{sql}");
    }

    fn broker() -> Database {
        Database {
            message_types: vec![MessageType { name: "M".into(), validation: "NONE".into(), ..Default::default() }],
            contracts: vec![Contract { name: "C".into(), messages: vec![ContractMessage { message_type: "M".into(), sent_by: "ANY".into() }], ..Default::default() }],
            queues: vec![Queue { schema: "dbo".into(), name: "Q".into(), ..Default::default() }],
            services: vec![Service { name: "S".into(), queue: ObjectName::new("dbo", "Q"), contracts: vec!["C".into()], ..Default::default() }],
            dependencies: vec![
                Dependency { from: ObjectName::new(KEY_CONTRACT, "C"), to: ObjectName::new(KEY_MESSAGE_TYPE, "M"), strong: true },
                Dependency { from: ObjectName::new(KEY_SERVICE, "S"), to: ObjectName::new("dbo", "Q"), strong: true },
                Dependency { from: ObjectName::new(KEY_SERVICE, "S"), to: ObjectName::new(KEY_CONTRACT, "C"), strong: true },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn service_broker_in_dependency_order() {
        let opts = Options::default_set();
        let s = script(&broker(), &Database::default(), &opts);
        in_order(&s.sql, &["CREATE MESSAGE TYPE [M]", "CREATE CONTRACT [C]", "CREATE SERVICE [S]"]);
        assert!(s.sql.contains("CREATE QUEUE [dbo].[Q]\r\nWITH STATUS=ON,\r\nRETENTION=OFF,\r\nPOISON_MESSAGE_HANDLING (STATUS=ON)"), "{}", s.sql);
        let s = script(&Database::default(), &broker(), &opts);
        in_order(&s.sql, &["DROP SERVICE [S]", "DROP CONTRACT [C]", "DROP MESSAGE TYPE [M]"]);
        // A contract whose messages change is re-created, and the service on it too.
        let mut changed = broker();
        changed.contracts[0].messages[0].sent_by = "INITIATOR".into();
        let s = script(&changed, &broker(), &opts);
        in_order(&s.sql, &["DROP SERVICE [S]", "DROP CONTRACT [C]", "CREATE CONTRACT [C]", "CREATE SERVICE [S]"]);
        // Azure SQL Database has no Service Broker.
        let mut azure = Database::default();
        azure.server.engine_edition = 5;
        let s = script(&broker(), &azure, &opts);
        assert!(!s.sql.contains("CREATE QUEUE") && s.warnings.iter().any(|w| w.message == "The queue [dbo].[Q] is not supported in the target database version."), "{:?}", s.warnings);
    }

    #[test]
    fn partition_functions_split_and_merge() {
        let pf = |b: &[&str]| PartitionFunction { name: "pf".into(), data_type: DataType { name: "int".into(), ..Default::default() }, range_right: true, boundaries: b.iter().map(|x| x.to_string()).collect() };
        let ps = PartitionScheme { name: "ps".into(), function: "pf".into(), filegroups: vec!["PRIMARY".into(); 3] };
        let db = |f| Database { partition_functions: vec![f], partition_schemes: vec![ps.clone()], ..Default::default() };
        let opts = Options::default_set().without(Opt::IgnoreFileGroups);
        let s = script(&db(pf(&["2", "3"])), &db(pf(&["1", "2"])), &opts);
        in_order(&s.sql, &["MERGE RANGE (1)", "ALTER PARTITION SCHEME [ps] NEXT USED [PRIMARY]", "SPLIT RANGE (3)"]);
        // Not compared when filegroups are ignored (the default).
        assert!(script(&db(pf(&["2", "3"])), &db(pf(&["1", "2"])), &Options::default_set()).is_empty());
    }

    #[test]
    fn ddl_triggers_and_policies() {
        let tr = DdlTrigger { name: "audit".into(), definition: "CREATE TRIGGER audit ON DATABASE FOR CREATE_TABLE AS PRINT 1".into(), disabled: true, ..Default::default() };
        let src = Database { ddl_triggers: vec![tr], ..Default::default() };
        let s = script(&src, &Database::default(), &Options::default_set());
        in_order(&s.sql, &["PRINT N'Creating DDL triggers'", "CREATE TRIGGER [audit] ON DATABASE", "DISABLE TRIGGER [audit] ON DATABASE"]);
        let s = script(&Database::default(), &src, &Options::default_set());
        assert!(s.sql.contains("DROP TRIGGER [audit] ON DATABASE"), "{}", s.sql);
        let pr = SecurityPredicate { kind: "FILTER".into(), function: "[rls].[f]".into(), arguments: "([TenantId])".into(), table: ObjectName::new("dbo", "T"), operation: None };
        let pol = SecurityPolicy { schema: "rls".into(), name: "P".into(), enabled: true, schema_bound: true, predicates: vec![pr], ..Default::default() };
        let mut off = pol.clone();
        off.enabled = false;
        let s = script(&Database { security_policies: vec![off], ..Default::default() }, &Database { security_policies: vec![pol], ..Default::default() }, &Options::default_set());
        assert!(s.sql.contains("ALTER SECURITY POLICY [rls].[P]\r\nWITH (STATE = OFF)"), "{}", s.sql);
    }

    #[test]
    fn fulltext_outside_the_transaction() {
        let col = Column { name: "Body".into(), data_type: DataType { name: "nvarchar".into(), length: Some(-1), ..Default::default() }, nullable: true, ..Default::default() };
        let id = Column { name: "Id".into(), data_type: DataType { name: "int".into(), ..Default::default() }, ..Default::default() };
        let pk = Index { name: "PK".into(), kind: IndexKind::Clustered, primary_key: true, unique: true, columns: vec![IndexColumn { name: "Id".into(), descending: false }], allow_row_locks: true, allow_page_locks: true, ..Default::default() };
        let ft = FullTextIndex { catalog: "Cat".into(), key_index: "PK".into(), change_tracking: "AUTO".into(), stoplist: "SYSTEM".into(), columns: vec![FullTextColumn { name: "Body".into(), type_column: None, language: 1033 }], ..Default::default() };
        let t = Table { schema: "dbo".into(), name: "Doc".into(), columns: vec![id, col], indexes: vec![pk], lock_escalation: "TABLE".into(), fulltext_index: Some(ft), ..Default::default() };
        let src = Database { tables: vec![t], fulltext_catalogs: vec![FullTextCatalog { name: "Cat".into(), ..Default::default() }], ..Default::default() };
        let s = script(&src, &Database::default(), &Options::default_set());
        in_order(&s.sql, &["CREATE FULLTEXT CATALOG [Cat]", "BEGIN TRANSACTION", "CREATE TABLE [dbo].[Doc]", "COMMIT TRANSACTION", "CREATE FULLTEXT INDEX ON [dbo].[Doc] KEY INDEX [PK] ON [Cat]", "ADD ([Body] LANGUAGE 1033)", "ENABLE"]);
        assert!(s.warnings.iter().any(|w| w.message.starts_with("Full-text indexes can't be deployed in transactions")));
        let s = script(&Database::default(), &src, &Options::default_set());
        in_order(&s.sql, &["DROP FULLTEXT INDEX ON [dbo].[Doc]", "DROP FULLTEXT CATALOG [Cat]", "BEGIN TRANSACTION", "DROP TABLE [dbo].[Doc]"]);
        assert!(script(&src, &Database::default(), &Options::default_set().with(Opt::IgnoreFullTextIndexing)).sql.find("FULLTEXT").is_none());
    }

    #[test]
    fn certificates_are_comments() {
        let cert = Certificate { name: "C".into(), subject: "Quill".into(), private_key: "PW".into(), ..Default::default() };
        let src = Database { certificates: vec![cert], ..Default::default() };
        let s = script(&src, &Database::default(), &Options::default_set());
        assert!(s.sql.contains("-- Certificate [C]\r\n--   Authorization: dbo\r\n--   Subject: Quill") && s.sql.contains("Encrypted by password"), "{}", s.sql);
        let s = script(&Database::default(), &src, &Options::default_set());
        in_order(&s.sql, &["COMMIT TRANSACTION", "DROP CERTIFICATE [C]"]);
        assert!(script(&src, &Database::default(), &Options::default_set().with(Opt::IgnoreCertificatesAndCryptoKeys)).is_empty());
    }
}
