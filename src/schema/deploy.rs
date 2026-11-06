//! Turns the selected differences into a deployment script that makes database 2 (the
//! target) match database 1 (the source).
//!
//! Planning puts every statement into a phase; phases run in a fixed order (drops of
//! dependants first, then creates and alters in dependency order, then keys, triggers and
//! extended properties), and objects inside the create and drop phases are sorted by the
//! dependency graph of the source and target respectively.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::cast::{self, Cast};
use super::compare::{Comparison, DiffKind, Difference, Mappings, ObjectType, column_differences};
use super::ddl::{self, qname};
use super::options::{Opt, Options};
use crate::catalog::*;
use crate::db::{literal, quote};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub severity: Severity,
    pub object: String,
    pub message: String,
}

/// Settings for the script that are not comparison options.
#[derive(Clone, Debug)]
pub struct ScriptSettings {
    pub transaction_isolation_level: String,
    /// `server.database` of each side, for the header.
    pub source: String,
    pub target: String,
    pub target_database: String,
    pub target_major_version: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentScript {
    pub sql: String,
    pub warnings: Vec<Warning>,
    /// The statements in execution order, without GO; empty when nothing changes.
    pub batches: Vec<String>,
}

impl DeploymentScript {
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    pub fn worst(&self) -> Option<Severity> {
        self.warnings.iter().map(|w| w.severity).max()
    }
}

/// When a statement runs. The order of the variants is the order of the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Phase {
    CreateSchemas,
    CreatePrincipals,
    RoleMembers,
    DropExtendedProperties,
    DropForeignKeys,
    DropChecks,
    DropKeys,
    DropDefaults,
    DropIndexes,
    DropTriggers,
    /// Views, functions, procedures and tables, dependants first.
    DropObjects,
    DropSynonyms,
    DropSequences,
    DropTypes,
    CreateTypes,
    CreateSequences,
    /// Tables, views, functions and procedures, dependencies first.
    CreateObjects,
    CreateIndexes,
    CreateChecks,
    CreateForeignKeys,
    CreateDefaults,
    Triggers,
    CreateSynonyms,
    ExtendedProperties,
    Permissions,
    DropSchemas,
    DropPrincipals,
}

#[derive(Debug)]
struct Step {
    phase: Phase,
    /// Position inside a dependency-sorted phase.
    rank: usize,
    print: String,
    statements: Vec<String>,
    /// ANSI_NULLS and QUOTED_IDENTIFIER the statements need (modules).
    settings: Option<ModuleSettings>,
}

pub fn generate(
    comparison: &Comparison,
    source: &Database,
    target: &Database,
    opts: &Options,
    mappings: &Mappings,
    settings: &ScriptSettings,
) -> Result<DeploymentScript, String> {
    opts.validate()?;
    let mut p = Planner::new(comparison, source, target, opts, mappings, settings);
    p.plan();
    Ok(p.emit())
}

struct Planner<'a> {
    cmp: &'a Comparison,
    src: &'a Database,
    tgt: &'a Database,
    opts: &'a Options,
    mappings: &'a Mappings,
    settings: &'a ScriptSettings,
    steps: Vec<Step>,
    warnings: Vec<Warning>,
    /// Objects (by target name) whose dependants must be dropped and re-created.
    disturbed: BTreeSet<ObjectName>,
    /// Target modules already dropped by this script.
    dropped: HashSet<ObjectName>,
    /// Constraints and indexes already dropped: (table, name).
    dropped_sub: HashSet<(ObjectName, String)>,
    /// Source modules that will be created (rather than altered).
    recreate: BTreeSet<ObjectName>,
    /// Foreign keys already added: (table, name).
    added_fks: HashSet<(ObjectName, String)>,
    case_sensitive: bool,
}

impl<'a> Planner<'a> {
    fn new(
        cmp: &'a Comparison,
        src: &'a Database,
        tgt: &'a Database,
        opts: &'a Options,
        mappings: &'a Mappings,
        settings: &'a ScriptSettings,
    ) -> Self {
        Planner {
            cmp,
            src,
            tgt,
            opts,
            mappings,
            settings,
            steps: Vec::new(),
            warnings: Vec::new(),
            disturbed: BTreeSet::new(),
            dropped: HashSet::new(),
            dropped_sub: HashSet::new(),
            recreate: BTreeSet::new(),
            added_fks: HashSet::new(),
            case_sensitive: opts.has(Opt::CaseSensitiveObjectDefinition),
        }
    }

    fn key(&self, s: &str) -> String {
        if self.case_sensitive { s.to_string() } else { s.to_lowercase() }
    }

    fn same(&self, a: &str, b: &str) -> bool {
        self.key(a) == self.key(b)
    }

    fn warn(&mut self, severity: Severity, object: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(Warning { severity, object: object.into(), message: message.into() });
    }

    fn step(&mut self, phase: Phase, print: impl Into<String>, statements: Vec<String>) {
        if !statements.is_empty() {
            self.steps.push(Step { phase, rank: 0, print: print.into(), statements, settings: None });
        }
    }

    fn module_step(&mut self, phase: Phase, print: impl Into<String>, statements: Vec<String>, settings: &ModuleSettings) {
        self.steps.push(Step { phase, rank: 0, print: print.into(), statements, settings: Some(settings.clone()) });
    }

    fn selected(&self) -> Vec<&'a Difference> {
        let mut chosen: Vec<&Difference> = self.cmp.differences.iter().filter(|d| d.selected && d.kind != DiffKind::Equal).collect();
        if self.opts.has(Opt::IncludeDependencies) {
            // Objects that selected creates and alters need, and that differ, come along.
            let mut names: HashSet<ObjectName> = chosen.iter().map(|d| d.name.clone()).collect();
            let mut queue: Vec<ObjectName> = chosen.iter().filter(|d| d.kind != DiffKind::OnlyIn2).map(|d| d.name.clone()).collect();
            while let Some(n) = queue.pop() {
                for dep in self.src.dependencies.iter().filter(|d| d.from == n) {
                    if names.contains(&dep.to) {
                        continue;
                    }
                    if let Some(d) = self.cmp.differences.iter().find(|d| {
                        d.name == dep.to && matches!(d.kind, DiffKind::OnlyIn1 | DiffKind::Different)
                    }) {
                        names.insert(d.name.clone());
                        queue.push(d.name.clone());
                        chosen.push(d);
                    }
                }
            }
        }
        chosen
    }

    fn target_name(&self, d: &Difference) -> ObjectName {
        d.target_name.clone().unwrap_or_else(|| d.name.clone())
    }

    fn plan(&mut self) {
        let selected = self.selected();
        for d in &selected {
            match d.object_type {
                ObjectType::Table => self.table(d),
                ObjectType::Schema => self.schema(d),
                ObjectType::Role | ObjectType::User => self.principal(d),
                _ => {}
            }
        }
        for d in &selected {
            match d.object_type {
                ObjectType::View | ObjectType::Function | ObjectType::StoredProcedure => self.module(d),
                ObjectType::UserDefinedType => self.user_type(d),
                ObjectType::Sequence => self.sequence(d),
                ObjectType::Synonym => self.synonym(d),
                ObjectType::ExtendedProperty => self.database_property(d),
                _ => {}
            }
        }
        self.dependants();
        self.extended_properties(&selected);
        self.permissions(&selected);
        self.sort();
    }

    // ---- Schemas, types, sequences, synonyms ----

    fn schema(&mut self, d: &Difference) {
        let name = &d.name.name;
        match d.kind {
            DiffKind::OnlyIn1 => {
                let owner = self.src.schemas.iter().find(|s| s.name == *name).map(|s| s.owner.clone()).unwrap_or_else(|| "dbo".into());
                self.step(
                    Phase::CreateSchemas,
                    format!("Creating schema {}", quote(name)),
                    vec![format!("CREATE SCHEMA {} AUTHORIZATION {}", quote(name), quote(&owner))],
                );
            }
            DiffKind::OnlyIn2 => {
                self.step(Phase::DropSchemas, format!("Dropping schema {}", quote(name)), vec![format!("DROP SCHEMA {}", quote(name))]);
            }
            DiffKind::Different => {
                if let Some(s) = self.src.schemas.iter().find(|s| s.name == *name) {
                    self.step(
                        Phase::CreateSchemas,
                        format!("Altering schema {}", quote(name)),
                        vec![format!("ALTER AUTHORIZATION ON SCHEMA::{} TO {}", quote(name), quote(&s.owner))],
                    );
                }
            }
            DiffKind::Equal => {}
        }
    }

    fn user_type(&mut self, d: &Difference) {
        let s = self.src.types.iter().find(|t| t.schema == d.name.schema && t.name == d.name.name);
        let tn = self.target_name(d);
        let t = self.tgt.types.iter().find(|t| t.schema == tn.schema && t.name == tn.name);
        if let Some(t) = t {
            let users: Vec<String> = self
                .tgt
                .tables
                .iter()
                .filter(|tab| tab.columns.iter().any(|c| c.data_type.schema.as_deref() == Some(&t.schema) && c.data_type.name == t.name))
                .map(|tab| tab.object_name().to_string())
                .collect();
            if !users.is_empty() {
                self.warn(
                    Severity::High,
                    tn.to_string(),
                    format!("type {tn} is used by {}; drop and re-create those columns' tables first, or the script will fail", users.join(", ")),
                );
            }
            // Modules that use the type go away with it and come back from the source.
            self.disturbed.insert(tn.clone());
            self.step(Phase::DropTypes, format!("Dropping type {tn}"), vec![format!("DROP TYPE {tn}")]);
        }
        if let Some(s) = s {
            self.step(Phase::CreateTypes, format!("Creating type {}", d.name), vec![ddl::create_type(s)]);
        }
    }

    fn sequence(&mut self, d: &Difference) {
        let s = self.src.sequences.iter().find(|q| q.schema == d.name.schema && q.name == d.name.name);
        let tn = self.target_name(d);
        let t = self.tgt.sequences.iter().find(|q| q.schema == tn.schema && q.name == tn.name);
        match (s, t) {
            (Some(s), None) => self.step(Phase::CreateSequences, format!("Creating sequence {}", d.name), vec![ddl::create_sequence(s)]),
            (None, Some(_)) => self.step(Phase::DropSequences, format!("Dropping sequence {tn}"), vec![format!("DROP SEQUENCE {tn}")]),
            (Some(s), Some(t)) => {
                if s.data_type.sql() != t.data_type.sql() {
                    self.warn(Severity::Low, tn.to_string(), "the sequence is dropped and re-created, so it restarts at its START WITH value");
                    self.step(Phase::DropSequences, format!("Dropping sequence {tn}"), vec![format!("DROP SEQUENCE {tn}")]);
                    self.step(Phase::CreateSequences, format!("Creating sequence {}", d.name), vec![ddl::create_sequence(s)]);
                } else if let Some(sql) = ddl::alter_sequence(s, t) {
                    self.step(Phase::CreateSequences, format!("Altering sequence {tn}"), vec![sql]);
                }
            }
            _ => {}
        }
    }

    fn synonym(&mut self, d: &Difference) {
        let s = self.src.synonyms.iter().find(|q| q.schema == d.name.schema && q.name == d.name.name);
        let tn = self.target_name(d);
        if self.tgt.synonyms.iter().any(|q| q.schema == tn.schema && q.name == tn.name) {
            self.step(Phase::DropSynonyms, format!("Dropping synonym {tn}"), vec![format!("DROP SYNONYM {tn}")]);
        }
        if let Some(s) = s {
            self.step(Phase::CreateSynonyms, format!("Creating synonym {}", d.name), vec![ddl::create_synonym(s)]);
        }
    }

    fn database_property(&mut self, d: &Difference) {
        let find = |db: &'a Database| db.extended_properties.iter().find(|p| p.class == "DATABASE" && p.name == d.name.name);
        let levels = "NULL, NULL, NULL, NULL, NULL, NULL";
        match (find(self.src), find(self.tgt)) {
            (Some(s), None) => self.step(Phase::ExtendedProperties, "Adding extended properties", vec![ddl::add_xprop(s, levels)]),
            (Some(s), Some(_)) => self.step(Phase::ExtendedProperties, "Altering extended properties", vec![ddl::update_xprop(s, levels)]),
            (None, Some(t)) => self.step(Phase::DropExtendedProperties, "Dropping extended properties", vec![ddl::drop_xprop(t, levels)]),
            _ => {}
        }
    }

    // ---- Modules ----

    fn source_module(&self, name: &ObjectName) -> Option<Module<'a>> {
        let src = self.src;
        src.views
            .iter()
            .find(|v| v.schema == name.schema && v.name == name.name)
            .map(Module::View)
            .or_else(|| src.routines.iter().find(|r| r.schema == name.schema && r.name == name.name).map(Module::Routine))
    }

    fn target_module(&self, name: &ObjectName) -> Option<Module<'a>> {
        let tgt = self.tgt;
        tgt.views
            .iter()
            .find(|v| v.schema == name.schema && v.name == name.name)
            .map(Module::View)
            .or_else(|| tgt.routines.iter().find(|r| r.schema == name.schema && r.name == name.name).map(Module::Routine))
    }

    fn module(&mut self, d: &Difference) {
        let tn = self.target_name(d);
        let source = self.source_module(&d.name);
        let target = self.target_module(&tn);
        let drop_create = self.opts.has(Opt::DropAndCreateInsteadOfAlter)
            || matches!((&source, &target), (Some(Module::Routine(a)), Some(Module::Routine(b))) if function_class(a) != function_class(b));
        match (source, target) {
            (Some(s), None) => self.create_module(&d.name, s),
            (None, Some(t)) => self.drop_module(&tn, t),
            (Some(s), Some(t)) => {
                if s.encrypted() {
                    self.warn(Severity::High, d.name.to_string(), "the source module is encrypted and cannot be scripted; it is left as it is");
                    return;
                }
                // Anything schema-bound to this module has to be dropped first and re-created.
                self.disturbed.insert(tn.clone());
                if drop_create {
                    self.drop_module(&tn, t);
                    self.create_module(&d.name, s);
                } else {
                    let verb = if self.opts.has(Opt::CreateOrAlterInsteadOfAlter) && self.settings.target_major_version >= 13 {
                        "CREATE OR ALTER"
                    } else {
                        "ALTER"
                    };
                    let sql = ddl::module_sql(s.definition(), verb, &d.name.quoted());
                    self.module_step(Phase::CreateObjects, format!("Altering {}", d.name), vec![sql], s.settings());
                    // ALTER VIEW removes the view's indexes.
                    if let Module::View(v) = s {
                        self.view_indexes(&d.name, v);
                    }
                }
            }
            _ => {}
        }
    }

    fn create_module(&mut self, name: &ObjectName, m: Module<'a>) {
        if m.encrypted() {
            self.warn(Severity::High, name.to_string(), "the module is encrypted and cannot be scripted; it is not created");
            return;
        }
        if m.is_clr() {
            self.warn(Severity::High, name.to_string(), "CLR modules are not scripted yet; deploy the assembly and module by hand");
            return;
        }
        self.recreate.insert(name.clone());
        let sql = ddl::module_sql(m.definition(), "CREATE", &name.quoted());
        self.module_step(Phase::CreateObjects, format!("Creating {name}"), vec![sql], m.settings());
        if let Module::View(v) = m {
            self.view_indexes(name, v);
            for tr in &v.triggers {
                self.create_trigger(name, tr);
            }
        }
    }

    fn drop_module(&mut self, name: &ObjectName, m: Module<'a>) {
        if !self.dropped.insert(name.clone()) {
            return;
        }
        let kind = match m {
            Module::View(_) => "VIEW",
            Module::Routine(r) => ddl::routine_keyword(r),
        };
        self.disturbed.insert(name.clone());
        self.step(Phase::DropObjects, format!("Dropping {name}"), vec![ddl::drop_module(kind, &name.quoted())]);
    }

    fn view_indexes(&mut self, name: &ObjectName, v: &View) {
        let table = name.quoted();
        let mut ixs: Vec<&Index> = v.indexes.iter().collect();
        ixs.sort_by_key(|i| i.kind != IndexKind::Clustered);
        let statements: Vec<String> = ixs.iter().flat_map(|ix| ddl::create_index(&table, ix)).collect();
        self.step(Phase::CreateIndexes, format!("Creating indexes on {name}"), statements);
    }

    fn create_trigger(&mut self, parent: &ObjectName, tr: &Trigger) {
        if tr.encrypted || tr.definition.is_empty() {
            self.warn(Severity::High, format!("{parent}.{}", quote(&tr.name)), "the trigger is encrypted and cannot be scripted");
            return;
        }
        let name = qname(&parent.schema, &tr.name);
        let mut statements = vec![ddl::module_sql(&tr.definition, "CREATE", &name)];
        if tr.disabled {
            statements.push(format!("DISABLE TRIGGER {name} ON {}", parent.quoted()));
        }
        for (event, order) in &tr.order {
            statements.push(format!("EXEC sp_settriggerorder N'{}', '{}', '{event}'", name.replace('\'', "''"), title(order)));
        }
        self.module_step(Phase::Triggers, format!("Creating trigger {name} on {parent}"), statements, &ModuleSettings::default());
    }

    /// Re-creates (or drops) everything schema-bound to a disturbed object: views and functions
    /// can't survive a change to what they are bound to.
    fn dependants(&mut self) {
        let mut queue: Vec<ObjectName> = self.disturbed.iter().cloned().collect();
        let mut seen: HashSet<ObjectName> = queue.iter().cloned().collect();
        while let Some(n) = queue.pop() {
            let deps: Vec<ObjectName> =
                self.tgt.dependencies.iter().filter(|d| d.strong && d.to == n).map(|d| d.from.clone()).collect();
            for dep in deps {
                if !seen.insert(dep.clone()) {
                    continue;
                }
                let Some(t) = self.target_module(&dep) else { continue };
                queue.push(dep.clone());
                if self.dropped.contains(&dep) {
                    continue;
                }
                self.drop_module(&dep, t);
                // Bring it back from the source, unless the comparison already creates or drops it.
                let handled = self.recreate.contains(&dep);
                if !handled && let Some(s) = self.source_module(&dep) {
                    self.create_module(&dep, s);
                }
            }
        }
    }

    // ---- Tables ----

    fn table(&mut self, d: &Difference) {
        let tn = self.target_name(d);
        let s = self.src.table(&d.name.schema, &d.name.name);
        let t = self.tgt.table(&tn.schema, &tn.name);
        match (s, t) {
            (Some(s), None) => self.create_table(s),
            (None, Some(t)) => self.drop_table(t),
            (Some(s), Some(t)) => self.alter_table(s, t),
            _ => {}
        }
    }

    fn create_table(&mut self, s: &'a Table) {
        let name = s.object_name();
        if s.temporal.is_some() {
            self.warn(Severity::High, name.to_string(), "system-versioned tables are created without versioning by this version of QuillSQL");
        }
        self.recreate.insert(name.clone());
        let settings = ModuleSettings::default();
        let mut statements = vec![ddl::create_table(s, None, false)];
        if s.lock_escalation != "TABLE" && !s.lock_escalation.is_empty() {
            statements.push(format!("ALTER TABLE {} SET (LOCK_ESCALATION = {})", name.quoted(), s.lock_escalation));
        }
        if let Some(ct) = &s.change_tracking {
            statements.push(change_tracking_on(&name, ct));
        }
        self.module_step(Phase::CreateObjects, format!("Creating {name}"), statements, &settings);
        self.table_children(s, None);
    }

    /// Indexes, checks, foreign keys and triggers of a new or rebuilt table.
    fn table_children(&mut self, s: &'a Table, only_indexes_not_in: Option<&[String]>) {
        let name = s.object_name();
        let table = name.quoted();
        if !s.memory_optimized {
            let mut ixs: Vec<&Index> = s.indexes.iter().filter(|i| only_indexes_not_in.is_none_or(|skip| !skip.contains(&i.name))).collect();
            ixs.sort_by_key(|i| (!i.primary_key, i.kind != IndexKind::Clustered && i.kind != IndexKind::ClusteredColumnstore));
            let statements: Vec<String> = ixs.iter().flat_map(|ix| ddl::create_index(&table, ix)).collect();
            self.step(Phase::CreateIndexes, format!("Creating indexes on {name}"), statements);
        }
        let ignore = self.opts.has(Opt::IgnoreNocheckAndWithNocheck);
        let checks: Vec<String> = s.check_constraints.iter().flat_map(|c| ddl::add_check(&table, c, ignore)).collect();
        self.step(Phase::CreateChecks, format!("Adding constraints to {name}"), checks);
        let fks: Vec<&ForeignKey> = s.foreign_keys.iter().filter(|fk| self.referenced_exists(&fk.referenced_table)).collect();
        for fk in fks {
            self.add_foreign_key(&name, fk);
        }
        for tr in &s.triggers {
            self.create_trigger(&name, tr);
        }
    }

    /// A table a new foreign key points at will exist after the deployment.
    fn referenced_exists(&self, t: &ObjectName) -> bool {
        self.src.table(&t.schema, &t.name).is_some()
            && (self.tgt.table(&t.schema, &t.name).is_some()
                || self.cmp.differences.iter().any(|d| d.object_type == ObjectType::Table && d.name == *t && d.selected))
    }

    fn drop_table(&mut self, t: &'a Table) {
        let name = t.object_name();
        self.warn(Severity::High, name.to_string(), "the table is dropped and its data will be lost");
        if t.temporal.is_some() {
            self.step(
                Phase::DropObjects,
                format!("Turning off system versioning on {name}"),
                vec![format!("ALTER TABLE {} SET (SYSTEM_VERSIONING = OFF)", name.quoted())],
            );
        }
        self.drop_inbound_foreign_keys(t, false);
        self.disturbed.insert(name.clone());
        self.dropped.insert(name.clone());
        let mut statements = vec![format!("DROP TABLE {}", name.quoted())];
        if let Some(temporal) = &t.temporal {
            statements.push(format!("DROP TABLE {}", temporal.history_table.quoted()));
        }
        self.step(Phase::DropObjects, format!("Dropping {name}"), statements);
    }

    /// Foreign keys of other target tables that point at `t`; re-created from the source when
    /// `recreate` and the source still has them.
    fn drop_inbound_foreign_keys(&mut self, t: &Table, recreate: bool) {
        let name = t.object_name();
        let tgt = self.tgt;
        for other in tgt.tables.iter().filter(|o| o.object_name() != name) {
            for fk in other.foreign_keys.iter().filter(|fk| fk.referenced_table == name) {
                self.drop_foreign_key(other, fk);
                if recreate {
                    let src_fk = self
                        .src
                        .table(&other.schema, &other.name)
                        .and_then(|st| st.foreign_keys.iter().find(|f| self.same(&f.name, &fk.name)));
                    let fk = src_fk.unwrap_or(fk);
                    self.add_foreign_key(&other.object_name(), fk);
                }
            }
        }
    }

    /// Adds a foreign key once, however many reasons there are to (re-)create it.
    fn add_foreign_key(&mut self, owner: &ObjectName, fk: &ForeignKey) {
        if !self.added_fks.insert((owner.clone(), self.key(&fk.name))) {
            return;
        }
        let ignore = self.opts.has(Opt::IgnoreWithNocheck) || self.opts.has(Opt::IgnoreNocheckAndWithNocheck);
        let sql = ddl::add_foreign_key(&owner.quoted(), fk, ignore);
        self.step(Phase::CreateForeignKeys, format!("Adding foreign keys to {owner}"), sql);
    }

    fn drop_foreign_key(&mut self, owner: &Table, fk: &ForeignKey) {
        let name = owner.object_name();
        if self.dropped_sub.insert((name.clone(), self.key(&fk.name))) {
            self.step(Phase::DropForeignKeys, format!("Dropping foreign keys from {name}"), vec![ddl::drop_constraint(&name.quoted(), &fk.name)]);
        }
    }

    fn drop_sub(&mut self, phase: Phase, table: &ObjectName, name: &str, sql: String, what: &str) {
        if self.dropped_sub.insert((table.clone(), self.key(name))) {
            self.step(phase, format!("Dropping {what} {} from {table}", quote(name)), vec![sql]);
        }
    }

    /// Source column → target column for a table pair.
    fn column_pairs(&self, s: &'a Table, t: &'a Table) -> Vec<(Option<&'a Column>, Option<&'a Column>)> {
        let sname = s.object_name();
        let mapping = self.mappings.tables.iter().find(|m| m.source == sname);
        let mut used = vec![false; t.columns.len()];
        let mut out = Vec::new();
        for c in &s.columns {
            let wanted = mapping
                .and_then(|m| m.columns.iter().find(|(a, _)| self.same(a, &c.name)).map(|(_, b)| b.clone()))
                .or_else(|| {
                    self.cmp.auto_mapped_columns.iter().find(|(tab, a, _)| *tab == sname && *a == c.name).map(|(_, _, b)| b.clone())
                })
                .unwrap_or_else(|| c.name.clone());
            match t.columns.iter().enumerate().position(|(j, tc)| !used[j] && self.same(&tc.name, &wanted)) {
                Some(j) => {
                    used[j] = true;
                    out.push((Some(c), Some(&t.columns[j])));
                }
                None => out.push((Some(c), None)),
            }
        }
        for (j, tc) in t.columns.iter().enumerate() {
            if !used[j] {
                out.push((None, Some(tc)));
            }
        }
        out
    }

    /// Why the table must be rebuilt rather than altered, if it must.
    fn rebuild_reason(&self, s: &Table, t: &Table, pairs: &[(Option<&Column>, Option<&Column>)]) -> Option<String> {
        let o = self.opts;
        if s.memory_optimized || t.memory_optimized {
            return Some("memory-optimized tables cannot be altered in place".into());
        }
        if s.graph != t.graph {
            return Some("the graph table type changes".into());
        }
        if !o.has(Opt::IgnoreFileGroups) {
            if self.key(s.storage.data_space.as_deref().unwrap_or("PRIMARY")) != self.key(t.storage.data_space.as_deref().unwrap_or("PRIMARY"))
                || s.storage.partition_column != t.storage.partition_column
            {
                return Some("the filegroup or partitioning changes".into());
            }
            if let (Some(a), Some(b)) = (&s.text_filegroup, &t.text_filegroup)
                && !self.same(a, b)
            {
                return Some("the TEXTIMAGE_ON filegroup changes".into());
            }
        }
        let modified: Vec<&str> = pairs
            .iter()
            .filter_map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) if !column_differences(a, b, o, false).is_empty() => Some(b.name.as_str()),
                _ => None,
            })
            .collect();
        for (a, b) in pairs {
            let (Some(a), Some(b)) = (a, b) else {
                if let (Some(a), None) = (a, b)
                    && (a.column_set || a.filestream)
                {
                    return Some(format!("column {} is a column set or FILESTREAM column", a.name));
                }
                if let (None, Some(b)) = (a, b)
                    && b.filestream
                {
                    return Some(format!("FILESTREAM column {} is dropped", b.name));
                }
                continue;
            };
            if column_differences(a, b, o, false).is_empty() {
                continue;
            }
            if a.computed.is_none() && b.computed.is_none() {
                match cast::classify(&b.data_type, &a.data_type) {
                    Cast::Explicit | Cast::None => {
                        return Some(format!("column {} changes from {} to {}, which needs CAST", b.name, b.data_type.sql(), a.data_type.sql()));
                    }
                    _ => {}
                }
            }
            if (a.computed.is_some() || b.computed.is_some())
                && (a.computed.is_none() || b.computed.is_none())
            {
                return Some(format!("column {} changes between computed and regular", a.name));
            }
            if !o.has(Opt::IgnoreIdentityProperties) {
                let seed = |c: &Column| c.identity.as_ref().map(|i| (i.seed.clone(), i.increment.clone()));
                if a.identity.is_some() != b.identity.is_some()
                    || (!o.has(Opt::IgnoreIdentitySeedAndIncrement) && seed(a) != seed(b))
                {
                    return Some(format!("the IDENTITY property of column {} changes", a.name));
                }
            }
            if a.column_set != b.column_set || a.filestream != b.filestream {
                return Some(format!("column {} changes its column set or FILESTREAM property", a.name));
            }
            if b.generated_always.is_some() && t.temporal.is_some() {
                return Some(format!("period column {} of a system-versioned table changes", b.name));
            }
            if matches!(a.data_type.name.as_str(), "timestamp" | "rowversion") != matches!(b.data_type.name.as_str(), "timestamp" | "rowversion") {
                return Some(format!("column {} changes to or from rowversion", a.name));
            }
        }
        // A persisted computed column that reads a modified column.
        for c in s.columns.iter().filter(|c| c.computed.as_ref().is_some_and(|x| x.persisted)) {
            let def = c.computed.as_ref().unwrap().definition.to_lowercase();
            if let Some(m) = modified.iter().find(|m| def.contains(&format!("[{}]", m.to_lowercase())) && **m != c.name) {
                return Some(format!("persisted computed column {} uses modified column {m}", c.name));
            }
        }
        if o.has(Opt::ForceColumnOrder) {
            let projected: Vec<String> = t
                .columns
                .iter()
                .filter_map(|tc| pairs.iter().find(|(_, b)| b.is_some_and(|b| b.name == tc.name)).and_then(|(a, _)| a.map(|a| self.key(&a.name))))
                .chain(pairs.iter().filter(|(_, b)| b.is_none()).filter_map(|(a, _)| a.map(|a| self.key(&a.name))))
                .collect();
            let wanted: Vec<String> = s.columns.iter().map(|c| self.key(&c.name)).collect();
            if projected != wanted {
                return Some("the column order changes (Force column order)".into());
            }
        }
        None
    }

    fn alter_table(&mut self, s: &'a Table, t: &'a Table) {
        let pairs = self.column_pairs(s, t);
        if s.temporal.is_some() != t.temporal.is_some() {
            self.warn(
                Severity::High,
                t.object_name().to_string(),
                "turning system versioning on or off is not scripted by this version of QuillSQL; the table is left as it is",
            );
            return;
        }
        if let Some(reason) = self.rebuild_reason(s, t, &pairs) {
            if t.temporal.is_some() {
                self.warn(Severity::High, t.object_name().to_string(), format!("the table needs a rebuild ({reason}), which this version of QuillSQL does not script for system-versioned tables"));
                return;
            }
            self.rebuild_table(s, t, &pairs, &reason);
            return;
        }
        self.alter_in_place(s, t, &pairs);
    }

    /// Drops what depends on a target column that changes or goes away.
    fn release_column(&mut self, t: &'a Table, col: &str, keep_indexes: bool) {
        let name = t.object_name();
        let table = name.quoted();
        let mentions = |text: &str| {
            let low = text.to_lowercase();
            low.contains(&format!("[{}]", col.to_lowercase()))
        };
        if !keep_indexes {
            for ix in &t.indexes {
                let uses = ix.columns.iter().any(|c| self.same(&c.name, col))
                    || ix.included.iter().any(|c| self.same(c, col))
                    || ix.filter.as_deref().is_some_and(mentions)
                    || ix.storage.partition_column.as_deref().is_some_and(|p| self.same(p, col));
                if uses {
                    if ix.primary_key || ix.unique_constraint {
                        // Foreign keys that reference this key must go first.
                        let tgt = self.tgt;
                        for other in &tgt.tables {
                            for fk in other.foreign_keys.iter().filter(|fk| fk.referenced_table == name) {
                                self.drop_foreign_key(other, fk);
                            }
                        }
                    }
                    let phase = if ix.primary_key || ix.unique_constraint { Phase::DropKeys } else { Phase::DropIndexes };
                    self.drop_sub(phase, &name, &ix.name, ddl::drop_index(&table, ix), "index");
                }
            }
        }
        for ck in &t.check_constraints {
            if ck.column.as_deref().is_some_and(|c| self.same(c, col)) || mentions(&ck.definition) {
                self.drop_sub(Phase::DropChecks, &name, &ck.name, ddl::drop_constraint(&table, &ck.name), "constraint");
            }
        }
        if let Some(df) = t.columns.iter().find(|c| self.same(&c.name, col)).and_then(|c| c.default.as_ref()) {
            self.drop_sub(Phase::DropDefaults, &name, &df.name, ddl::drop_constraint(&table, &df.name), "default");
        }
        for fk in &t.foreign_keys {
            if fk.columns.iter().any(|c| self.same(c, col)) {
                self.drop_foreign_key(t, fk);
            }
        }
        let tgt = self.tgt;
        let inbound: Vec<(&Table, &ForeignKey)> = tgt
            .tables
            .iter()
            .flat_map(|other| other.foreign_keys.iter().map(move |fk| (other, fk)))
            .filter(|(_, fk)| fk.referenced_table == name && fk.referenced_columns.iter().any(|c| self.same(c, col)))
            .collect();
        for (other, fk) in inbound {
            self.drop_foreign_key(other, fk);
        }
    }

    fn alter_in_place(&mut self, s: &'a Table, t: &'a Table, pairs: &[(Option<&'a Column>, Option<&'a Column>)]) {
        let o = self.opts;
        let name = t.object_name();
        let table = name.quoted();
        let mut statements: Vec<String> = Vec::new();
        let mut after_defaults: Vec<String> = Vec::new();
        let mut disturbed = false;

        let mut adds = Vec::new();
        let mut drops = Vec::new();
        let mut computed_adds = Vec::new();
        for (a, b) in pairs {
            match (a, b) {
                (Some(a), None) if a.computed.is_some() => computed_adds.push(*a),
                (Some(a), None) => {
                    if !a.nullable && a.default.is_none() && a.identity.is_none() && a.data_type.name != "timestamp" && a.data_type.name != "rowversion" {
                        self.warn(
                            Severity::High,
                            name.to_string(),
                            format!("column {} is added as NOT NULL without a default; the deployment fails if the table has rows", a.name),
                        );
                    }
                    adds.push(*a);
                }
                (None, Some(b)) => {
                    self.release_column(t, &b.name, false);
                    drops.push(b.name.clone());
                    disturbed = true;
                }
                _ => {}
            }
        }
        let lost: Vec<String> = drops.iter().filter(|c| t.column(c).is_some_and(|c| c.computed.is_none())).cloned().collect();
        if !lost.is_empty() {
            self.warn(Severity::High, name.to_string(), format!("column(s) {} are dropped and their data will be lost", lost.join(", ")));
        }

        // Matched columns that change.
        let mut renames = Vec::new();
        let mut alters = Vec::new();
        let mut recompute: Vec<&Column> = Vec::new();
        for (a, b) in pairs {
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a.name != b.name {
                renames.push(format!("EXEC sp_rename N'{}.{}', N'{}', N'COLUMN'", table.replace('\'', "''"), quote(&b.name).replace('\'', "''"), a.name.replace('\'', "''")));
                disturbed = true;
            }
            let diffs = column_differences(a, b, o, false);
            if diffs.is_empty() {
                continue;
            }
            let only_default = diffs.iter().all(|d| d.name.starts_with("Default"));
            if only_default {
                if let Some(df) = &b.default {
                    self.drop_sub(Phase::DropDefaults, &name, &df.name, ddl::drop_constraint(&table, &df.name), "default");
                }
                if let Some(df) = &a.default {
                    after_defaults.push(ddl::add_default(&table, &a.name, df));
                }
                continue;
            }
            if a.computed.is_some() {
                // Non-persisted computed columns are dropped and added back.
                self.release_column(t, &b.name, false);
                drops.push(b.name.clone());
                recompute.push(a);
                continue;
            }
            self.release_column(t, &b.name, false);
            disturbed = true;
            for w in cast::data_loss(&b.data_type, &a.data_type) {
                self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), w);
            }
            if cast::classify(&b.data_type, &a.data_type) == Cast::Lossy {
                self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), format!("converting {} to {} may lose precision or fail", b.data_type.sql(), a.data_type.sql()));
            }
            let becomes_not_null = b.nullable && !a.nullable && !o.has(Opt::IgnoreNullability);
            let mut col: Column = (*a).clone();
            col.identity = None;
            col.default = None;
            if becomes_not_null {
                if let Some(df) = &a.default {
                    self.warn(Severity::Low, format!("{name}.{}", quote(&a.name)), "the column no longer allows NULLs; existing NULLs are set to the default");
                    col.nullable = true;
                    alters.push(format!("ALTER TABLE {table} ALTER COLUMN {}", ddl::column_definition(&col, false)));
                    after_defaults.push(format!("UPDATE {table} SET {} = {} WHERE {} IS NULL", quote(&a.name), df.definition, quote(&a.name)));
                    col.nullable = false;
                    after_defaults.push(format!("ALTER TABLE {table} ALTER COLUMN {}", ddl::column_definition(&col, false)));
                    if let Some(df) = &a.default {
                        after_defaults.insert(after_defaults.len() - 2, ddl::add_default(&table, &a.name, df));
                    }
                    continue;
                }
                self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), "the column no longer allows NULLs and has no default; the deployment fails if it holds NULLs");
            }
            if o.has(Opt::IgnoreNullability) {
                col.nullable = b.nullable;
            }
            let mut sql = format!("ALTER TABLE {table} ALTER COLUMN {}", ddl::column_definition(&col, false));
            if o.has(Opt::AddOnlineOn) {
                sql.push_str(" WITH (ONLINE = ON)");
            }
            alters.push(sql);
            if let Some(df) = &a.default {
                after_defaults.push(ddl::add_default(&table, &a.name, df));
            }
        }

        // Statement order inside one table change: drop columns, add columns, renames, alters, computed.
        if !drops.is_empty() {
            statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", drops.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")));
        }
        if !adds.is_empty() {
            let cols: Vec<String> = adds.iter().map(|c| ddl::column_definition(c, true)).collect();
            statements.push(format!("ALTER TABLE {table} ADD\n{}", cols.join(",\n")));
        }
        statements.extend(renames);
        statements.extend(alters);
        let computed: Vec<String> = computed_adds.iter().chain(recompute.iter()).map(|c| ddl::column_definition(c, false)).collect();
        if !computed.is_empty() {
            statements.push(format!("ALTER TABLE {table} ADD\n{}", computed.join(",\n")));
        }
        // Table options.
        if s.lock_escalation != t.lock_escalation && !s.lock_escalation.is_empty() {
            statements.push(format!("ALTER TABLE {table} SET (LOCK_ESCALATION = {})", s.lock_escalation));
        }
        if !o.has(Opt::IgnoreDataCompression) && s.storage.compression != t.storage.compression {
            let c = s.storage.compression.first().map(|p| p.compression.as_str()).unwrap_or("NONE");
            statements.push(format!("ALTER TABLE {table} REBUILD PARTITION = ALL WITH (DATA_COMPRESSION = {c})"));
        }
        if !o.has(Opt::IgnoreChangeTracking) && s.change_tracking != t.change_tracking {
            if t.change_tracking.is_some() {
                statements.insert(0, format!("ALTER TABLE {table} DISABLE CHANGE_TRACKING"));
            }
            if let Some(ct) = &s.change_tracking {
                statements.push(change_tracking_on(&name, ct));
            }
        }
        if disturbed {
            self.disturbed.insert(name.clone());
        }
        self.module_step(Phase::CreateObjects, format!("Altering {name}"), statements, &ModuleSettings::default());
        self.step(Phase::CreateDefaults, format!("Altering {name}"), after_defaults);
        self.sub_objects(s, t);
    }

    /// Indexes, checks, foreign keys and triggers of a table altered in place.
    fn sub_objects(&mut self, s: &'a Table, t: &'a Table) {
        let o = self.opts;
        let name = t.object_name();
        let table = name.quoted();
        let compare_opts = o.clone();
        let differs_ix = |a: &Index, b: &Index| {
            let mut x = Table { indexes: vec![a.clone()], ..Default::default() };
            x.schema = String::new();
            let y = Table { indexes: vec![b.clone()], ..Default::default() };
            let c = super::compare::compare(
                &Database { tables: vec![x], ..Default::default() },
                &Database { tables: vec![y], ..Default::default() },
                &compare_opts,
                &Mappings::default(),
            );
            !c.identical()
        };
        let ignored = |ix: &Index| {
            o.has(Opt::IgnoreIndexes) || (o.has(Opt::IgnorePerformanceIndexes) && !(ix.primary_key || ix.unique || ix.unique_constraint))
        };
        // Indexes: by name; a changed index is dropped and created again.
        let mut creates = Vec::new();
        for a in s.indexes.iter().filter(|i| !ignored(i)) {
            let b = t.indexes.iter().find(|b| self.same(&b.name, &a.name));
            let released = self.dropped_sub.contains(&(name.clone(), self.key(&a.name)));
            match b {
                Some(b) if !released && !differs_ix(a, b) => {}
                Some(b) => {
                    if !released {
                        self.release_index(t, b);
                    }
                    creates.push(a);
                }
                None => creates.push(a),
            }
        }
        for b in t.indexes.iter().filter(|i| !ignored(i)) {
            if !s.indexes.iter().any(|a| self.same(&a.name, &b.name)) {
                self.release_index(t, b);
            }
        }
        // Indexes dropped because a column changed come back too.
        for a in &s.indexes {
            if self.dropped_sub.contains(&(name.clone(), self.key(&a.name))) && !creates.iter().any(|c| c.name == a.name) {
                creates.push(a);
            }
        }
        creates.sort_by_key(|i| (!i.primary_key, i.kind != IndexKind::Clustered));
        let statements: Vec<String> = creates.iter().flat_map(|ix| ddl::create_index(&table, ix)).collect();
        self.step(Phase::CreateIndexes, format!("Creating indexes on {name}"), statements);

        // Check constraints.
        let ignore_nocheck = o.has(Opt::IgnoreNocheckAndWithNocheck);
        if !o.has(Opt::IgnoreChecks) {
            let mut adds = Vec::new();
            for a in &s.check_constraints {
                let b = t.check_constraints.iter().find(|b| self.same(&b.name, &a.name));
                let released = self.dropped_sub.contains(&(name.clone(), self.key(&a.name)));
                let same = b.is_some_and(|b| {
                    super::text::compare(&a.definition, &b.definition, super::text::TextKind::Expression, o).is_none()
                        && (ignore_nocheck || (a.disabled == b.disabled && (o.has(Opt::IgnoreWithNocheck) || a.not_trusted == b.not_trusted)))
                });
                if same && !released {
                    continue;
                }
                if let Some(b) = b {
                    self.drop_sub(Phase::DropChecks, &name, &b.name, ddl::drop_constraint(&table, &b.name), "constraint");
                }
                adds.extend(ddl::add_check(&table, a, ignore_nocheck));
            }
            for b in &t.check_constraints {
                if !s.check_constraints.iter().any(|a| self.same(&a.name, &b.name)) {
                    self.drop_sub(Phase::DropChecks, &name, &b.name, ddl::drop_constraint(&table, &b.name), "constraint");
                }
            }
            self.step(Phase::CreateChecks, format!("Adding constraints to {name}"), adds);
        }

        // Foreign keys.
        if !o.has(Opt::IgnoreKeys) {
            let ignore_with = o.has(Opt::IgnoreWithNocheck) || ignore_nocheck;
            for a in &s.foreign_keys {
                let b = t.foreign_keys.iter().find(|b| self.same(&b.name, &a.name));
                let released = self.dropped_sub.contains(&(name.clone(), self.key(&a.name)));
                let same = b.is_some_and(|b| {
                    a.columns.iter().map(|c| self.key(c)).eq(b.columns.iter().map(|c| self.key(c)))
                        && a.referenced_columns.iter().map(|c| self.key(c)).eq(b.referenced_columns.iter().map(|c| self.key(c)))
                        && self.key(&a.referenced_table.to_string()) == self.key(&b.referenced_table.to_string())
                        && a.on_delete == b.on_delete
                        && a.on_update == b.on_update
                        && (ignore_with || a.not_trusted == b.not_trusted)
                        && (ignore_nocheck || a.disabled == b.disabled)
                });
                if same && !released {
                    continue;
                }
                if let Some(b) = b {
                    self.drop_foreign_key(t, b);
                }
                if self.referenced_exists(&a.referenced_table) {
                    self.add_foreign_key(&name, a);
                }
            }
            for b in &t.foreign_keys {
                if !s.foreign_keys.iter().any(|a| self.same(&a.name, &b.name)) {
                    self.drop_foreign_key(t, b);
                }
            }
        }
        // Foreign keys of other tables dropped because of this table come back from the source.
        let tgt = self.tgt;
        for other in tgt.tables.iter().filter(|o| o.object_name() != name) {
            for fk in other.foreign_keys.iter().filter(|fk| fk.referenced_table == name) {
                if self.dropped_sub.contains(&(other.object_name(), self.key(&fk.name)))
                    && let Some(src_other) = self.src.table(&other.schema, &other.name)
                    && let Some(src_fk) = src_other.foreign_keys.iter().find(|f| self.same(&f.name, &fk.name))
                {
                    self.add_foreign_key(&other.object_name(), src_fk);
                }
            }
        }

        // Triggers.
        if !o.has(Opt::IgnoreTriggers) {
            for a in &s.triggers {
                match t.triggers.iter().find(|b| self.same(&b.name, &a.name)) {
                    None => self.create_trigger(&name, a),
                    Some(b) => {
                        let same_text = super::text::compare(&a.definition, &b.definition, super::text::TextKind::Module, &o.clone().with(Opt::IgnoreOwners)).is_none();
                        let trig = qname(&name.schema, &a.name);
                        let mut statements = Vec::new();
                        if !same_text {
                            statements.push(ddl::module_sql(&a.definition, "ALTER", &trig));
                        }
                        if a.disabled != b.disabled || !same_text {
                            statements.push(format!("{} TRIGGER {trig} ON {table}", if a.disabled { "DISABLE" } else { "ENABLE" }));
                        }
                        if a.order != b.order && !o.has(Opt::IgnoreTriggerOrder) {
                            for (event, order) in &a.order {
                                statements.push(format!("EXEC sp_settriggerorder N'{}', '{}', '{event}'", trig.replace('\'', "''"), title(order)));
                            }
                        }
                        if !statements.is_empty() {
                            self.module_step(Phase::Triggers, format!("Altering trigger {trig} on {name}"), statements, &ModuleSettings::default());
                        }
                    }
                }
            }
            for b in &t.triggers {
                if !s.triggers.iter().any(|a| self.same(&a.name, &b.name)) {
                    let trig = qname(&name.schema, &b.name);
                    self.step(Phase::DropTriggers, format!("Dropping trigger {trig}"), vec![format!("DROP TRIGGER {trig}")]);
                }
            }
        }
    }

    fn release_index(&mut self, t: &'a Table, ix: &Index) {
        let name = t.object_name();
        if ix.primary_key || ix.unique_constraint {
            let tgt = self.tgt;
            for other in &tgt.tables {
                for fk in other.foreign_keys.iter().filter(|fk| fk.referenced_table == name) {
                    self.drop_foreign_key(other, fk);
                }
            }
        }
        let phase = if ix.primary_key || ix.unique_constraint { Phase::DropKeys } else { Phase::DropIndexes };
        self.drop_sub(phase, &name, &ix.name, ddl::drop_index(&name.quoted(), ix), "index");
    }

    /// Creates the new table under a temporary name, copies the data, drops the old table and
    /// renames the new one; then keys, indexes, checks, foreign keys and triggers come back.
    fn rebuild_table(&mut self, s: &'a Table, t: &'a Table, pairs: &[(Option<&'a Column>, Option<&'a Column>)], reason: &str) {
        let name = s.object_name();
        let old = t.object_name();
        self.warn(Severity::Medium, name.to_string(), format!("the table is rebuilt because {reason}; its data is copied to the new table"));
        self.disturbed.insert(old.clone());
        self.recreate.insert(name.clone());
        // Everything attached to the old table goes with it; foreign keys from other tables are
        // dropped first and come back afterwards.
        for fk in &t.foreign_keys {
            self.drop_foreign_key(t, fk);
        }
        self.drop_inbound_foreign_keys(t, true);
        // Default constraint names are schema-wide, so the old ones are dropped before the new table takes them.
        for c in &t.columns {
            if let Some(df) = &c.default {
                self.drop_sub(Phase::DropDefaults, &old, &df.name, ddl::drop_constraint(&old.quoted(), &df.name), "default");
            }
        }
        // Keys and check constraints are schema-wide names too.
        for ix in t.indexes.iter().filter(|i| i.primary_key || i.unique_constraint) {
            self.drop_sub(Phase::DropKeys, &old, &ix.name, ddl::drop_index(&old.quoted(), ix), "index");
        }
        for ck in &t.check_constraints {
            self.drop_sub(Phase::DropChecks, &old, &ck.name, ddl::drop_constraint(&old.quoted(), &ck.name), "constraint");
        }
        let tmp_name = format!("QS_Rebuild_{}", s.name);
        let tmp = qname(&s.schema, &tmp_name);
        let mut temp_table = s.clone();
        temp_table.indexes.clear();
        temp_table.temporal = None;
        let mut statements = vec![ddl::create_table(&temp_table, Some(&tmp_name), false)];

        let mut into = Vec::new();
        let mut select = Vec::new();
        let mut identity = false;
        for (a, b) in pairs {
            match (a, b) {
                (Some(a), Some(b)) => {
                    if a.computed.is_some() || matches!(a.data_type.name.as_str(), "timestamp" | "rowversion") {
                        continue;
                    }
                    if b.computed.is_some() {
                        self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), "the column was computed and becomes a regular column; its values are not copied");
                        continue;
                    }
                    let mut expr = quote(&b.name);
                    match cast::classify(&b.data_type, &a.data_type) {
                        Cast::None => {
                            self.warn(
                                Severity::High,
                                format!("{name}.{}", quote(&a.name)),
                                format!("{} cannot be converted to {}; the column's data is not copied", b.data_type.sql(), a.data_type.sql()),
                            );
                            continue;
                        }
                        Cast::Explicit => {
                            self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), format!("values are converted with CAST from {} to {} and may fail or lose precision", b.data_type.sql(), a.data_type.sql()));
                            expr = format!("CAST({expr} AS {})", a.data_type.sql());
                        }
                        Cast::Lossy => {
                            self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), format!("converting {} to {} may lose precision or fail", b.data_type.sql(), a.data_type.sql()));
                        }
                        Cast::Implicit => {}
                    }
                    for w in cast::data_loss(&b.data_type, &a.data_type) {
                        self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), w);
                    }
                    if !a.nullable && b.nullable && a.identity.is_none()
                        && let Some(df) = &a.default
                    {
                        self.warn(Severity::High, format!("{name}.{}", quote(&a.name)), "NULL values are replaced by the column's default");
                        expr = format!("ISNULL({expr}, {})", df.definition);
                    }
                    identity |= a.identity.is_some();
                    into.push(quote(&a.name));
                    select.push(expr);
                }
                (None, Some(b)) if b.computed.is_none() => {
                    self.warn(Severity::High, format!("{old}.{}", quote(&b.name)), "the column has no place in the new table; its data will be lost");
                }
                _ => {}
            }
        }
        if !into.is_empty() {
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {tmp} ON"));
            }
            statements.push(format!("INSERT INTO {tmp} ({}) SELECT {} FROM {}", into.join(", "), select.join(", "), old.quoted()));
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {tmp} OFF"));
            }
        }
        let identity_col = s.columns.iter().find(|c| c.identity.is_some());
        if let Some(c) = identity_col
            && t.columns.iter().any(|tc| tc.identity.is_some() && self.same(&tc.name, &c.name))
        {
            statements.push(format!(
                "DECLARE @idVal BIGINT\nSELECT @idVal = IDENT_CURRENT(N'{}')\nIF @idVal IS NOT NULL\n    DBCC CHECKIDENT(N'{}', RESEED, @idVal)",
                old.quoted().replace('\'', "''"),
                tmp.replace('\'', "''")
            ));
        }
        statements.push(format!("DROP TABLE {}", old.quoted()));
        statements.push(format!("EXEC sp_rename N'{}', N'{}', N'OBJECT'", tmp.replace('\'', "''"), s.name.replace('\'', "''")));
        if s.lock_escalation != "TABLE" && !s.lock_escalation.is_empty() {
            statements.push(format!("ALTER TABLE {} SET (LOCK_ESCALATION = {})", name.quoted(), s.lock_escalation));
        }
        if let Some(ct) = &s.change_tracking {
            statements.push(change_tracking_on(&name, ct));
        }
        self.module_step(Phase::CreateObjects, format!("Rebuilding {name}"), statements, &ModuleSettings::default());
        self.table_children(s, None);
    }

    // ---- Extended properties ----

    fn extended_properties(&mut self, selected: &[&Difference]) {
        if self.opts.has(Opt::IgnoreExtendedProperties) {
            return;
        }
        for d in selected {
            let class = match d.object_type {
                ObjectType::Role | ObjectType::User => continue,
                ObjectType::Table => "TABLE",
                ObjectType::View => "VIEW",
                ObjectType::Function => "FUNCTION",
                ObjectType::StoredProcedure => "PROCEDURE",
                ObjectType::Sequence => "SEQUENCE",
                ObjectType::Synonym => "SYNONYM",
                ObjectType::UserDefinedType => "TYPE",
                ObjectType::Schema => "SCHEMA",
                ObjectType::ExtendedProperty => continue,
            };
            let tn = self.target_name(d);
            let of = |db: &'a Database, n: &ObjectName| -> Vec<&'a ExtendedProperty> {
                db.extended_properties
                    .iter()
                    .filter(|p| {
                        p.class == class
                            && if class == "SCHEMA" {
                                p.schema.as_deref() == Some(n.name.as_str())
                            } else {
                                p.schema.as_deref() == Some(n.schema.as_str()) && p.object.as_deref() == Some(n.name.as_str())
                            }
                    })
                    .collect()
            };
            let src_props = if d.kind == DiffKind::OnlyIn2 { Vec::new() } else { of(self.src, &d.name) };
            // A rebuilt or re-created object lost its properties with it.
            let gone = d.kind == DiffKind::OnlyIn2 || self.recreate.contains(&d.name) || self.dropped.contains(&tn);
            let tgt_props = if d.kind == DiffKind::OnlyIn1 || gone { Vec::new() } else { of(self.tgt, &tn) };
            let levels = |p: &ExtendedProperty| ddl::xprop_levels(p, |s| s.to_string());
            for sp in &src_props {
                let existing = tgt_props.iter().find(|tp| tp.name == sp.name && tp.minor_type == sp.minor_type && tp.minor_name == sp.minor_name);
                match existing {
                    None => self.step(Phase::ExtendedProperties, "Adding extended properties", vec![ddl::add_xprop(sp, &levels(sp))]),
                    Some(tp) if tp.value != sp.value => {
                        self.step(Phase::ExtendedProperties, "Altering extended properties", vec![ddl::update_xprop(sp, &levels(sp))])
                    }
                    _ => {}
                }
            }
            if d.kind != DiffKind::OnlyIn2 {
                for tp in &tgt_props {
                    if !src_props.iter().any(|sp| sp.name == tp.name && sp.minor_type == tp.minor_type && sp.minor_name == tp.minor_name) {
                        self.step(Phase::DropExtendedProperties, "Dropping extended properties", vec![ddl::drop_xprop(tp, &levels(tp))]);
                    }
                }
            }
        }
    }

    // ---- Users, roles and permissions ----

    fn principal(&mut self, d: &Difference) {
        let find = |db: &'a Database, n: &str| db.principals.iter().find(|p| p.name == n);
        let tn = self.target_name(d);
        let (s, t) = (find(self.src, &d.name.name), find(self.tgt, &tn.name));
        let q = quote(&d.name.name);
        if let Some(s) = s
            && t.is_none()
        {
            let mut sql = Vec::new();
            match s.kind.as_str() {
                "DATABASE_ROLE" => {
                    let owner = s.owner.as_deref().filter(|o| *o != "dbo").map(|o| format!(" AUTHORIZATION {}", quote(o))).unwrap_or_default();
                    sql.push(format!("CREATE ROLE {q}{owner}"));
                }
                "SQL_USER" | "WINDOWS_USER" | "WINDOWS_GROUP" => {
                    let schema = s.default_schema.as_deref().filter(|x| *x != "dbo").map(|x| format!(" WITH DEFAULT_SCHEMA = {}", quote(x))).unwrap_or_default();
                    match &s.login {
                        Some(login) => {
                            self.warn(Severity::Low, q.clone(), format!("the login {login} must exist on the target server"));
                            sql.push(format!("CREATE USER {q} FOR LOGIN {}{schema}", quote(login)));
                        }
                        None => sql.push(format!("CREATE USER {q} WITHOUT LOGIN{schema}")),
                    }
                }
                "EXTERNAL_USER" | "EXTERNAL_GROUP" => {
                    let schema = s.default_schema.as_deref().filter(|x| *x != "dbo").map(|x| format!(" WITH DEFAULT_SCHEMA = {}", quote(x))).unwrap_or_default();
                    sql.push(format!("CREATE USER {q} FROM EXTERNAL PROVIDER{schema}"));
                }
                other => {
                    self.warn(Severity::High, q.clone(), format!("{other} principals are not scripted yet; create it by hand"));
                    return;
                }
            }
            self.step(Phase::CreatePrincipals, format!("Creating {q}"), sql);
            for r in &s.member_of {
                self.step(Phase::RoleMembers, format!("Adding {q} to role {}", quote(r)), vec![format!("ALTER ROLE {} ADD MEMBER {q}", quote(r))]);
            }
            self.database_permissions(&s.name, &[]);
        } else if let Some(t) = t
            && s.is_none()
        {
            let tq = quote(&t.name);
            self.warn(Severity::Low, tq.clone(), "the principal is dropped; objects it owns must be given to another owner first");
            let mut sql: Vec<String> = Vec::new();
            if t.is_role() {
                for m in self.tgt.principals.iter().filter(|m| m.member_of.contains(&t.name)) {
                    sql.push(format!("ALTER ROLE {tq} DROP MEMBER {}", quote(&m.name)));
                }
                sql.push(format!("DROP ROLE {tq}"));
            } else {
                sql.push(format!("DROP USER {tq}"));
            }
            self.step(Phase::DropPrincipals, format!("Dropping {tq}"), sql);
        } else if let (Some(s), Some(t)) = (s, t) {
            let mut sql = Vec::new();
            if !s.is_role() && !self.opts.has(Opt::IgnoreUserProperties) {
                let mut with = Vec::new();
                if s.default_schema != t.default_schema
                    && let Some(x) = &s.default_schema
                {
                    with.push(format!("DEFAULT_SCHEMA = {}", quote(x)));
                }
                if s.login != t.login
                    && let Some(l) = &s.login
                {
                    with.push(format!("LOGIN = {}", quote(l)));
                }
                if !with.is_empty() {
                    sql.push(format!("ALTER USER {q} WITH {}", with.join(", ")));
                }
            }
            if s.is_role() && s.owner != t.owner && !self.opts.has(Opt::IgnoreOwners) {
                sql.push(format!("ALTER AUTHORIZATION ON ROLE::{q} TO {}", quote(s.owner.as_deref().unwrap_or("dbo"))));
            }
            self.step(Phase::CreatePrincipals, format!("Altering {q}"), sql);
            let added: Vec<&String> = s.member_of.iter().filter(|r| !t.member_of.iter().any(|x| self.same(x, r))).collect();
            let removed: Vec<&String> = t.member_of.iter().filter(|r| !s.member_of.iter().any(|x| self.same(x, r))).collect();
            for r in added {
                self.step(Phase::RoleMembers, format!("Adding {q} to role {}", quote(r)), vec![format!("ALTER ROLE {} ADD MEMBER {q}", quote(r))]);
            }
            for r in removed {
                self.step(Phase::RoleMembers, format!("Removing {q} from role {}", quote(r)), vec![format!("ALTER ROLE {} DROP MEMBER {q}", quote(r))]);
            }
            let existing: Vec<Permission> = self.tgt.permissions.iter().filter(|p| p.class == "DATABASE" && self.same(&p.grantee, &t.name)).cloned().collect();
            self.database_permissions(&s.name, &existing);
        }
    }

    /// Database-level GRANT/DENY of one principal: source's minus what the target has.
    fn database_permissions(&mut self, grantee: &str, existing: &[Permission]) {
        if self.opts.has(Opt::IgnorePermissions) {
            return;
        }
        let wanted: Vec<Permission> = self.src.permissions.iter().filter(|p| p.class == "DATABASE" && self.same(&p.grantee, grantee)).cloned().collect();
        let key = |p: &Permission| self.key(&format!("{} {}", p.state, p.permission));
        let grants: Vec<String> = wanted.iter().filter(|w| !existing.iter().any(|e| key(e) == key(w))).map(Permission::grant_sql).collect();
        let revokes: Vec<String> = existing.iter().filter(|e| !wanted.iter().any(|w| key(w) == key(e))).map(Permission::revoke_sql).collect();
        self.step(Phase::Permissions, "Altering permissions", revokes.into_iter().chain(grants).collect());
    }

    /// GRANT, DENY and REVOKE for the securables of the selected differences. Objects that are
    /// created, rebuilt or re-created have no permissions yet, so all of the source's are granted
    /// (or, with IgnorePermissions, the target's are restored).
    fn permissions(&mut self, selected: &[&Difference]) {
        let ignore = self.opts.has(Opt::IgnorePermissions);
        let roles_only = self.opts.has(Opt::IgnoreUsers);
        for d in selected {
            let (class, object) = match d.object_type {
                ObjectType::Table | ObjectType::View | ObjectType::Function | ObjectType::StoredProcedure | ObjectType::Sequence | ObjectType::Synonym => ("OBJECT", true),
                ObjectType::Schema => ("SCHEMA", false),
                _ => continue,
            };
            if d.kind == DiffKind::OnlyIn2 {
                continue;
            }
            let tn = self.target_name(d);
            let on = |db: &'a Database, n: &ObjectName| -> Vec<Permission> {
                let (schema, obj) = if object { (n.schema.as_str(), Some(n.name.as_str())) } else { (n.name.as_str(), None) };
                db.permissions
                    .iter()
                    .filter(|p| {
                        p.class == class
                            && p.schema.as_deref() == Some(schema)
                            && p.object.as_deref() == obj
                            && (!roles_only || p.grantee == "public" || db.principals.iter().any(|x| x.is_role() && x.name == p.grantee))
                    })
                    .cloned()
                    .collect()
            };
            let fresh = d.kind == DiffKind::OnlyIn1 || self.recreate.contains(&d.name) || self.dropped.contains(&tn);
            let existing = if fresh { Vec::new() } else { on(self.tgt, &tn) };
            let wanted = if ignore {
                if fresh && d.kind != DiffKind::OnlyIn1 { on(self.tgt, &tn) } else { continue }
            } else {
                on(self.src, &d.name)
            };
            // Statements name the object as it is called after the deployment.
            let retarget = |p: &Permission| {
                let mut p = p.clone();
                if object {
                    p.schema = Some(d.name.schema.clone());
                    p.object = Some(d.name.name.clone());
                } else {
                    p.schema = Some(d.name.name.clone());
                }
                p
            };
            let key = |p: &Permission| self.key(&format!("{} {} {}", p.state, p.permission, p.grantee));
            let revokes: Vec<String> =
                existing.iter().filter(|e| !wanted.iter().any(|w| key(w) == key(e))).map(|e| retarget(e).revoke_sql()).collect();
            let grants: Vec<String> =
                wanted.iter().filter(|w| !existing.iter().any(|e| key(e) == key(w))).map(|w| retarget(w).grant_sql()).collect();
            let label = if object { d.name.to_string() } else { quote(&d.name.name) };
            self.step(Phase::Permissions, format!("Altering permissions on {label}"), revokes.into_iter().chain(grants).collect());
        }
    }

    // ---- Ordering and output ----

    /// Ranks steps of the object phases by dependency order.
    fn sort(&mut self) {
        let order_src = topo_order(&self.src.dependencies);
        let order_tgt = topo_order(&self.tgt.dependencies);
        for step in &mut self.steps {
            let Some(name) = step_object(&step.print) else { continue };
            step.rank = match step.phase {
                Phase::CreateObjects => *order_src.get(&name).unwrap_or(&usize::MAX),
                // Dependants are dropped first.
                Phase::DropObjects => usize::MAX - order_tgt.get(&name).copied().unwrap_or(0),
                _ => 0,
            };
        }
        // Stable: keeps planning order within equal ranks.
        self.steps.sort_by_key(|s| (s.phase, s.rank));
    }

    fn emit(self) -> DeploymentScript {
        let o = self.opts;
        let transactions = !o.has(Opt::NoTransactions) && !o.has(Opt::AssumeAlreadyInTransaction);
        let errors = !o.has(Opt::NoErrorHandling);
        let mut w = Writer::new(errors);
        let ignore_set = o.has(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings);
        if !o.has(Opt::DoNotOutputCommentHeader) {
            w.comment(format!(
                "/*\nRun this script on:\n\n        {}    -  This database will be modified\n\nto synchronize it with:\n\n        {}\n\nYou are recommended to back up your database before running this script\n\nScript created by QuillSQL {} at {} UTC\n\n*/",
                self.settings.target,
                self.settings.source,
                env!("CARGO_PKG_VERSION"),
                utc_now()
            ));
        }
        w.batch("SET NUMERIC_ROUNDABORT OFF".into());
        w.batch(if ignore_set {
            "SET ANSI_PADDING, ANSI_WARNINGS, CONCAT_NULL_YIELDS_NULL, ARITHABORT ON".into()
        } else {
            "SET ANSI_PADDING, ANSI_WARNINGS, CONCAT_NULL_YIELDS_NULL, ARITHABORT, QUOTED_IDENTIFIER, ANSI_NULLS ON".into()
        });
        w.set_state("QUOTED_IDENTIFIER", true);
        w.set_state("ANSI_NULLS", true);
        if o.has(Opt::AddDatabaseUseStatement) && !self.settings.target_database.is_empty() {
            w.batch(format!("USE {}", quote(&self.settings.target_database)));
        }
        if transactions {
            w.batch("SET XACT_ABORT ON".into());
            w.batch(format!("SET TRANSACTION ISOLATION LEVEL {}", self.settings.transaction_isolation_level));
            w.batch("BEGIN TRANSACTION".into());
        }
        let body_start = w.batches.len();
        for step in &self.steps {
            if !step.print.is_empty() {
                w.error_check();
                w.batch(format!("PRINT {}", literal(&step.print)));
            }
            if let Some(s) = &step.settings
                && !ignore_set
            {
                w.set("QUOTED_IDENTIFIER", s.quoted_identifier);
                w.set("ANSI_NULLS", s.ansi_nulls);
            }
            for sql in &step.statements {
                w.statement(sql.clone());
                w.error_check();
            }
        }
        let empty = w.batches.len() == body_start;
        if transactions {
            w.batch("COMMIT TRANSACTION".into());
            w.error_check();
        }
        if errors {
            let rollback = if transactions { "\tIF @@TRANCOUNT > 0 ROLLBACK TRANSACTION\n" } else { "" };
            w.batch(format!(
                "DECLARE @Success AS BIT\nSET @Success = 1\nSET NOEXEC OFF\nIF (@Success = 1) PRINT 'The database update succeeded'\nELSE BEGIN\n{rollback}\tPRINT 'The database update failed'\nEND"
            ));
        }
        let mut warnings = self.warnings;
        warnings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.object.cmp(&b.object)));
        warnings.dedup();
        if o.has(Opt::NoTransactions) {
            warnings.push(Warning {
                severity: Severity::High,
                object: String::new(),
                message: "the script does not use a transaction; back up the database first, a failure leaves it partly changed".into(),
            });
        }
        DeploymentScript { sql: w.text, warnings, batches: if empty { Vec::new() } else { w.batches } }
    }
}

#[derive(Clone, Copy)]
enum Module<'a> {
    View(&'a View),
    Routine(&'a Routine),
}

impl<'a> Module<'a> {
    fn definition(&self) -> &'a str {
        match self {
            Module::View(v) => &v.definition,
            Module::Routine(r) => &r.definition,
        }
    }

    fn settings(&self) -> &'a ModuleSettings {
        match self {
            Module::View(v) => &v.settings,
            Module::Routine(r) => &r.settings,
        }
    }

    fn encrypted(&self) -> bool {
        match self {
            Module::View(v) => v.encrypted,
            Module::Routine(r) => r.encrypted,
        }
    }

    fn is_clr(&self) -> bool {
        matches!(self, Module::Routine(r) if r.clr_method.is_some())
    }
}

/// Scalar, table-valued and procedures can't be ALTERed into one another.
fn function_class(r: &Routine) -> u8 {
    match r.kind {
        RoutineKind::Procedure | RoutineKind::ClrProcedure => 0,
        RoutineKind::ScalarFunction | RoutineKind::ClrScalarFunction => 1,
        RoutineKind::InlineTableFunction => 2,
        RoutineKind::TableFunction | RoutineKind::ClrTableFunction => 3,
        RoutineKind::ClrAggregate => 4,
    }
}

fn change_tracking_on(name: &ObjectName, ct: &ChangeTracking) -> String {
    format!(
        "ALTER TABLE {} ENABLE CHANGE_TRACKING{}",
        name.quoted(),
        if ct.track_columns_updated { " WITH (TRACK_COLUMNS_UPDATED = ON)" } else { "" }
    )
}

fn title(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// The object named in a step's PRINT text ("Creating [s].[t]" → s.t).
fn step_object(print: &str) -> Option<ObjectName> {
    let rest = ["Creating ", "Altering ", "Rebuilding ", "Dropping "].iter().find_map(|p| print.strip_prefix(p))?;
    let parts = parse_quoted_name(rest)?;
    Some(parts)
}

fn parse_quoted_name(s: &str) -> Option<ObjectName> {
    // One bracketed part, with ]] escapes; returns the name and the rest.
    fn part(s: &str) -> Option<(String, &str)> {
        let mut chars = s.strip_prefix('[')?.char_indices().peekable();
        let mut out = String::new();
        while let Some((i, c)) = chars.next() {
            if c == ']' {
                if chars.peek().is_some_and(|&(_, n)| n == ']') {
                    chars.next();
                    out.push(']');
                } else {
                    return Some((out, &s[i + 2..]));
                }
            } else {
                out.push(c);
            }
        }
        None
    }
    let (schema, rest) = part(s.trim())?;
    let (name, rest) = part(rest.strip_prefix('.')?)?;
    rest.trim().is_empty().then(|| ObjectName::new(&schema, &name))
}

/// Position of each object in an order where dependencies come before their dependants
/// (Tarjan's algorithm; members of a cycle share neighbouring positions).
fn topo_order(deps: &[Dependency]) -> HashMap<ObjectName, usize> {
    let mut graph: BTreeMap<&ObjectName, Vec<&ObjectName>> = BTreeMap::new();
    for d in deps {
        graph.entry(&d.from).or_default().push(&d.to);
        graph.entry(&d.to).or_default();
    }
    let mut order = HashMap::new();
    let mut visiting: HashSet<&ObjectName> = HashSet::new();
    fn visit<'g>(
        n: &'g ObjectName,
        graph: &BTreeMap<&'g ObjectName, Vec<&'g ObjectName>>,
        visiting: &mut HashSet<&'g ObjectName>,
        order: &mut HashMap<ObjectName, usize>,
    ) {
        if order.contains_key(n) || !visiting.insert(n) {
            return;
        }
        for m in graph.get(n).map(Vec::as_slice).unwrap_or(&[]) {
            visit(m, graph, visiting, order);
        }
        let next = order.len();
        order.insert(n.clone(), next);
    }
    for n in graph.keys() {
        visit(n, &graph, &mut visiting, &mut order);
    }
    order
}

/// Builds the script text and the batch list, removing repeated SETs and error checks.
struct Writer {
    text: String,
    batches: Vec<String>,
    errors: bool,
    last_was_check: bool,
    state: HashMap<&'static str, bool>,
    pending_comment: Option<String>,
}

const ERROR_CHECK: &str = "IF @@ERROR <> 0 SET NOEXEC ON";

impl Writer {
    fn new(errors: bool) -> Self {
        Writer { text: String::new(), batches: Vec::new(), errors, last_was_check: true, state: HashMap::new(), pending_comment: None }
    }

    fn comment(&mut self, c: String) {
        self.pending_comment = Some(c);
    }

    fn batch(&mut self, sql: String) {
        let mut out = String::new();
        if let Some(c) = self.pending_comment.take() {
            out.push_str(&c);
            out.push('\n');
        }
        out.push_str(&sql);
        self.text.push_str(&out.replace("\r\n", "\n").replace('\n', "\r\n"));
        self.text.push_str("\r\nGO\r\n");
        self.batches.push(sql);
        self.last_was_check = false;
    }

    fn statement(&mut self, sql: String) {
        self.batch(sql);
    }

    fn error_check(&mut self) {
        if self.errors && !self.last_was_check && !self.batches.is_empty() {
            self.batch(ERROR_CHECK.into());
            self.last_was_check = true;
        }
    }

    fn set_state(&mut self, option: &'static str, on: bool) {
        self.state.insert(option, on);
    }

    fn set(&mut self, option: &'static str, on: bool) {
        if self.state.get(option) == Some(&on) {
            return;
        }
        self.state.insert(option, on);
        self.batch(format!("SET {option} {}", if on { "ON" } else { "OFF" }));
    }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC without a date library.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Runs a deployment script's batches on the target, stopping at the first error and rolling
/// back whatever transaction is open. Returns how many batches ran.
pub fn execute(spec: &crate::db::ConnectionSpec, script: &DeploymentScript, progress: impl Fn(usize, usize)) -> Result<usize, String> {
    let rt = crate::db::runtime()?;
    rt.block_on(async {
        let mut conn = crate::db::connect_async(spec).await?;
        let total = script.batches.len();
        for (i, batch) in script.batches.iter().enumerate() {
            progress(i, total);
            if let Err(e) = crate::db::execute(&mut conn, batch).await {
                let _ = crate::db::execute(&mut conn, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
                let first_line = batch.lines().next().unwrap_or_default();
                return Err(format!("batch {} of {total} failed and the deployment was rolled back: {e}\n  in: {first_line}", i + 1));
            }
        }
        Ok(total)
    })
}

/// Splits a script on GO lines into batches (for running a saved script).
pub fn split_batches(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in script.lines() {
        if line.trim().eq_ignore_ascii_case("GO") {
            if !cur.trim().is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_prints() {
        assert_eq!(step_object("Creating [dbo].[T]"), Some(ObjectName::new("dbo", "T")));
        assert_eq!(step_object("Altering [a]]b].[c.d]"), Some(ObjectName::new("a]b", "c.d")));
        assert_eq!(step_object("Adding constraints to [dbo].[T]"), None);
        assert_eq!(step_object("Creating trigger [dbo].[tr] on [dbo].[T]"), None);
    }

    #[test]
    fn topological() {
        let d = |a: &str, b: &str| Dependency { from: ObjectName::new("dbo", a), to: ObjectName::new("dbo", b), strong: true };
        let order = topo_order(&[d("v2", "v1"), d("v1", "t"), d("p", "v2")]);
        let pos = |n: &str| order[&ObjectName::new("dbo", n)];
        assert!(pos("t") < pos("v1") && pos("v1") < pos("v2") && pos("v2") < pos("p"));
        // A cycle does not loop forever.
        let order = topo_order(&[d("a", "b"), d("b", "a")]);
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn utc_format() {
        assert_eq!(utc_now().len(), 19);
    }
}
