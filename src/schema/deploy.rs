//! Turns the selected differences into a deployment script that makes database 2 (the
//! target) match database 1 (the source).
//!
//! Planning puts every statement into a phase; phases run in a fixed order (drops of
//! dependants first, then creates and alters in dependency order, then keys, triggers and
//! extended properties), and objects inside the create and drop phases are sorted by the
//! dependency graph of the source and target respectively. The layout, PRINT texts and
//! statement shapes follow SQL Compare's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::cast::{self, Cast};
use super::compare::{Comparison, DiffKind, Difference, Mappings, ObjectType, column_differences};
use super::ddl::{self, Host, Render, type_sql};
use super::ddl_objects;
use super::exists::{Check, Checks};
use super::features::{Feature, Features};
use super::options::{Opt, Options};
use super::warnings as warn;
use crate::catalog::*;
use crate::db::{literal, quote};

mod objects;
mod unbind;

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

/// When a statement runs. The order of the variants is the order of the script; the first
/// runs before the transaction and the last after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Phase {
    /// Certificates and keys: created and altered before anything can use them.
    PreCrypto,
    /// Full-text work, which can't run in a transaction (ranked inside).
    PreFullText,
    PreTransaction,
    DisableDdlTriggers,
    CreatePrincipals,
    RoleMembers,
    /// External tables, data sources and file formats (ranked in that order).
    DropExternal,
    DropSensitivity,
    CreateSchemas,
    /// External file formats, then data sources.
    CreateExternalFormats,
    DisableChangeTracking,
    DropExtendedProperties,
    DropForeignKeys,
    /// XML and spatial indexes (secondary XML first).
    DropXmlIndexes,
    DropChecks,
    DropKeys,
    DropDefaults,
    /// Other indexes, then index enable/disable.
    DropIndexes,
    DropTriggers,
    UnbindColumns,
    /// Alias types taken off the columns that use them while the type is re-created.
    UnbindTypes,
    DropDdlTriggers,
    /// Views, functions, procedures, tables, Service Broker objects and security policies,
    /// dependants first.
    DropObjects,
    /// The first rebuild of tables that use a re-created CLR type or partition scheme.
    UnbindRebuild,
    DropSequences,
    /// Rules, then defaults (CREATE RULE / CREATE DEFAULT objects).
    DropBindables,
    DropTypes,
    DropAssemblies,
    DropCryptoKeys,
    /// Partition schemes and functions: drops, creates and alters (ranked).
    Partitioning,
    /// Defaults, then rules.
    CreateBindables,
    DropRoutes,
    /// Assembly creates, then alters.
    CreateAssemblies,
    DropSynonyms,
    /// Typed xml columns taken off a collection that is re-created.
    UnbindXml,
    /// Drops, then creates.
    XmlSchemaCollections,
    CreateTypes,
    RebindTypes,
    CreateSequences,
    CreateExternalTables,
    /// Tables (with their indexes), views, functions, procedures, synonyms, Service Broker
    /// objects and security policies, dependencies first.
    CreateObjects,
    /// SET (SYSTEM_VERSIONING = ON …) once the table has its primary key again.
    Versioning,
    BindColumns,
    CreateChecks,
    CreateForeignKeys,
    CreateDefaults,
    RebindXml,
    Triggers,
    DdlTriggers,
    /// Alters, then creates.
    Routes,
    ExtendedProperties,
    Permissions,
    DropSchemas,
    EnableDdlTriggers,
    AddSensitivity,
    /// Full-text indexes are created after the transaction.
    PostFullText,
    PostTransaction,
    DropCertificates,
}

impl Phase {
    fn before_transaction(self) -> bool {
        self <= Phase::PreTransaction
    }

    fn after_transaction(self) -> bool {
        self >= Phase::PostFullText
    }
}

#[derive(Debug)]
struct Step {
    phase: Phase,
    /// Position inside a phase: dependency order for objects, else as planned.
    rank: usize,
    print: String,
    statements: Vec<String>,
    /// ANSI_NULLS and QUOTED_IDENTIFIER the statements need (modules).
    settings: Option<ModuleSettings>,
    /// The object the step ranks with; parsed from the PRINT text when not given.
    object: Option<ObjectName>,
    /// Later steps with the same phase and PRINT join this one (one PRINT for the group).
    group: bool,
    /// Each statement runs in SQL Compare's TRY/CATCH.
    try_catch: bool,
}

impl Step {
    fn new(phase: Phase, print: impl Into<String>, statements: Vec<String>) -> Step {
        Step { phase, rank: 0, print: print.into(), statements, settings: None, object: None, group: false, try_catch: false }
    }
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
    let prepared = unbind::prepare(comparison, source, target, opts);
    let mut p = Planner::new(comparison, source, target, opts, mappings, settings);
    p.prepared = &prepared;
    p.plan();
    Ok(p.emit())
}

/// A leftover table of an earlier rebuild that failed half way: `RG_Recovery_<n>_…` (and
/// QuillSQL's earlier `QS_Rebuild_` prefix). Returns n, 0 for the old prefix.
fn recovery_number(table: &str) -> Option<usize> {
    if table.starts_with("QS_Rebuild_") && table.len() > "QS_Rebuild_".len() {
        return Some(0);
    }
    let rest = table.strip_prefix("RG_Recovery_")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let tail = &rest[digits.len()..];
    (!digits.is_empty() && tail.len() > 1 && tail.starts_with('_')).then(|| digits.parse().unwrap_or(0))
}

struct Planner<'a> {
    cmp: &'a Comparison,
    src: &'a Database,
    tgt: &'a Database,
    opts: &'a Options,
    mappings: &'a Mappings,
    settings: &'a ScriptSettings,
    r: Render,
    ex: Checks,
    /// Dependencies of each side (without synonym ones under IgnoreSynonymDependencies).
    src_deps: Vec<Dependency>,
    tgt_deps: Vec<Dependency>,
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
    /// Column bindings already removed: (table, column, rule).
    unbound: HashSet<(ObjectName, String, bool)>,
    case_sensitive: bool,
    /// Number of the next rebuild's temporary table.
    next_temp: usize,
    /// Memory-optimized objects are created or rebuilt.
    memory_optimized: bool,
    selective_xml: bool,
    table_filegroups: BTreeSet<String>,
    object_filegroups: BTreeSet<String>,
    objects: objects::State,
    prepared: &'a unbind::Prepared,
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
        let features = Features::of(&tgt.server, settings.target_major_version, tgt.compatibility_level, opts.has(Opt::UseCompatibilityLevel));
        let deps = |db: &Database| -> Vec<Dependency> {
            if !opts.has(Opt::IgnoreSynonymDependencies) {
                return db.dependencies.clone();
            }
            let synonyms: HashSet<ObjectName> = db.synonyms.iter().map(|s| ObjectName::new(&s.schema, &s.name)).collect();
            db.dependencies.iter().filter(|d| !synonyms.contains(&d.from)).cloned().collect()
        };
        let mut warnings = Vec::new();
        let mut next_temp = 1;
        for t in src.tables.iter().chain(&tgt.tables) {
            if let Some(n) = recovery_number(&t.name) {
                warnings.push(warn::recovered_table(&t.object_name().to_string()));
                next_temp = next_temp.max(n + 1);
            }
        }
        Planner {
            cmp,
            src,
            tgt,
            opts,
            mappings,
            settings,
            r: Render { opts: opts.clone(), features },
            ex: Checks::new(opts),
            src_deps: deps(src),
            tgt_deps: deps(tgt),
            steps: Vec::new(),
            warnings,
            disturbed: BTreeSet::new(),
            dropped: HashSet::new(),
            dropped_sub: HashSet::new(),
            recreate: BTreeSet::new(),
            added_fks: HashSet::new(),
            unbound: HashSet::new(),
            case_sensitive: opts.has(Opt::CaseSensitiveObjectDefinition),
            next_temp,
            memory_optimized: false,
            selective_xml: false,
            table_filegroups: BTreeSet::new(),
            object_filegroups: BTreeSet::new(),
            objects: objects::State::default(),
            prepared: &unbind::NOTHING,
        }
    }

    fn key(&self, s: &str) -> String {
        if self.case_sensitive { s.to_string() } else { s.to_lowercase() }
    }

    fn same(&self, a: &str, b: &str) -> bool {
        self.key(a) == self.key(b)
    }

    fn has(&self, f: Feature) -> bool {
        self.r.features.has(f)
    }

    fn warn(&mut self, w: Warning) {
        self.warnings.push(w);
    }

    fn push(&mut self, step: Step) {
        if !step.statements.is_empty() {
            self.steps.push(step);
        }
    }

    fn step(&mut self, phase: Phase, print: impl Into<String>, statements: Vec<String>) {
        self.push(Step::new(phase, print, statements));
    }

    /// A step whose PRINT is shared by everything of its kind (or of one table).
    fn group(&mut self, phase: Phase, print: impl Into<String>, statements: Vec<String>) {
        self.push(Step { group: true, ..Step::new(phase, print, statements) });
    }

    /// Ranks with `object` in a dependency-sorted phase.
    fn ranked(&mut self, phase: Phase, print: impl Into<String>, object: &ObjectName, statements: Vec<String>) {
        self.push(Step { object: Some(object.clone()), ..Step::new(phase, print, statements) });
    }

    fn module_step(&mut self, phase: Phase, print: impl Into<String>, statements: Vec<String>, settings: &ModuleSettings) {
        self.push(Step { settings: Some(settings.clone()), ..Step::new(phase, print, statements) });
    }

    fn selected(&self) -> Vec<&'a Difference> {
        let mut chosen: Vec<&Difference> = self.cmp.differences.iter().filter(|d| d.selected && d.kind != DiffKind::Equal).collect();
        if self.opts.has(Opt::IncludeDependencies) {
            // Objects that selected creates and alters need, and that differ, come along.
            let mut names: HashSet<ObjectName> = chosen.iter().map(|d| d.key()).collect();
            let mut queue: Vec<ObjectName> = chosen.iter().filter(|d| d.kind != DiffKind::OnlyIn2).map(|d| d.key()).collect();
            while let Some(n) = queue.pop() {
                for dep in self.src_deps.iter().filter(|d| d.from == n) {
                    if names.contains(&dep.to) {
                        continue;
                    }
                    if let Some(d) = self.cmp.differences.iter().find(|d| {
                        d.key() == dep.to && matches!(d.kind, DiffKind::OnlyIn1 | DiffKind::Different)
                    }) {
                        names.insert(d.key());
                        queue.push(d.key());
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
        self.ddl_triggers();
        self.unbind_by_rebuild();
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
                _ => self.more_object(d),
            }
        }
        self.finish_objects();
        self.dependants(&selected);
        self.extended_properties(&selected);
        self.permissions(&selected);
        self.deployment_warnings();
        self.sort();
    }

    // ---- Script-wide parts ----

    /// DisableAndReenableDdlTriggers: database DDL triggers are switched off for the deployment.
    fn ddl_triggers(&mut self) {
        if !self.opts.has(Opt::DisableAndReenableDdlTriggers) || self.tgt.ddl_triggers.is_empty() {
            return;
        }
        self.step(Phase::DisableDdlTriggers, "Disabling DDL triggers", vec!["DISABLE TRIGGER ALL ON DATABASE".into()]);
        let enable: Vec<String> = self
            .tgt
            .ddl_triggers
            .iter()
            .filter(|t| !t.disabled)
            .filter(|t| !self.src.ddl_triggers.iter().any(|s| self.same(&s.name, &t.name) && s.disabled))
            .map(|t| format!("ENABLE TRIGGER {} ON DATABASE", quote(&t.name)))
            .collect();
        self.step(Phase::EnableDdlTriggers, "Re-enabling DDL triggers", enable);
    }

    fn deployment_warnings(&mut self) {
        let transactions = !self.opts.has(Opt::NoTransactions) && !self.opts.has(Opt::AssumeAlreadyInTransaction);
        if self.memory_optimized {
            if transactions {
                self.warn(warn::memory_optimized_in_transaction());
            }
            self.warn(warn::memory_optimized_filegroup());
        }
        if !self.opts.has(Opt::IgnoreFileGroups) {
            let list = |set: &BTreeSet<String>| set.iter().map(|f| quote(f)).collect::<Vec<_>>().join(", ");
            if !self.table_filegroups.is_empty() {
                self.warn(warn::filegroups_for_tables(&list(&self.table_filegroups)));
            }
            if !self.object_filegroups.is_empty() {
                self.warn(warn::filegroups_for_objects(&list(&self.object_filegroups)));
            }
            if !(self.table_filegroups.is_empty() && self.object_filegroups.is_empty()) && !self.has(Feature::Filegroups) {
                self.warn(warn::filegroups_unsupported());
            }
        }
        if self.selective_xml {
            self.step(
                Phase::PreTransaction,
                "Adding selective XML indexes",
                vec!["IF OBJECT_ID('[sys].[sp_db_selective_xml_index]') IS NOT NULL EXEC sys.sp_db_selective_xml_index NULL, 'on'".into()],
            );
        }
    }

    fn note_filegroup(&mut self, storage: &Storage, table: bool) {
        if let Some(fg) = &storage.data_space
            && storage.partition_column.is_none()
            && !fg.eq_ignore_ascii_case("PRIMARY")
        {
            if table { self.table_filegroups.insert(fg.clone()) } else { self.object_filegroups.insert(fg.clone()) };
        }
    }

    // ---- Schemas, types, sequences, synonyms ----

    fn schema(&mut self, d: &Difference) {
        let name = &d.name.name;
        match d.kind {
            DiffKind::OnlyIn1 => {
                let owner = self.src.schemas.iter().find(|s| s.name == *name).map(|s| s.owner.clone()).unwrap_or_else(|| "dbo".into());
                let sql = format!("CREATE SCHEMA {}\r\nAUTHORIZATION {}", quote(name), quote(&owner));
                self.group(Phase::CreateSchemas, "Creating schemas", vec![self.ex.schema(name, Check::Create, &sql)]);
            }
            DiffKind::OnlyIn2 => {
                self.warn(warn::schema_drop(name));
                let sql = format!("DROP SCHEMA {}", quote(name));
                self.group(Phase::DropSchemas, "Dropping schemas", vec![self.ex.schema(name, Check::Drop, &sql)]);
            }
            DiffKind::Different => {
                if let Some(s) = self.src.schemas.iter().find(|s| s.name == *name) {
                    self.group(Phase::CreateSchemas, "Altering schemas", vec![format!("ALTER AUTHORIZATION ON SCHEMA::{} TO {}", quote(name), quote(&s.owner))]);
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
            // Columns that use the type go over to its base type while it is re-created.
            if t.kind == UserTypeKind::Alias {
                self.unbind_type(t, s);
            }
            if s.is_none() {
                self.warn(warn::type_drop(&tn.to_string()));
            }
            // Modules that use the type go away with it and come back from the source.
            self.disturbed.insert(tn.clone());
            let sql = self.ex.user_type(&tn, Check::Drop, &format!("DROP TYPE {tn}"));
            self.group(Phase::DropTypes, "Dropping types", vec![sql]);
        }
        if let Some(s) = s {
            let name = d.name.to_string();
            if s.memory_optimized {
                self.memory_optimized = true;
                if !self.has(Feature::MemoryOptimized) {
                    self.warn(warn::target::memory_optimized_table_type(&name));
                }
            }
            if s.kind == UserTypeKind::Table && s.indexes.iter().any(|i| !i.primary_key && !i.unique_constraint) && !self.has(Feature::TableTypeIndexes) {
                self.warn(warn::target::table_type_indexes(&name));
            }
            let sql = self.ex.user_type(&d.name, Check::Create, &ddl::create_type(s));
            self.group(Phase::CreateTypes, "Creating types", vec![sql]);
        }
    }

    fn sequence(&mut self, d: &Difference) {
        let s = self.src.sequences.iter().find(|q| q.schema == d.name.schema && q.name == d.name.name);
        let tn = self.target_name(d);
        let t = self.tgt.sequences.iter().find(|q| q.schema == tn.schema && q.name == tn.name);
        if s.is_some() && !self.has(Feature::Sequences) {
            self.warn(warn::target::sequence(&d.name.to_string()));
            return;
        }
        let create = |p: &Self, s: &Sequence| p.ex.object(&d.name, "SO", Check::Create, &ddl::create_sequence(s));
        let drop = |p: &Self| p.ex.object(&tn, "SO", Check::Drop, &format!("DROP SEQUENCE {tn}"));
        match (s, t) {
            (Some(s), None) => self.group(Phase::CreateSequences, "Creating sequences", vec![create(self, s)]),
            (None, Some(_)) => self.group(Phase::DropSequences, "Dropping sequences", vec![drop(self)]),
            (Some(s), Some(t)) => {
                if s.data_type.sql() != t.data_type.sql() {
                    self.warn(warn::sequence_restarted(&tn.to_string()));
                    self.group(Phase::DropSequences, "Dropping sequences", vec![drop(self)]);
                    self.group(Phase::CreateSequences, "Creating sequences", vec![create(self, s)]);
                } else if let Some(sql) = ddl::alter_sequence(s, t) {
                    if s.start == t.start && sql.contains("RESTART WITH") {
                        self.warn(warn::sequence_restarting(&tn.to_string()));
                    }
                    let sql = self.ex.object(&tn, "SO", Check::Alter, &sql);
                    self.group(Phase::CreateSequences, "Altering sequences", vec![sql]);
                }
            }
            _ => {}
        }
    }

    fn synonym(&mut self, d: &Difference) {
        let s = self.src.synonyms.iter().find(|q| q.schema == d.name.schema && q.name == d.name.name);
        let tn = self.target_name(d);
        if self.tgt.synonyms.iter().any(|q| q.schema == tn.schema && q.name == tn.name) {
            let sql = self.ex.object(&tn, "SN", Check::Drop, &format!("DROP SYNONYM {tn}"));
            self.group(Phase::DropSynonyms, "Dropping synonyms", vec![sql]);
        }
        if let Some(s) = s {
            let parts = s.base_object.matches("].[").count() + 1;
            if parts > 2 && !self.has(Feature::MultipartSynonyms) {
                self.warn(warn::target::multipart_synonym(&d.name.to_string()));
            }
            let sql = self.ex.object(&d.name, "SN", Check::Create, &ddl::create_synonym(s));
            self.ranked(Phase::CreateObjects, format!("Creating {}", d.name), &d.name, vec![sql]);
        }
    }

    fn database_property(&mut self, d: &Difference) {
        let find = |db: &'a Database| db.extended_properties.iter().find(|p| p.class == "DATABASE" && p.name == d.name.name);
        let levels = "NULL, NULL, NULL, NULL, NULL, NULL";
        match (find(self.src), find(self.tgt)) {
            (Some(s), None) => self.add_xprop(s, levels),
            (Some(s), Some(_)) => self.update_xprop(s, levels),
            (None, Some(t)) => self.drop_xprop(t, levels),
            _ => {}
        }
    }

    fn add_xprop(&mut self, p: &ExtendedProperty, levels: &str) {
        let sql = self.ex.extended_property(&p.name, levels, Check::Create, &ddl::add_xprop(p, levels));
        self.push(Step { group: true, try_catch: true, ..Step::new(Phase::ExtendedProperties, "Creating extended properties", vec![sql]) });
    }

    fn update_xprop(&mut self, p: &ExtendedProperty, levels: &str) {
        let sql = ddl::update_xprop(p, levels);
        self.push(Step { group: true, try_catch: true, ..Step::new(Phase::ExtendedProperties, "Altering extended properties", vec![sql]) });
    }

    fn drop_xprop(&mut self, p: &ExtendedProperty, levels: &str) {
        let sql = self.ex.extended_property(&p.name, levels, Check::Drop, &ddl::drop_xprop(p, levels));
        self.push(Step { group: true, try_catch: true, ..Step::new(Phase::DropExtendedProperties, "Dropping extended properties", vec![sql]) });
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

    /// The module's text with WITH ENCRYPTION as AddWithEncryption / IgnoreWithEncryption want it.
    fn encryption(&self, definition: &str, target_encrypted: bool) -> String {
        if self.opts.has(Opt::IgnoreWithEncryption) {
            ddl::with_encryption(definition, target_encrypted)
        } else if self.opts.has(Opt::AddWithEncryption) {
            ddl::with_encryption(definition, true)
        } else {
            definition.to_string()
        }
    }

    /// A DML trigger's text: encryption as for modules, and WITH APPEND (deprecated, refused
    /// from SQL Server 2012) taken out where the target no longer takes it.
    fn trigger_text(&mut self, name: &ObjectName, definition: &str, target_encrypted: bool) -> String {
        let mut text = self.encryption(definition, target_encrypted);
        if let Some(at) = with_append(&text) {
            let removed = self.has(Feature::NoWithAppend);
            self.warn(Warning {
                severity: Severity::High,
                object: name.to_string(),
                message: format!(
                    "The trigger {name} contains the WITH APPEND clause which is {} in the target database version.",
                    if removed { "deprecated" } else { "not supported" }
                ),
            });
            if removed {
                text.replace_range(at, "");
            }
        }
        text
    }

    /// The module's statement with its existence check.
    fn module_statement(&self, name: &ObjectName, m: Module<'a>, check: Check, sql: &str) -> String {
        match m {
            Module::View(_) => self.ex.module(name, "V", check, sql),
            Module::Routine(r) => match r.kind {
                RoutineKind::Procedure => self.ex.module(name, "P", check, sql),
                RoutineKind::ClrProcedure => self.ex.module(name, "PC", check, sql),
                RoutineKind::ClrAggregate => self.ex.function(name, true, check, sql),
                _ => self.ex.function(name, false, check, sql),
            },
        }
    }

    fn nondecryptable(&mut self, name: &ObjectName, m: Module<'a>) {
        let kind = m.kind_name();
        self.warn(warn::nondecryptable(kind, &name.to_string()));
        if self.opts.has(Opt::IncludeDependencies) {
            self.warn(warn::nondecryptable_dependencies(kind, &name.to_string()));
        }
    }

    fn module(&mut self, d: &Difference) {
        let tn = self.target_name(d);
        let source = self.source_module(&d.name);
        let target = self.target_module(&tn);
        let kind_changed = matches!((&source, &target), (Some(Module::Routine(a)), Some(Module::Routine(b))) if function_class(a) != function_class(b) || a.clr_method.is_some() != b.clr_method.is_some());
        if kind_changed {
            self.warn(warn::function_type_change(&d.name.to_string()));
        }
        let drop_create = self.opts.has(Opt::DropAndCreateInsteadOfAlter) || kind_changed;
        match (source, target) {
            (Some(s), None) => self.create_module(&d.name, s),
            (None, Some(t)) => self.drop_module(&tn, t),
            (Some(s), Some(t)) => {
                if s.encrypted() || t.encrypted() {
                    self.nondecryptable(&d.name, s);
                    return;
                }
                // Anything schema-bound to this module has to be dropped first and re-created.
                self.disturbed.insert(tn.clone());
                if drop_create {
                    self.drop_module(&tn, t);
                    self.create_module(&d.name, s);
                } else {
                    self.native_module(&d.name, s);
                    let verb = if self.opts.has(Opt::CreateOrAlterInsteadOfAlter) && self.has(Feature::CreateOrAlter) { "CREATE OR ALTER" } else { "ALTER" };
                    let sql = match s {
                        Module::Routine(r) if r.clr_method.is_some() => ddl_objects::clr_routine(r, verb),
                        _ => ddl::module_sql(&self.encryption(s.definition(), t.encrypted()), verb, &d.name.quoted()),
                    };
                    let sql = self.module_statement(&d.name, s, Check::Alter, &sql);
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

    /// Natively compiled modules need memory-optimized support and no transaction.
    fn native_module(&mut self, name: &ObjectName, m: Module<'a>) {
        if let Module::Routine(r) = m
            && r.native_compilation
        {
            self.memory_optimized = true;
            if !self.has(Feature::MemoryOptimized) {
                let w = if r.kind == RoutineKind::Procedure { warn::target::native_procedure(&name.to_string()) } else { warn::target::native_function(&name.to_string()) };
                self.warn(w);
            }
        }
    }

    fn create_module(&mut self, name: &ObjectName, m: Module<'a>) {
        if m.encrypted() {
            self.nondecryptable(name, m);
            return;
        }
        self.native_module(name, m);
        self.recreate.insert(name.clone());
        let verb = if self.opts.has(Opt::CreateOrAlterInsteadOfAlter) && self.has(Feature::CreateOrAlter) { "CREATE OR ALTER" } else { "CREATE" };
        let sql = match m {
            Module::Routine(r) if r.clr_method.is_some() => ddl_objects::clr_routine(r, verb),
            _ => ddl::module_sql(&self.encryption(m.definition(), false), verb, &name.quoted()),
        };
        let sql = self.module_statement(name, m, Check::Create, &sql);
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
        let sql = self.module_statement(name, m, Check::Drop, &ddl::drop_module(kind, &name.quoted()));
        self.step(Phase::DropObjects, format!("Dropping {name}"), vec![sql]);
    }

    fn view_indexes(&mut self, name: &ObjectName, v: &'a View) {
        self.create_indexes(name, &Host::view(), v.indexes.iter().collect());
    }

    fn create_trigger(&mut self, parent: &ObjectName, tr: &Trigger) {
        let name = ObjectName::new(&parent.schema, &tr.name);
        let mut statements = if tr.clr_method.is_some() {
            vec![self.ex.module(&name, "TA", Check::Create, &ddl_objects::clr_trigger(&name, parent, tr, "CREATE"))]
        } else {
            if tr.encrypted || tr.definition.is_empty() {
                self.warn(warn::nondecryptable("trigger", &name.to_string()));
                return;
            }
            let text = self.trigger_text(&name, &tr.definition, false);
            vec![self.ex.module(&name, "TR", Check::Create, &ddl::module_sql(&text, "CREATE", &name.quoted()))]
        };
        if tr.disabled {
            statements.push(format!("DISABLE TRIGGER {name} ON {parent}"));
        }
        for (event, order) in &tr.order {
            statements.push(trigger_order(&name, order, event));
        }
        self.module_step(Phase::Triggers, format!("Creating trigger {name} on {parent}"), statements, &ModuleSettings::default());
    }

    /// Re-creates (or drops) everything schema-bound to a disturbed object: views and functions
    /// can't survive a change to what they are bound to. Without IncludeDependencies only
    /// selected objects are touched and the rest are reported.
    fn dependants(&mut self, selected: &[&Difference]) {
        let include = self.opts.has(Opt::IncludeDependencies);
        let mut queue: Vec<ObjectName> = self.disturbed.iter().cloned().collect();
        let mut seen: HashSet<ObjectName> = queue.iter().cloned().collect();
        let mut missing: BTreeMap<ObjectName, Vec<ObjectName>> = BTreeMap::new();
        while let Some(n) = queue.pop() {
            let deps: Vec<ObjectName> = self.tgt_deps.iter().filter(|d| d.strong && d.to == n).map(|d| d.from.clone()).collect();
            for dep in deps {
                if !seen.insert(dep.clone()) {
                    continue;
                }
                if self.object_dependant(&dep) {
                    queue.push(dep.clone());
                    continue;
                }
                let Some(t) = self.target_module(&dep) else { continue };
                if !include && !selected.iter().any(|d| self.target_name(d) == dep) {
                    missing.entry(n.clone()).or_default().push(dep.clone());
                    continue;
                }
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
        for (owner, objects) in missing {
            let list = objects.iter().map(|o| o.to_string()).collect::<Vec<_>>().join("\r\n");
            self.warn(warn::strong_dependencies(&owner.to_string(), &list));
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
            (Some(s), Some(_)) if let Some(copy) = self.unbound_copy(&tn) => self.rebuild_from_copy(s, copy),
            (Some(s), Some(t)) => self.alter_table(s, t),
            _ => {}
        }
    }

    /// Feature warnings for a table that is created or rebuilt.
    fn table_feature_warnings(&mut self, s: &Table) {
        let name = s.object_name().to_string();
        if s.memory_optimized {
            self.memory_optimized = true;
            if !self.has(Feature::MemoryOptimized) {
                self.warn(warn::target::memory_optimized_table(&name));
            }
        }
        if s.graph.is_some() && !self.has(Feature::Graph) {
            self.warn(warn::target::graph(&name));
        }
        for c in &s.columns {
            if c.masking_function.is_some() && !self.has(Feature::DataMasking) && !self.opts.has(Opt::IgnoreDynamicDataMasking) {
                self.warn(warn::target::masking(&name, &c.name));
            }
            if c.filestream && !self.has(Feature::Filestream) {
                self.warn(warn::target::filestream(&name, &c.name));
            }
        }
        if !self.opts.has(Opt::IgnoreFileGroups) && !s.memory_optimized {
            self.note_filegroup(&s.storage, true);
            for fg in s.text_filegroup.iter().chain(&s.filestream_filegroup) {
                if !fg.eq_ignore_ascii_case("PRIMARY") {
                    self.table_filegroups.insert(fg.clone());
                }
            }
        }
    }

    fn create_table(&mut self, s: &'a Table) {
        let name = s.object_name();
        // A system-versioned table comes with its history table, created first.
        let mut statements = Vec::new();
        if let Some(temporal) = &s.temporal {
            if !self.has(Feature::Temporal) {
                self.warn(warn::target::temporal(&name.to_string()));
            } else {
                self.retention_warning(s);
                if let Some(h) = self.src.table(&temporal.history_table.schema, &temporal.history_table.name) {
                    statements.push(ddl::create_table_with(h, None, false, &self.r));
                    for ix in &h.indexes {
                        statements.extend(ddl::create_index_with(&h.object_name().quoted(), ix, &self.r, &Host::of_table(h)));
                    }
                }
            }
        }
        self.table_feature_warnings(s);
        self.recreate.insert(name.clone());
        // A system-versioned table needs its primary key in the CREATE TABLE.
        let inline_key = s.temporal.is_some() && self.has(Feature::Temporal);
        if inline_key {
            self.objects.inline_key.insert(name.clone());
        }
        statements.push(self.ex.object(&name, "U", Check::Create, &ddl::create_table_with(s, None, inline_key, &self.r)));
        if s.lock_escalation != "TABLE" && !s.lock_escalation.is_empty() {
            statements.push(ddl::lock_escalation(&name.quoted(), &s.lock_escalation));
        }
        if let Some(ct) = &s.change_tracking {
            self.warn(warn::change_tracking_enable(&name.to_string()));
            statements.push(self.change_tracking_on(&name, ct));
        }
        self.module_step(Phase::CreateObjects, format!("Creating {name}"), statements, &ModuleSettings::default());
        self.table_children(s);
        self.bind_columns(s, None, true);
        self.table_parts(s, None);
    }

    /// Indexes, checks, foreign keys and triggers of a new or rebuilt table.
    fn table_children(&mut self, s: &'a Table) {
        let name = s.object_name();
        if !s.memory_optimized {
            let inline_key = self.objects.inline_key.contains(&name);
            self.create_indexes(&name, &Host::of_table(s), s.indexes.iter().filter(|i| !(inline_key && i.primary_key)).collect());
        }
        for ck in &s.check_constraints {
            self.add_check(&name, ck);
        }
        let fks: Vec<&ForeignKey> = s.foreign_keys.iter().filter(|fk| self.referenced_exists(&fk.referenced_table)).collect();
        for fk in fks {
            self.add_foreign_key(&name, fk);
        }
        for tr in &s.triggers {
            self.create_trigger(&name, tr);
        }
    }

    /// Creates indexes right after their table or view: clustered first, then the rest, then
    /// XML indexes (primary ones before those built on them).
    fn create_indexes(&mut self, owner: &ObjectName, host: &Host, mut ixs: Vec<&Index>) {
        let order = |i: &Index| match i.kind {
            IndexKind::Clustered | IndexKind::ClusteredColumnstore => 0,
            IndexKind::Xml if i.xml.as_ref().is_none_or(|x| x.using_index.is_none()) => 2,
            IndexKind::Xml => 3,
            _ => 1,
        };
        ixs.sort_by_key(|i| order(i));
        let table = owner.quoted();
        for ix in ixs {
            self.index_warnings(owner, ix, host);
            let mut statements = ddl::create_index_with(&table, ix, &self.r, host);
            let print = if ix.primary_key || ix.unique_constraint {
                statements[0] = self.ex.key(owner, &ix.name, ix.primary_key, Check::Create, &statements[0]);
                if ix.primary_key { format!("Creating primary key [{}] on {owner}", ix.name) } else { format!("Adding constraints to {owner}") }
            } else {
                statements[0] = self.ex.index(owner, &ix.name, Check::Create, &statements[0]);
                format!("Creating index [{}] on {owner}", ix.name)
            };
            if ix.xml.as_ref().is_some_and(|x| x.kind == XmlIndexKind::Selective) {
                self.selective_xml = true;
            }
            if !self.opts.has(Opt::IgnoreFileGroups) {
                self.note_filegroup(&ix.storage, false);
            }
            self.ranked(Phase::CreateObjects, print, owner, statements);
        }
    }

    fn index_warnings(&mut self, owner: &ObjectName, ix: &Index, host: &Host) {
        let (t, name) = (owner.to_string(), ix.name.as_str());
        match ix.kind {
            IndexKind::ClusteredColumnstore if !self.has(Feature::ClusteredColumnstore) => self.warn(warn::target::clustered_columnstore(&t, name)),
            IndexKind::NonclusteredColumnstore if !self.has(Feature::Columnstore) => self.warn(warn::target::columnstore(&t, name)),
            _ => {}
        }
        if ix.incremental && !self.opts.has(Opt::IgnoreStatisticsIncremental) {
            if !self.has(Feature::IncrementalStatistics) {
                self.warn(warn::target::incremental_statistics(&t, name));
            } else if self.opts.has(Opt::IgnoreFileGroups) {
                self.warn(warn::incremental_with_ignored_filegroups(&t, name));
            }
        }
        if let Some(sp) = &ix.spatial
            && sp.tessellation.to_ascii_uppercase().ends_with("AUTO_GRID")
            && !self.has(Feature::AutoGrid)
        {
            self.warn(warn::target::auto_grid(&t, name));
        }
        if ix.xml.as_ref().is_some_and(|x| matches!(x.kind, XmlIndexKind::Selective | XmlIndexKind::SecondarySelective)) && !self.has(Feature::SelectiveXmlIndexes) {
            self.warn(warn::target::selective_xml(&owner.name, name));
        }
        if self.opts.has(Opt::AddOnlineOn) && !host.memory_optimized {
            let f = &self.r.features;
            let w = match ix.kind {
                IndexKind::ClusteredColumnstore if !f.has(Feature::OnlineClusteredColumnstore) => Some(warn::target::online_clustered_columnstore(&t, f.version_name())),
                IndexKind::NonclusteredColumnstore if !f.has(Feature::OnlineNonclusteredColumnstore) => {
                    Some(warn::target::online_nonclustered_columnstore(&t, f.version_name()))
                }
                IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore => None,
                _ if !f.has(Feature::Online) => Some(warn::target::online(&t, f.edition_name())),
                _ => None,
            };
            if let Some(w) = w {
                self.warn(w);
            }
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
        self.warn(warn::table_drop(&name.to_string()));
        self.drop_fulltext_before(t);
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
        let mut statements = vec![self.ex.object(&name, "U", Check::Drop, &format!("DROP TABLE {}", name.quoted()))];
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
        let mut sql = ddl::add_foreign_key_with(&owner.quoted(), fk, self.opts);
        sql[0] = self.ex.foreign_key(owner, &fk.name, Check::Create, &sql[0]);
        let disable = sql.split_off(1);
        self.group(Phase::CreateForeignKeys, format!("Adding foreign keys to {owner}"), sql);
        let disable = disable.into_iter().map(|s| self.ex.foreign_key(owner, &fk.name, Check::Alter, &s)).collect();
        self.group(Phase::CreateForeignKeys, format!("Disabling constraints on {owner}"), disable);
    }

    fn add_check(&mut self, owner: &ObjectName, ck: &CheckConstraint) {
        let mut sql = ddl::add_check_with(&owner.quoted(), ck, self.opts);
        sql[0] = self.ex.check_constraint(owner, &ck.name, Check::Create, &sql[0]);
        let disable = sql.split_off(1);
        self.group(Phase::CreateChecks, format!("Adding constraints to {owner}"), sql);
        let disable = disable.into_iter().map(|s| self.ex.check_constraint(owner, &ck.name, Check::Alter, &s)).collect();
        self.group(Phase::CreateChecks, format!("Disabling constraints on {owner}"), disable);
    }

    fn drop_foreign_key(&mut self, owner: &Table, fk: &ForeignKey) {
        let name = owner.object_name();
        if self.dropped_sub.insert((name.clone(), self.key(&fk.name))) {
            let sql = self.ex.foreign_key(&name, &fk.name, Check::Drop, &ddl::drop_constraint(&name.quoted(), &fk.name));
            self.group(Phase::DropForeignKeys, format!("Dropping foreign keys from {name}"), vec![sql]);
        }
    }

    /// Drops a check constraint once.
    fn drop_check(&mut self, table: &ObjectName, name: &str) {
        if self.dropped_sub.insert((table.clone(), self.key(name))) {
            let sql = self.ex.check_constraint(table, name, Check::Drop, &ddl::drop_constraint(&table.quoted(), name));
            self.step(Phase::DropChecks, format!("Dropping constraints from {table}"), vec![sql]);
        }
    }

    /// Drops a default constraint once.
    fn drop_default(&mut self, table: &ObjectName, column: &str, df: &DefaultConstraint) {
        if self.dropped_sub.insert((table.clone(), self.key(&df.name))) {
            let sql = self.ex.default_constraint(table, column, &df.name, Check::Drop, &ddl::drop_constraint(&table.quoted(), &df.name));
            self.step(Phase::DropDefaults, format!("Dropping constraints from {table}"), vec![sql]);
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

    /// Whether the table must be rebuilt rather than altered, and the warnings that say why
    /// (none for the rebuilds SQL Compare does silently).
    fn rebuild_reason(&self, s: &Table, t: &Table, pairs: &[(Option<&Column>, Option<&Column>)]) -> Option<Vec<Warning>> {
        let o = self.opts;
        let name = s.object_name().to_string();
        if s.memory_optimized || t.memory_optimized {
            return Some(Vec::new());
        }
        match (&s.graph, &t.graph) {
            (Some(_), None) => return Some(vec![warn::rebuild::add_graph(&name)]),
            (None, Some(_)) => return Some(vec![warn::rebuild::remove_graph(&name)]),
            _ => {}
        }
        if !o.has(Opt::IgnoreFileGroups) {
            if s.storage.partition_column != t.storage.partition_column {
                return Some(if s.storage.partition_column.is_some() && t.storage.partition_column.is_none() {
                    Vec::new()
                } else {
                    vec![warn::rebuild::partitioning(&name)]
                });
            }
            if self.key(s.storage.data_space.as_deref().unwrap_or("PRIMARY")) != self.key(t.storage.data_space.as_deref().unwrap_or("PRIMARY")) {
                return Some(vec![if s.storage.partition_column.is_some() { warn::rebuild::partitioning(&name) } else { warn::rebuild::filegroup(&name) }]);
            }
            if let Some(a) = &s.text_filegroup
                && !self.same(a, t.text_filegroup.as_deref().or(t.storage.data_space.as_deref()).unwrap_or("PRIMARY"))
            {
                return Some(vec![warn::rebuild::text_filegroup(&name)]);
            }
            if let (Some(a), Some(b)) = (&s.filestream_filegroup, &t.filestream_filegroup)
                && !self.same(a, b)
            {
                return Some(vec![warn::rebuild::filestream_filegroup(&name)]);
            }
        }
        let modified: Vec<&str> = pairs
            .iter()
            .filter_map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) if !material_differences(a, b, o).is_empty() => Some(b.name.as_str()),
                _ => None,
            })
            .collect();
        let def = |c: &Column, nullable: bool| {
            let mut c = c.clone();
            c.nullable = nullable;
            ddl::column_definition_with(&c, false, &self.r, false)
        };
        for (a, b) in pairs {
            let (Some(a), Some(b)) = (a, b) else {
                if let (Some(a), None) = (a, b) {
                    if a.filestream {
                        return Some(vec![warn::rebuild::filestream(&name, &a.name)]);
                    }
                    if a.column_set && t.columns.iter().any(|c| c.sparse) {
                        return Some(vec![warn::rebuild::column_set(&name, &a.name)]);
                    }
                }
                if let (None, Some(b)) = (a, b)
                    && b.filestream
                {
                    return Some(vec![warn::rebuild::filestream(&name, &b.name)]);
                }
                continue;
            };
            if a.name != b.name && a.column_set && b.column_set {
                return Some(vec![warn::rebuild::column_set(&name, &a.name)]);
            }
            if material_differences(a, b, o).is_empty() {
                continue;
            }
            if a.filestream || b.filestream {
                return Some(vec![warn::rebuild::filestream(&name, &a.name)]);
            }
            if b.generated_always.is_some() && t.temporal.is_some() {
                return Some(vec![warn::rebuild::period_column(&name, &def(b, b.nullable))]);
            }
            let explicit = a.computed.is_some()
                || b.computed.is_some()
                || matches!(a.data_type.name.as_str(), "timestamp" | "rowversion")
                || matches!(b.data_type.name.as_str(), "timestamp" | "rowversion")
                || matches!(cast::classify(&b.data_type, &a.data_type), Cast::Explicit | Cast::None);
            if explicit {
                return Some(vec![warn::rebuild::column_alter(&name, &def(b, b.nullable), &def(a, b.nullable))]);
            }
            if !o.has(Opt::IgnoreIdentityProperties) {
                let seed = |c: &Column| c.identity.as_ref().map(|i| (i.seed.clone(), i.increment.clone()));
                if a.identity.is_some() != b.identity.is_some()
                    || (!o.has(Opt::IgnoreIdentitySeedAndIncrement) && seed(a) != seed(b))
                {
                    return Some(vec![warn::rebuild::identity_alter(&name, &b.name)]);
                }
            }
            if a.column_set != b.column_set {
                return Some(vec![warn::rebuild::column_set(&name, &a.name)]);
            }
        }
        // An added NOT NULL column without any default: the table is rebuilt and fails if it has rows.
        for (a, b) in pairs {
            if let (Some(a), None) = (a, b)
                && !a.nullable
                && a.default.is_none()
                && a.identity.is_none()
                && a.computed.is_none()
                && !matches!(a.data_type.name.as_str(), "timestamp" | "rowversion")
            {
                return Some(vec![match &a.bound_default {
                    Some(d) => warn::rebuild::add_not_null_bound_default(&name, &a.name, d),
                    None => warn::rebuild::add_not_null_no_default(&name, &a.name),
                }]);
            }
        }
        // The IDENTITY moved to another column.
        if !o.has(Opt::IgnoreIdentityProperties) {
            let si = s.columns.iter().find(|c| c.identity.is_some()).map(|c| self.key(&c.name));
            let ti = t.columns.iter().find(|c| c.identity.is_some()).map(|c| self.key(&c.name));
            if si.is_some() && ti.is_some() && si != ti && pairs.iter().all(|(a, b)| !(a.is_some_and(|a| a.identity.is_some()) && b.is_some_and(|b| b.identity.is_some()))) {
                return Some(vec![warn::rebuild::identity_change(&name)]);
            }
        }
        // A persisted computed column that reads a modified column.
        for c in s.columns.iter().filter(|c| c.computed.as_ref().is_some_and(|x| x.persisted)) {
            let def = c.computed.as_ref().unwrap().definition.to_lowercase();
            if modified.iter().any(|m| def.contains(&format!("[{}]", m.to_lowercase())) && **m != c.name) {
                return Some(vec![warn::rebuild::persisted_reference(&name, &c.name)]);
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
                return Some(vec![warn::rebuild::column_order(&name)]);
            }
        }
        None
    }

    fn alter_table(&mut self, s: &'a Table, t: &'a Table) {
        let pairs = self.column_pairs(s, t);
        if let Some(why) = self.rebuild_reason(s, t, &pairs) {
            for w in why {
                self.warn(w);
            }
            if s.temporal.is_some() || t.temporal.is_some() {
                self.rebuild_temporal(s, t, &pairs);
            } else {
                self.rebuild_table(s, t, &pairs);
            }
            return;
        }
        self.alter_in_place(s, t, &pairs);
    }

    /// Drops what depends on a target column that changes or goes away.
    fn release_column(&mut self, t: &'a Table, col: &str, keep_indexes: bool) {
        let name = t.object_name();
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
                    self.release_index(t, ix);
                }
            }
        }
        for ck in &t.check_constraints {
            if ck.column.as_deref().is_some_and(|c| self.same(c, col)) || mentions(&ck.definition) {
                self.drop_check(&name, &ck.name);
            }
        }
        if let Some(c) = t.columns.iter().find(|c| self.same(&c.name, col)) {
            if let Some(df) = &c.default {
                self.drop_default(&name, &c.name, df);
            }
            self.unbind(&name, c);
        }
        for fk in &t.foreign_keys {
            if fk.columns.iter().any(|c| self.same(c, col)) {
                self.drop_foreign_key(t, fk);
            }
        }
        for st in &t.statistics {
            if st.columns.iter().any(|c| self.same(c, col)) || st.filter.as_deref().is_some_and(mentions) {
                self.drop_statistics(t, st);
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

    /// sp_unbindefault / sp_unbindrule for a target column's bindings.
    fn unbind(&mut self, table: &ObjectName, c: &Column) {
        if self.opts.has(Opt::IgnoreBindings) {
            return;
        }
        let column = format!("{table}.{}", quote(&c.name));
        if let Some(d) = &c.bound_default
            && self.unbound.insert((table.clone(), self.key(&c.name), false))
        {
            let sql = self.ex.binding(table, &c.name, d, false, Check::Drop, &format!("EXEC sp_unbindefault {}", literal(&column)));
            self.group(Phase::UnbindColumns, "Unbinding defaults from columns", vec![sql]);
        }
        if let Some(r) = &c.bound_rule
            && self.unbound.insert((table.clone(), self.key(&c.name), true))
        {
            let sql = self.ex.binding(table, &c.name, r, true, Check::Drop, &format!("EXEC sp_unbindrule {}", literal(&column)));
            self.group(Phase::UnbindColumns, "Unbinding rules from columns", vec![sql]);
        }
    }

    /// sp_bindefault / sp_bindrule for the source columns of a table; `only` limits it to
    /// columns that lost or never had the binding in the target.
    fn bind_columns(&mut self, s: &Table, only: Option<&[&str]>, defaults: bool) {
        if self.opts.has(Opt::IgnoreBindings) {
            return;
        }
        let name = s.object_name();
        for c in &s.columns {
            if only.is_some_and(|o| !o.iter().any(|x| self.same(x, &c.name))) {
                continue;
            }
            let column = format!("{name}.{}", quote(&c.name));
            if let Some(d) = c.bound_default.as_ref().filter(|_| defaults) {
                let sql = self.ex.binding(&name, &c.name, d, false, Check::Create, &format!("EXEC sp_bindefault {}, {}", literal(d), literal(&column)));
                self.group(Phase::BindColumns, "Binding defaults to columns", vec![sql]);
            }
            if let Some(r) = &c.bound_rule {
                let sql = self.ex.binding(&name, &c.name, r, true, Check::Create, &format!("EXEC sp_bindrule {}, {}", literal(r), literal(&column)));
                self.group(Phase::BindColumns, "Binding rules to columns", vec![sql]);
            }
        }
    }

    fn change_tracking_on(&self, name: &ObjectName, ct: &ChangeTracking) -> String {
        let sql = format!(
            "ALTER TABLE {} ENABLE CHANGE_TRACKING{}",
            name.quoted(),
            if ct.track_columns_updated { " WITH (TRACK_COLUMNS_UPDATED = ON)" } else { "" }
        );
        self.ex.change_tracking(name, Check::Create, &sql)
    }

    /// ALTER COLUMN … WITH (ONLINE = ON) is possible for this column change.
    fn online_alter(&mut self, t: &Table, a: &Column, b: &Column) -> bool {
        if !self.opts.has(Opt::AddOnlineOn) {
            return false;
        }
        let host = Host::of_table(t);
        let clr = |c: &Column| c.data_type.schema.is_some() || matches!(c.data_type.name.as_str(), "geometry" | "geography" | "hierarchyid");
        if host.legacy_lob || t.columns.iter().any(|c| c.filestream) || clr(a) || clr(b) {
            return false;
        }
        if !self.has(Feature::Online) {
            let edition = self.r.features.edition_name();
            self.warn(warn::target::online(&t.object_name().to_string(), edition));
            return false;
        }
        t.temporal.is_none()
    }

    fn alter_in_place(&mut self, s: &'a Table, t: &'a Table, pairs: &[(Option<&'a Column>, Option<&'a Column>)]) {
        let o = self.opts;
        let name = t.object_name();
        let table = name.quoted();
        let label = name.to_string();
        let mut statements: Vec<String> = Vec::new();
        let mut after_defaults: Vec<String> = Vec::new();
        let mut disturbed = false;
        let mut rebind: Vec<&str> = Vec::new();

        let mut adds = Vec::new();
        let mut drops: Vec<&Column> = Vec::new();
        let mut computed_drops: Vec<&Column> = Vec::new();
        let mut computed_adds = Vec::new();
        for (a, b) in pairs {
            match (a, b) {
                (Some(a), None) if a.computed.is_some() => computed_adds.push(*a),
                (Some(a), None) => {
                    adds.push(*a);
                    rebind.push(&a.name);
                }
                (None, Some(b)) => {
                    self.release_column(t, &b.name, false);
                    if b.computed.is_some() || matches!(b.data_type.name.as_str(), "timestamp" | "rowversion") {
                        computed_drops.push(*b);
                    } else {
                        drops.push(*b);
                    }
                    disturbed = true;
                }
                _ => {}
            }
        }
        if !drops.is_empty() {
            let lost = drops.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ");
            self.warn(warn::columns_dropped(&label, &lost));
        }

        // Matched columns that change.
        let mut renames = Vec::new();
        let mut alters = Vec::new();
        let mut recompute: Vec<&Column> = Vec::new();
        for (a, b) in pairs {
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a.name != b.name {
                let sql = format!("EXEC sp_rename {}, {}, N'COLUMN'", literal(&format!("{table}.{}", quote(&b.name))), literal(&a.name));
                renames.push(self.ex.rename_column(&name, &b.name, &a.name, &sql));
                disturbed = true;
            }
            let diffs = material_differences(a, b, o);
            if diffs.is_empty() {
                continue;
            }
            // Bindings are deployed on their own.
            let binding = |d: &super::compare::PropDiff| d.name == "Bound default" || d.name == "Bound rule";
            if diffs.iter().any(binding) {
                self.unbind(&name, b);
                rebind.push(&a.name);
            }
            let diffs: Vec<_> = diffs.into_iter().filter(|d| !binding(d)).collect();
            if diffs.is_empty() {
                continue;
            }
            let only_default = diffs.iter().all(|d| d.name.starts_with("Default"));
            if only_default {
                if let Some(df) = &b.default {
                    self.drop_default(&name, &b.name, df);
                }
                if let Some(df) = &a.default {
                    after_defaults.push(self.ex.default_constraint(&name, &a.name, &df.name, Check::Create, &ddl::add_default(&table, &a.name, df)));
                }
                continue;
            }
            if a.computed.is_some() {
                // Non-persisted computed columns are dropped and added back.
                self.release_column(t, &b.name, false);
                computed_drops.push(*b);
                recompute.push(a);
                continue;
            }
            self.release_column(t, &b.name, false);
            rebind.push(&a.name);
            disturbed = true;
            let (old, new) = (type_sql(&b.data_type), type_sql(&a.data_type));
            let (shorter, unicode) = cast::losses(&b.data_type, &a.data_type);
            if shorter {
                self.warn(warn::truncated(&label, &a.name));
            }
            if unicode {
                self.warn(warn::unicode_loss(&label, &a.name));
            }
            if cast::classify(&b.data_type, &a.data_type) == Cast::Lossy {
                self.warn(match (&a.data_type.xml_collection, a.data_type.name.as_str()) {
                    (Some(xc), "xml") => warn::xml_schema_change(&label, &a.name, xc),
                    _ => warn::precision_loss(&label, &a.name, &old, &new),
                });
            }
            let becomes_not_null = b.nullable && !a.nullable && !o.has(Opt::IgnoreNullability);
            let mut col: Column = (*a).clone();
            col.identity = None;
            col.default = None;
            let online = if self.online_alter(t, a, b) { " WITH (ONLINE = ON)" } else { "" };
            if becomes_not_null {
                if let Some(df) = &a.default {
                    self.warn(warn::not_null_default(&label, &a.name));
                    col.nullable = true;
                    alters.push(format!("ALTER TABLE {table} ALTER COLUMN {}{online}", ddl::column_definition_with(&col, false, &self.r, t.memory_optimized)));
                    after_defaults.push(self.ex.default_constraint(&name, &a.name, &df.name, Check::Create, &ddl::add_default(&table, &a.name, df)));
                    after_defaults.push(format!("UPDATE {table} SET {}=DEFAULT WHERE {} IS NULL", quote(&a.name), quote(&a.name)));
                    col.nullable = false;
                    after_defaults.push(format!("ALTER TABLE {table} ALTER COLUMN {}{online}", ddl::column_definition_with(&col, false, &self.r, t.memory_optimized)));
                    continue;
                }
                self.warn(warn::not_null_no_default(&label, &a.name));
            }
            if o.has(Opt::IgnoreNullability) {
                col.nullable = b.nullable;
            }
            alters.push(format!("ALTER TABLE {table} ALTER COLUMN {}{online}", ddl::column_definition_with(&col, false, &self.r, t.memory_optimized)));
            if let Some(df) = &a.default {
                after_defaults.push(self.ex.default_constraint(&name, &a.name, &df.name, Check::Create, &ddl::add_default(&table, &a.name, df)));
            }
        }

        // Statement order inside one table change: drop computed columns, add columns, drop
        // columns, renames, alters, add computed columns.
        let drop_list = |p: &Self, cols: &[&Column]| -> Vec<String> {
            if cols.is_empty() {
                Vec::new()
            } else if p.ex.enabled() {
                cols.iter().map(|c| p.ex.columns(&name, &[&c.name], Check::Drop, &format!("ALTER TABLE {table} DROP COLUMN {}", quote(&c.name)))).collect()
            } else {
                vec![format!("ALTER TABLE {table} DROP\r\n{}", cols.iter().map(|c| format!("COLUMN {}", quote(&c.name))).collect::<Vec<_>>().join(",\r\n"))]
            }
        };
        // Period columns of a table that becomes system-versioned are added with their PERIOD.
        let period = match (&s.temporal, &t.temporal) {
            (Some(x), None) if adds.iter().any(|c| c.generated_always.is_some()) => Some(format!("PERIOD FOR SYSTEM_TIME ({}, {})", quote(&x.period_start), quote(&x.period_end))),
            _ => None,
        };
        let add_list = |p: &Self, cols: &[&Column]| -> Vec<String> {
            if cols.is_empty() {
                Vec::new()
            } else if let Some(period) = period.as_ref().filter(|_| cols.iter().any(|c| c.generated_always.is_some())) {
                let mut defs: Vec<String> = cols.iter().map(|c| ddl::column_definition_with(c, true, &p.r, t.memory_optimized)).collect();
                defs.push(period.clone());
                let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
                vec![p.ex.columns(&name, &names, Check::Create, &format!("ALTER TABLE {table} ADD\r\n{}", defs.join(",\r\n")))]
            } else if p.ex.enabled() {
                cols.iter()
                    .map(|c| {
                        let def = ddl::column_definition_with(c, true, &p.r, t.memory_optimized);
                        p.ex.columns(&name, &[&c.name], Check::Create, &format!("ALTER TABLE {table} ADD {def}"))
                    })
                    .collect()
            } else {
                let defs: Vec<String> = cols.iter().map(|c| ddl::column_definition_with(c, true, &p.r, t.memory_optimized)).collect();
                vec![format!("ALTER TABLE {table} ADD\r\n{}", defs.join(",\r\n"))]
            }
        };
        let (versioning_off, period_change) = self.versioning_change(s, t);
        statements.extend(period_change.0);
        statements.extend(drop_list(self, &computed_drops));
        statements.extend(add_list(self, &adds));
        statements.extend(drop_list(self, &drops));
        statements.extend(renames);
        statements.extend(alters);
        let computed: Vec<&Column> = computed_adds.iter().chain(recompute.iter()).copied().collect();
        statements.extend(add_list(self, &computed));
        statements.extend(period_change.1);
        // Table options.
        if !o.has(Opt::IgnoreDataCompression) && s.storage.compression != t.storage.compression {
            let c = s.storage.compression.first().map(|p| p.compression.as_str()).unwrap_or("NONE");
            statements.push(format!("ALTER TABLE {table} REBUILD PARTITION = ALL\r\nWITH (DATA_COMPRESSION = {c})"));
        }
        if s.lock_escalation != t.lock_escalation && !s.lock_escalation.is_empty() {
            statements.push(ddl::lock_escalation(&table, &s.lock_escalation));
        }
        if disturbed {
            self.disturbed.insert(name.clone());
        }
        self.module_step(Phase::CreateObjects, format!("Altering {name}"), statements, &ModuleSettings::default());
        self.step(Phase::CreateDefaults, format!("Altering {name}"), after_defaults);
        self.sub_objects(s, t);
        self.change_tracking(s, t);
        self.bind_columns(s, Some(&rebind), true);
        self.table_parts(s, Some(t));
        self.versioning_on(s, t, versioning_off);
    }

    /// Change tracking of a table altered in place. It needs the primary key, so a key that is
    /// dropped and created again takes change tracking off and on around it.
    fn change_tracking(&mut self, s: &'a Table, t: &'a Table) {
        let name = t.object_name();
        let label = name.to_string();
        let ignore = self.opts.has(Opt::IgnoreChangeTracking);
        let pk_dropped = t.indexes.iter().any(|i| i.primary_key && self.dropped_sub.contains(&(name.clone(), self.key(&i.name))));
        let wanted = if ignore { t.change_tracking.as_ref() } else { s.change_tracking.as_ref() };
        let (disable, enable) = match (&t.change_tracking, wanted) {
            (Some(_), _) if pk_dropped => {
                if wanted.is_some() {
                    self.warn(warn::change_tracking_reenable(&label));
                }
                (true, wanted)
            }
            (Some(a), Some(b)) if a != b => {
                self.warn(warn::change_tracking_reenable(&label));
                (true, wanted)
            }
            (Some(_), None) => (true, None),
            (None, Some(_)) => {
                self.warn(warn::change_tracking_enable(&label));
                if t.primary_key().is_none() && self.opts.has(Opt::IgnoreIndexes)
                    && let Some(pk) = s.primary_key()
                {
                    // The key is deployed although indexes are ignored: change tracking needs it.
                    self.warn(Warning {
                        severity: Severity::Medium,
                        object: label.clone(),
                        message: format!("Table change tracking requires a primary key. To enable change tracking on table {label}, primary key {} will be included in the deployment.", quote(&pk.name)),
                    });
                    self.create_indexes(&name, &Host::of_table(s), vec![pk]);
                }
                (false, wanted)
            }
            _ => (false, None),
        };
        if disable {
            let sql = self.ex.change_tracking(&name, Check::Drop, &format!("ALTER TABLE {} DISABLE CHANGE_TRACKING", name.quoted()));
            self.group(Phase::DisableChangeTracking, "Disabling table change tracking", vec![sql]);
        }
        if let Some(ct) = enable {
            let sql = self.change_tracking_on(&name, ct);
            self.ranked(Phase::CreateObjects, "", &name, vec![sql]);
        }
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
        // Indexes: by name; a changed index is dropped and created again, one that is only
        // enabled or disabled is altered.
        let mut creates = Vec::new();
        for a in s.indexes.iter().filter(|i| !ignored(i)) {
            let b = t.indexes.iter().find(|b| self.same(&b.name, &a.name));
            let released = self.dropped_sub.contains(&(name.clone(), self.key(&a.name)));
            match b {
                Some(b) if !released && !differs_ix(a, b) => {}
                Some(b) if !released && a.disabled != b.disabled && !differs_ix(&Index { disabled: b.disabled, ..a.clone() }, b) => {
                    let (verb, what) = if a.disabled { ("DISABLE", "Disabling") } else { ("REBUILD", "Enabling") };
                    self.step(Phase::DropIndexes, format!("{what} index {} on {name}", a.name), vec![format!("ALTER INDEX {} ON {table} {verb}", quote(&a.name))]);
                }
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
        // Indexes dropped because a column or key changed come back too.
        for a in &s.indexes {
            if self.dropped_sub.contains(&(name.clone(), self.key(&a.name))) && !creates.iter().any(|c| c.name == a.name) {
                creates.push(a);
            }
        }
        if t.indexes.iter().any(|i| i.primary_key && self.dropped_sub.contains(&(name.clone(), self.key(&i.name))))
            && t.indexes.iter().any(|i| i.disabled && !is_clustered(i))
        {
            self.warn(warn::indexes_auto_enabled(&name.to_string()));
        }
        self.create_indexes(&name, &Host::of_table(s), creates);

        // Check constraints.
        let ignore_nocheck = o.has(Opt::IgnoreNocheckAndWithNocheck);
        if !o.has(Opt::IgnoreChecks) {
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
                    self.drop_check(&name, &b.name);
                }
                self.add_check(&name, a);
            }
            for b in &t.check_constraints {
                if !s.check_constraints.iter().any(|a| self.same(&a.name, &b.name)) {
                    self.drop_check(&name, &b.name);
                }
            }
        }

        // Foreign keys: a change other than NOCHECK / disabled means drop and add.
        if !o.has(Opt::IgnoreKeys) {
            let ignore_with = o.has(Opt::IgnoreWithNocheck) || ignore_nocheck;
            for a in &s.foreign_keys {
                let b = t.foreign_keys.iter().find(|b| self.same(&b.name, &a.name));
                let released = self.dropped_sub.contains(&(name.clone(), self.key(&a.name)));
                let same_definition = b.is_some_and(|b| {
                    a.columns.iter().map(|c| self.key(c)).eq(b.columns.iter().map(|c| self.key(c)))
                        && a.referenced_columns.iter().map(|c| self.key(c)).eq(b.referenced_columns.iter().map(|c| self.key(c)))
                        && self.key(&a.referenced_table.to_string()) == self.key(&b.referenced_table.to_string())
                        && a.on_delete == b.on_delete
                        && a.on_update == b.on_update
                        && (o.has(Opt::IgnoreNotForReplication) || a.not_for_replication == b.not_for_replication)
                });
                if let Some(b) = b
                    && same_definition
                    && !released
                {
                    let trust_differs = !ignore_with && a.not_trusted != b.not_trusted;
                    let enabled_differs = !ignore_nocheck && a.disabled != b.disabled;
                    if trust_differs || enabled_differs {
                        self.recheck_foreign_key(&name, a, b, ignore_with);
                    }
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
                        let same_text = super::text::compare(&a.definition, &b.definition, super::text::TextKind::Module, &o.clone().with(Opt::IgnoreOwners)).is_none()
                            && a.clr_method == b.clr_method
                            && (a.clr_method.is_none() || (a.events == b.events && a.execute_as == b.execute_as));
                        let trig = ObjectName::new(&name.schema, &a.name);
                        let mut statements = Vec::new();
                        if !same_text {
                            if a.clr_method.is_some() {
                                statements.push(ddl_objects::clr_trigger(&trig, &name, a, "ALTER"));
                            } else {
                                let text = self.trigger_text(&trig, &a.definition, b.encrypted);
                                statements.push(ddl::module_sql(&text, "ALTER", &trig.quoted()));
                            }
                        }
                        if a.disabled != b.disabled || !same_text {
                            statements.push(format!("{} TRIGGER {trig} ON {table}", if a.disabled { "DISABLE" } else { "ENABLE" }));
                        }
                        if !statements.is_empty() {
                            self.module_step(Phase::Triggers, format!("Altering trigger {trig} on {name}"), statements, &ModuleSettings::default());
                        }
                        if a.order != b.order && !o.has(Opt::IgnoreTriggerOrder) {
                            let order: Vec<String> = a.order.iter().map(|(event, ord)| trigger_order(&trig, ord, event)).collect();
                            self.step(Phase::Triggers, format!("Altering trigger order for {trig} on {name}"), order);
                        }
                    }
                }
            }
            for b in &t.triggers {
                if !s.triggers.iter().any(|a| self.same(&a.name, &b.name)) {
                    let trig = ObjectName::new(&name.schema, &b.name);
                    let sql = self.ex.module(&trig, "TR", Check::Drop, &format!("DROP TRIGGER {trig}"));
                    self.step(Phase::DropTriggers, format!("Dropping trigger {trig} from {name}"), vec![sql]);
                }
            }
        }
    }

    /// A foreign key that only changes NOCHECK or WITH NOCHECK is re-checked in place.
    fn recheck_foreign_key(&mut self, table: &ObjectName, a: &ForeignKey, b: &ForeignKey, ignore_with: bool) {
        let t = table.quoted();
        let fk = quote(&a.name);
        if a.disabled {
            let sql = self.ex.foreign_key(table, &a.name, Check::Alter, &format!("ALTER TABLE {t} NOCHECK CONSTRAINT {fk}"));
            self.group(Phase::CreateForeignKeys, format!("Disabling constraints on {table}"), vec![sql]);
            return;
        }
        let statements = if a.not_trusted && !ignore_with {
            if b.disabled {
                vec![format!("ALTER TABLE {t} CHECK CONSTRAINT {fk}")]
            } else {
                // A trusted key becomes untrusted only by switching it off and on without checking.
                vec![format!("ALTER TABLE {t} NOCHECK CONSTRAINT {fk}"), format!("ALTER TABLE {t} CHECK CONSTRAINT {fk}")]
            }
        } else {
            vec![format!("ALTER TABLE {t} WITH CHECK CHECK CONSTRAINT {fk}")]
        };
        self.group(Phase::CreateForeignKeys, format!("Enabling constraints on {table}"), statements);
    }

    fn release_index(&mut self, t: &'a Table, ix: &Index) {
        let name = t.object_name();
        if !self.dropped_sub.insert((name.clone(), self.key(&ix.name))) {
            return;
        }
        if ix.primary_key || ix.unique_constraint {
            let tgt = self.tgt;
            for other in &tgt.tables {
                for fk in other.foreign_keys.iter().filter(|fk| fk.referenced_table == name) {
                    self.drop_foreign_key(other, fk);
                }
            }
        }
        // XML and spatial indexes need the clustered primary key; secondary XML indexes need
        // their primary XML index.
        if ix.primary_key && is_clustered(ix) {
            for x in t.indexes.iter().filter(|x| matches!(x.kind, IndexKind::Xml | IndexKind::Spatial)) {
                self.release_index(t, x);
            }
        }
        if ix.kind == IndexKind::Xml {
            let users: Vec<&Index> = t.indexes.iter().filter(|x| x.xml.as_ref().and_then(|x| x.using_index.as_deref()).is_some_and(|u| self.same(u, &ix.name))).collect();
            for x in users {
                self.release_index(t, x);
            }
        }
        let table = name.quoted();
        let sql = ddl::drop_index(&table, ix);
        let step = if ix.primary_key || ix.unique_constraint {
            Step::new(Phase::DropKeys, format!("Dropping constraints from {name}"), vec![self.ex.key(&name, &ix.name, ix.primary_key, Check::Drop, &sql)])
        } else {
            let sql = self.ex.index(&name, &ix.name, Check::Drop, &sql);
            match ix.kind {
                IndexKind::Xml => {
                    let secondary = ix.xml.as_ref().is_some_and(|x| x.using_index.is_some());
                    Step { rank: if secondary { 0 } else { 1 }, ..Step::new(Phase::DropXmlIndexes, format!("Dropping XML index [{}] from {name}", ix.name), vec![sql]) }
                }
                IndexKind::Spatial => Step::new(Phase::DropXmlIndexes, format!("Dropping spatial index [{}] from {name}", ix.name), vec![sql]),
                _ => Step::new(Phase::DropIndexes, format!("Dropping index [{}] from {name}", ix.name), vec![sql]),
            }
        };
        self.push(step);
    }

    /// Creates the new table under a temporary name, copies the data, drops the old table and
    /// renames the new one; then keys, indexes, checks, foreign keys and triggers come back.
    fn rebuild_table(&mut self, s: &'a Table, t: &'a Table, pairs: &[(Option<&'a Column>, Option<&'a Column>)]) {
        let name = s.object_name();
        let old = t.object_name();
        let label = name.to_string();
        self.table_feature_warnings(s);
        self.disturbed.insert(old.clone());
        self.recreate.insert(name.clone());
        // A memory-optimized table goes through a disk-based copy: it can't be renamed, and its
        // keys and constraints can't be dropped one by one.
        let memory_optimized = s.memory_optimized || t.memory_optimized;
        // Everything attached to the old table goes with it; foreign keys from other tables are
        // dropped first and come back afterwards.
        if !t.memory_optimized {
            for fk in &t.foreign_keys {
                self.drop_foreign_key(t, fk);
            }
        }
        self.drop_inbound_foreign_keys(t, true);
        self.drop_fulltext_before(t);
        if !t.memory_optimized {
            // Default constraint names are schema-wide, so the old ones are dropped before the new table takes them.
            for c in &t.columns {
                if let Some(df) = &c.default {
                    self.drop_default(&old, &c.name, df);
                }
            }
            // Keys and check constraints are schema-wide names too.
            for ix in t.indexes.iter().filter(|i| i.primary_key || i.unique_constraint) {
                self.release_index(t, ix);
            }
            for ck in &t.check_constraints {
                self.drop_check(&old, &ck.name);
            }
        }
        let temp_name = |p: &mut Self| -> String {
            let n: String = format!("RG_Recovery_{}_{}", p.next_temp, s.name).chars().take(128).collect();
            p.next_temp += 1;
            n
        };
        let tmp_name = temp_name(self);
        let tmp = ObjectName::new(&s.schema, &tmp_name);
        let mut temp_table = s.clone();
        temp_table.indexes.clear();
        temp_table.temporal = None;
        // The period comes back once the copy is renamed.
        for c in temp_table.columns.iter_mut() {
            c.generated_always = None;
            c.hidden = false;
        }
        // The disk-based copy's defaults get temporary names while the real ones are taken.
        // (column, temporary name, real default)
        let mut temp_defaults: Vec<(String, String, DefaultConstraint)> = Vec::new();
        if memory_optimized {
            temp_table.memory_optimized = false;
            temp_table.durability = None;
            temp_table.storage = Storage::default();
            for c in temp_table.columns.iter_mut() {
                if let Some(df) = c.default.as_mut() {
                    let real = df.clone();
                    df.name = temp_name(self);
                    df.system_named = false;
                    temp_defaults.push((c.name.clone(), df.name.clone(), real));
                }
            }
        }
        let mut statements = vec![ddl::create_table_with(&temp_table, Some(&tmp_name), false, &self.r)];

        let mut into = Vec::new();
        let mut select = Vec::new();
        let mut identity = false;
        for (a, b) in pairs {
            match (a, b) {
                (Some(a), Some(b)) => {
                    if a.computed.is_some() || a.column_set || matches!(a.data_type.name.as_str(), "timestamp" | "rowversion") {
                        continue;
                    }
                    if b.computed.is_some() {
                        self.warn(warn::rebuild::computed_not_preserved(&label, &a.name));
                        continue;
                    }
                    let mut expr = quote(&b.name);
                    let (old_type, new_type) = (type_sql(&b.data_type), type_sql(&a.data_type));
                    let class = cast::classify(&b.data_type, &a.data_type);
                    match class {
                        Cast::None => {
                            self.warn(warn::rebuild::cannot_cast(&label, &a.name, &old_type, &new_type));
                            continue;
                        }
                        Cast::Explicit | Cast::Lossy => {
                            if a.data_type.schema.is_some() {
                                self.warn(warn::rebuild::cast_to_non_system_type(&label, &a.name, &new_type));
                            } else {
                                self.warn(match (&a.data_type.xml_collection, a.data_type.name.as_str()) {
                                    (Some(xc), "xml") => warn::xml_schema_change(&label, &a.name, xc),
                                    _ => warn::precision_loss(&label, &a.name, &old_type, &new_type),
                                });
                            }
                            expr = format!("CAST({expr} AS {new_type})");
                            if a.collation.is_some() && a.collation != b.collation && !self.opts.has(Opt::IgnoreCollations) {
                                expr.push_str(&format!(" COLLATE {}", a.collation.as_deref().unwrap_or_default()));
                            }
                        }
                        Cast::Implicit => {
                            let (shorter, unicode) = cast::losses(&b.data_type, &a.data_type);
                            if shorter {
                                self.warn(warn::truncated(&label, &a.name));
                            }
                            if unicode {
                                self.warn(warn::unicode_loss(&label, &a.name));
                            }
                            if !a.nullable
                                && b.nullable
                                && a.identity.is_none()
                                && let Some(df) = &a.default
                            {
                                self.warn(warn::rebuild::null_to_default(&label, &a.name));
                                expr = format!("ISNULL({expr}, {})", df.definition);
                            }
                        }
                    }
                    identity |= a.identity.is_some();
                    into.push(quote(&a.name));
                    select.push(expr);
                }
                (None, Some(b)) if b.computed.is_none() => {
                    self.warn(warn::rebuild::fields_dropped(&old.to_string(), &b.name));
                }
                _ => {}
            }
        }
        let tmp_q = tmp.quoted();
        // Bound defaults go on the new table before the copy, for NOT NULL columns that get no value.
        if !self.opts.has(Opt::IgnoreBindings) {
            for c in &s.columns {
                if let Some(d) = &c.bound_default {
                    statements.push(format!("EXEC sp_bindefault {}, {}", literal(d), literal(&format!("{tmp_q}.{}", quote(&c.name)))));
                }
            }
        }
        if !into.is_empty() {
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {tmp_q} ON"));
            }
            statements.push(format!("INSERT INTO {tmp_q}({}) SELECT {} FROM {}", into.join(", "), select.join(", "), old.quoted()));
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {tmp_q} OFF"));
            }
        }
        let identity_col = s.columns.iter().find(|c| c.identity.is_some());
        if let Some(c) = identity_col
            && !s.memory_optimized
            && t.columns.iter().any(|tc| tc.identity.is_some() && self.same(&tc.name, &c.name))
        {
            statements.push(format!(
                "DECLARE @idVal BIGINT\r\nSELECT @idVal = IDENT_CURRENT({})\r\nIF @idVal IS NOT NULL\r\n    DBCC CHECKIDENT({}, RESEED, @idVal)",
                literal(&old.quoted()),
                literal(&tmp_q)
            ));
        }
        statements.push(format!("DROP TABLE {}", old.quoted()));
        if s.memory_optimized {
            // The final memory-optimized table, filled from the copy.
            statements.push(ddl::create_table_with(s, None, false, &self.r));
            let cols: Vec<String> = s
                .columns
                .iter()
                .filter(|c| c.computed.is_none() && !c.column_set && !matches!(c.data_type.name.as_str(), "timestamp" | "rowversion"))
                .map(|c| quote(&c.name))
                .collect();
            let identity = identity_col.is_some();
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {} ON", name.quoted()));
            }
            statements.push(format!("INSERT INTO {}({}) SELECT {} FROM {tmp_q}", name.quoted(), cols.join(", "), cols.join(", ")));
            if identity {
                statements.push(format!("SET IDENTITY_INSERT {} OFF", name.quoted()));
            }
            statements.push(format!("UPDATE STATISTICS {} WITH FULLSCAN, NORECOMPUTE", name.quoted()));
            statements.push(format!("DROP TABLE {tmp_q}"));
        } else {
            let rename = format!("EXEC sp_rename {}, {}, N'OBJECT'", literal(&tmp_q), literal(&s.name));
            statements.push(self.ex.rename_table(&tmp, &name, &rename));
            // The copy's defaults take their real names back.
            for (column, temp, real) in &temp_defaults {
                statements.push(ddl::drop_constraint(&name.quoted(), temp));
                statements.push(ddl::add_default(&name.quoted(), column, real));
            }
            if let Some(x) = s.temporal.as_ref().filter(|_| self.has(Feature::Temporal)) {
                statements.push(format!("ALTER TABLE {} ADD PERIOD FOR SYSTEM_TIME ({}, {})", name.quoted(), quote(&x.period_start), quote(&x.period_end)));
                for c in s.columns.iter().filter(|c| c.hidden) {
                    statements.push(format!("ALTER TABLE {} ALTER COLUMN {} ADD HIDDEN", name.quoted(), quote(&c.name)));
                }
            }
        }
        if s.lock_escalation != "TABLE" && !s.lock_escalation.is_empty() {
            statements.push(ddl::lock_escalation(&name.quoted(), &s.lock_escalation));
        }
        if let Some(ct) = &s.change_tracking {
            if t.change_tracking.is_none() {
                self.warn(warn::change_tracking_enable(&label));
            }
            statements.push(self.change_tracking_on(&name, ct));
        }
        self.module_step(Phase::CreateObjects, format!("Rebuilding {name}"), statements, &ModuleSettings::default());
        self.table_children(s);
        self.bind_columns(s, None, false);
        self.table_parts(s, None);
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
                t => match objects::xprop_class(t) {
                    Some(class) => class,
                    None => continue,
                },
            };
            let tn = self.target_name(d);
            let scoped = d.object_type.schema_scoped();
            let of = |db: &'a Database, n: &ObjectName| -> Vec<&'a ExtendedProperty> {
                db.extended_properties
                    .iter()
                    .filter(|p| {
                        p.class == class
                            && if class == "SCHEMA" {
                                p.schema.as_deref() == Some(n.name.as_str())
                            } else if !scoped {
                                p.schema.is_none() && p.object.as_deref() == Some(n.name.as_str())
                            } else {
                                p.schema.as_deref() == Some(n.schema.as_str()) && p.object.as_deref() == Some(n.name.as_str())
                            }
                    })
                    .collect()
            };
            let src_props = if d.kind == DiffKind::OnlyIn2 { Vec::new() } else { of(self.src, &d.name) };
            // A rebuilt or re-created object lost its properties with it.
            let gone = d.kind == DiffKind::OnlyIn2
                || self.recreate.contains(&d.name)
                || self.recreate.contains(&d.key())
                || self.dropped.contains(&tn)
                || self.dropped.contains(&d.target_key());
            let tgt_props = if d.kind == DiffKind::OnlyIn1 || gone { Vec::new() } else { of(self.tgt, &tn) };
            let levels = |p: &ExtendedProperty| ddl::xprop_levels(p, |s| s.to_string());
            for sp in &src_props {
                let existing = tgt_props.iter().find(|tp| tp.name == sp.name && tp.minor_type == sp.minor_type && tp.minor_name == sp.minor_name);
                match existing {
                    None => self.add_xprop(sp, &levels(sp)),
                    Some(tp) if tp.value != sp.value => self.update_xprop(sp, &levels(sp)),
                    _ => {}
                }
            }
            if d.kind != DiffKind::OnlyIn2 {
                for tp in &tgt_props {
                    if !src_props.iter().any(|sp| sp.name == tp.name && sp.minor_type == tp.minor_type && sp.minor_name == tp.minor_name) {
                        self.drop_xprop(tp, &levels(tp));
                    }
                }
            }
        }
    }

    // ---- Users, roles and permissions ----

    fn create_user_sql(&mut self, s: &Principal) -> Option<String> {
        let q = quote(&s.name);
        let schema = s.default_schema.as_deref().filter(|x| *x != "dbo").map(|x| format!(" WITH DEFAULT_SCHEMA={}", quote(x))).unwrap_or_default();
        let ex = self.ex;
        let create = |rest: String| ex.principal(&s.name, false, Check::Create, &format!("CREATE USER {q}{rest}{schema}"));
        Some(match (s.kind.as_str(), &s.login) {
            ("SQL_USER", Some(login)) => {
                self.warn(warn::login_may_not_exist(&s.name));
                // Map to the login when the server has it, else create the user without one.
                format!(
                    "DECLARE @associate bit\r\nSELECT @associate = CASE SERVERPROPERTY('EngineEdition') WHEN 5 THEN 1 ELSE 0 END\r\nIF @associate = 0 EXEC sp_executesql {}, N'@count bit OUT', @associate OUT\r\nIF @associate = 1\r\nBEGIN\r\n    PRINT {}\r\n    {}\r\nEND\r\nELSE\r\nBEGIN\r\n    PRINT {}\r\n    {}\r\nEND",
                    ddl::sql_string(&format!("SELECT @count = COUNT(*) FROM master.dbo.syslogins WHERE loginname = {}", ddl::sql_string(login))),
                    ddl::sql_string(&format!("Creating user [{}] and mapping to the login [{login}]", s.name)),
                    create(format!(" FOR LOGIN {}", quote(login))),
                    ddl::sql_string(&format!("Creating user [{}] without login", s.name)),
                    create(" WITHOUT LOGIN".into()),
                )
            }
            ("WINDOWS_USER" | "WINDOWS_GROUP", Some(login)) => create(format!(" FOR LOGIN {}", quote(login))),
            ("SQL_USER" | "WINDOWS_USER" | "WINDOWS_GROUP", None) => {
                if s.name != "guest" {
                    self.warn(warn::user_without_login(&s.name));
                }
                create(" WITHOUT LOGIN".into())
            }
            ("EXTERNAL_USER" | "EXTERNAL_GROUP", _) => create(" FROM EXTERNAL PROVIDER".into()),
            ("CERTIFICATE_MAPPED_USER", _) => create(format!(" FOR CERTIFICATE {}", quote(s.certificate.as_deref().unwrap_or("UNKNOWN")))),
            ("ASYMMETRIC_KEY_MAPPED_USER", _) => create(format!(" FOR ASYMMETRIC KEY {}", quote(s.asymmetric_key.as_deref().unwrap_or("UNKNOWN")))),
            _ => return None,
        })
    }

    fn member_sql(&self, role: &str, member: &str, add: bool) -> String {
        if self.has(Feature::AlterRole) {
            format!("ALTER ROLE {} {} MEMBER {}", quote(role), if add { "ADD" } else { "DROP" }, quote(member))
        } else if add {
            if self.opts.has(Opt::NoErrorHandling) {
                format!("EXEC sp_addrolemember {}, {}", literal(role), literal(member))
            } else {
                format!("DECLARE @returnValue INT\r\nEXEC @returnValue = sp_addrolemember {}, {}\r\nIF @returnValue = 1 SET NOEXEC ON", literal(role), literal(member))
            }
        } else {
            self.ex.principal(role, true, Check::Drop, &format!("EXEC sp_droprolemember {}, {}", literal(role), literal(member)))
        }
    }

    fn principal(&mut self, d: &Difference) {
        let find = |db: &'a Database, n: &str| db.principals.iter().find(|p| p.name == n);
        let tn = self.target_name(d);
        let (s, t) = (find(self.src, &d.name.name), find(self.tgt, &tn.name));
        let q = quote(&d.name.name);
        if let Some(s) = s
            && t.is_none()
        {
            match s.kind.as_str() {
                "DATABASE_ROLE" => {
                    let owner = s.owner.as_deref().map(|o| format!("\r\nAUTHORIZATION {}", quote(o))).unwrap_or_default();
                    let sql = self.ex.principal(&s.name, true, Check::Create, &format!("CREATE ROLE {q}{owner}"));
                    self.step(Phase::CreatePrincipals, format!("Creating role {}", s.name), vec![sql]);
                }
                "APPLICATION_ROLE" => {
                    // The password can't be read; SQL Compare's placeholder is used.
                    self.warn(super::warnings::objects::application_role_password(&q));
                    let schema = s.default_schema.as_deref().map(|x| format!(", DEFAULT_SCHEMA = {}", quote(x))).unwrap_or_default();
                    let sql = self.ex.principal(&s.name, true, Check::Create, &format!("CREATE APPLICATION ROLE {q} WITH PASSWORD = 'p@ssw0rd'{schema}"));
                    self.step(Phase::CreatePrincipals, format!("Creating role {}", s.name), vec![sql]);
                }
                _ => match self.create_user_sql(s) {
                    Some(sql) => self.step(Phase::CreatePrincipals, "", vec![sql]),
                    None => {
                        self.warn(Warning { severity: Severity::High, object: q.clone(), message: format!("{} principals are not scripted yet; create it by hand", s.kind) });
                        return;
                    }
                },
            }
            for r in &s.member_of {
                let sql = self.member_sql(r, &s.name, true);
                self.group(Phase::RoleMembers, format!("Altering members of role {r}"), vec![sql]);
            }
            self.database_permissions(&s.name, &[]);
        } else if let Some(t) = t
            && s.is_none()
        {
            let tq = quote(&t.name);
            if t.is_role() {
                for m in self.tgt.principals.iter().filter(|m| m.member_of.contains(&t.name)) {
                    let sql = self.member_sql(&t.name, &m.name, false);
                    self.group(Phase::RoleMembers, format!("Altering members of role {}", t.name), vec![sql]);
                }
                let what = if t.kind == "APPLICATION_ROLE" { "APPLICATION ROLE" } else { "ROLE" };
                let sql = self.ex.principal(&t.name, true, Check::Drop, &format!("DROP {what} {tq}"));
                self.push(Step { group: true, rank: 0, ..Step::new(Phase::PostTransaction, "Dropping roles", vec![sql]) });
            } else {
                self.warn(warn::user_drop(&t.name));
                let sql = self.ex.principal(&t.name, false, Check::Drop, &format!("DROP USER {tq}"));
                self.push(Step { group: true, rank: 1, ..Step::new(Phase::PostTransaction, "Dropping users", vec![sql]) });
            }
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
            if s.kind == "APPLICATION_ROLE"
                && s.default_schema != t.default_schema
                && let Some(x) = &s.default_schema
            {
                sql.push(format!("ALTER APPLICATION ROLE {q}\r\nWITH DEFAULT_SCHEMA = {}", quote(x)));
            }
            if s.kind == "DATABASE_ROLE" && s.owner != t.owner && !self.opts.has(Opt::IgnoreOwners) {
                sql.push(format!("ALTER AUTHORIZATION ON ROLE::{q} TO {}", quote(s.owner.as_deref().unwrap_or("dbo"))));
            }
            self.step(Phase::CreatePrincipals, "", sql);
            let added: Vec<&String> = s.member_of.iter().filter(|r| !t.member_of.iter().any(|x| self.same(x, r))).collect();
            let removed: Vec<&String> = t.member_of.iter().filter(|r| !s.member_of.iter().any(|x| self.same(x, r))).collect();
            for r in added {
                let sql = self.member_sql(r, &s.name, true);
                self.group(Phase::RoleMembers, format!("Altering members of role {r}"), vec![sql]);
            }
            for r in removed {
                let sql = self.member_sql(r, &s.name, false);
                self.group(Phase::RoleMembers, format!("Altering members of role {r}"), vec![sql]);
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
        self.step(Phase::Permissions, "", revokes.into_iter().chain(grants).collect());
    }

    /// GRANT, DENY and REVOKE for the securables of the selected differences. Objects that are
    /// created, rebuilt or re-created have no permissions yet, so all of the source's are granted
    /// (or, with IgnorePermissions, the target's are restored).
    fn permissions(&mut self, selected: &[&Difference]) {
        let ignore = self.opts.has(Opt::IgnorePermissions);
        let roles_only = self.opts.has(Opt::IgnoreUsers);
        for d in selected {
            let (class, object) = match d.object_type {
                ObjectType::Table
                | ObjectType::View
                | ObjectType::Function
                | ObjectType::StoredProcedure
                | ObjectType::Sequence
                | ObjectType::Synonym
                | ObjectType::Queue
                | ObjectType::SecurityPolicy
                | ObjectType::ExternalTable => ("OBJECT", true),
                ObjectType::Schema => ("SCHEMA", false),
                ObjectType::XmlSchemaCollection => ("XML SCHEMA COLLECTION", true),
                ObjectType::UserDefinedType => ("TYPE", true),
                t => match objects::securable_class(t) {
                    Some(class) => (class, false),
                    None => continue,
                },
            };
            let scoped = d.object_type.schema_scoped() || class == "SCHEMA";
            if d.kind == DiffKind::OnlyIn2 {
                continue;
            }
            let tn = self.target_name(d);
            let on = |db: &'a Database, n: &ObjectName| -> Vec<Permission> {
                let (schema, obj) = match (object, scoped) {
                    (true, _) => (Some(n.schema.as_str()), Some(n.name.as_str())),
                    (false, true) => (Some(n.name.as_str()), None),
                    (false, false) => (None, Some(n.name.as_str())),
                };
                db.permissions
                    .iter()
                    .filter(|p| {
                        p.class == class
                            && p.schema.as_deref() == schema
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
            // Statements name the object as it is called after the deployment, SQL Compare's
            // way: `ON [s].[o]` for objects (with a space where a class prefix would go).
            let securable = match (class, object) {
                ("OBJECT", _) => format!(" {}", d.name.quoted()),
                ("SCHEMA", _) => format!("SCHEMA:: {}", quote(&d.name.name)),
                (class, true) => format!("{class}:: {}", d.name.quoted()),
                (class, false) => format!("{class}:: {}", quote(&d.name.name)),
            };
            let key = |p: &Permission| self.key(&format!("{} {} {}", p.state, p.action(), p.grantee));
            // Column permissions name the column and leave the class prefix out.
            let on = |p: &Permission| if p.column.is_some() { d.name.quoted() } else { securable.clone() };
            let revokes = existing.iter().filter(|e| !wanted.iter().any(|w| key(w) == key(e))).map(|e| {
                let cascade = if e.state == "GRANT_WITH_GRANT_OPTION" { " CASCADE" } else { "" };
                format!("REVOKE {} ON {} TO {}{cascade}", e.action(), on(e), quote(&e.grantee))
            });
            let grants = wanted.iter().filter(|w| !existing.iter().any(|e| key(e) == key(w))).map(|w| match w.state.as_str() {
                "DENY" => format!("DENY {} ON {} TO {}", w.action(), on(w), quote(&w.grantee)),
                "GRANT_WITH_GRANT_OPTION" => format!("GRANT {} ON {} TO {} WITH GRANT OPTION", w.action(), on(w), quote(&w.grantee)),
                _ => format!("GRANT {} ON {} TO {}", w.action(), on(w), quote(&w.grantee)),
            });
            let statements: Vec<String> = revokes.chain(grants).collect();
            self.step(Phase::Permissions, format!("Altering permissions on {securable}"), statements);
        }
    }

    // ---- Ordering and output ----

    /// Joins grouped steps, then ranks the object phases by dependency order.
    fn sort(&mut self) {
        let mut merged: Vec<Step> = Vec::new();
        let mut first: HashMap<(Phase, String), usize> = HashMap::new();
        for step in std::mem::take(&mut self.steps) {
            if step.group {
                if let Some(&i) = first.get(&(step.phase, step.print.clone())) {
                    merged[i].statements.extend(step.statements);
                    continue;
                }
                first.insert((step.phase, step.print.clone()), merged.len());
            }
            merged.push(step);
        }
        let order_src = topo_order(&self.src_deps);
        let order_tgt = topo_order(&self.tgt_deps);
        for step in &mut merged {
            let Some(name) = step.object.clone().or_else(|| step_object(&step.print)) else { continue };
            match step.phase {
                Phase::CreateObjects => step.rank = *order_src.get(&name).unwrap_or(&usize::MAX),
                // Dependants are dropped first.
                Phase::DropObjects => step.rank = usize::MAX - order_tgt.get(&name).copied().unwrap_or(0),
                _ => {}
            }
        }
        // Stable: keeps planning order within equal ranks.
        merged.sort_by_key(|s| (s.phase, s.rank));
        self.steps = merged;
    }

    fn emit(mut self) -> DeploymentScript {
        let o = self.opts;
        let transactions = !o.has(Opt::NoTransactions) && !o.has(Opt::AssumeAlreadyInTransaction);
        let errors = !o.has(Opt::NoErrorHandling);
        let mut w = Writer::new(errors);
        let ignore_set = o.has(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings);
        if !o.has(Opt::DoNotOutputCommentHeader) {
            w.comment(format!(
                "/*\r\nRun this script on:\r\n\r\n        {}    -  This database will be modified\r\n\r\nto synchronize it with:\r\n\r\n        {}\r\n\r\nYou are recommended to back up your database before running this script\r\n\r\nScript created by QuillSQL {} at {} UTC\r\n\r\n*/",
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
            if self.has(Feature::UseStatement) {
                w.batch(format!("USE {}", quote(&self.settings.target_database)));
            } else {
                self.warnings.push(warn::use_unsupported());
            }
        }
        if transactions {
            w.batch("SET XACT_ABORT ON".into());
            w.batch(format!("SET TRANSACTION ISOLATION LEVEL {}", self.settings.transaction_isolation_level));
        }
        let empty = self.steps.is_empty();
        let mut steps = self.steps.iter().peekable();
        while let Some(step) = steps.next_if(|s| s.phase.before_transaction()) {
            w.step(step, ignore_set);
        }
        if transactions {
            w.batch("BEGIN TRANSACTION".into());
        }
        while let Some(step) = steps.next_if(|s| !s.phase.after_transaction()) {
            w.step(step, ignore_set);
        }
        if transactions {
            w.batch("COMMIT TRANSACTION".into());
        }
        w.error_check();
        let mut post = false;
        for step in steps {
            w.step(step, ignore_set);
            post = true;
        }
        if post {
            w.error_check();
        }
        if !o.has(Opt::NoDeploymentLogging) {
            w.batch(LOGGING.replace("{product}", "QuillSQL Compare"));
        }
        if w.any_check {
            let rollback = if transactions { "\tIF @@TRANCOUNT > 0 ROLLBACK TRANSACTION\r\n" } else { "" };
            w.batch(format!(
                "DECLARE @Success AS BIT\r\nSET @Success = 1\r\nSET NOEXEC OFF\r\nIF (@Success = 1) PRINT 'The database update succeeded'\r\nELSE BEGIN\r\n{rollback}\tPRINT 'The database update failed'\r\nEND"
            ));
        }
        let mut warnings = self.warnings;
        warnings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.object.cmp(&b.object)));
        warnings.dedup();
        if o.has(Opt::NoTransactions) {
            warnings.push(warn::no_transactions());
        }
        DeploymentScript { sql: w.text, warnings, batches: if empty { Vec::new() } else { w.batches } }
    }
}

/// Writes the deployment to the SQL Server log for monitoring tools.
const LOGGING: &str = "-- This statement writes to the SQL Server Log so SQL Monitor can show this deployment.\r\nIF HAS_PERMS_BY_NAME(N'sys.xp_logevent', N'OBJECT', N'EXECUTE') = 1\r\nBEGIN\r\n    DECLARE @databaseName AS nvarchar(2048), @eventMessage AS nvarchar(2048)\r\n    SET @databaseName = REPLACE(REPLACE(DB_NAME(), N'\\', N'\\\\'), N'\"', N'\\\"')\r\n    SET @eventMessage = N'{product}: { \"deployment\": { \"description\": \"{product} deployed to ' + @databaseName + N'\", \"database\": \"' + @databaseName + N'\" }}'\r\n    EXECUTE sys.xp_logevent 55000, @eventMessage\r\nEND";

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

    /// As SQL Compare's messages call it.
    fn kind_name(&self) -> &'static str {
        match self {
            Module::View(_) => "view",
            Module::Routine(r) if matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure) => "procedure",
            Module::Routine(_) => "function",
        }
    }
}

/// Column differences an ALTER COLUMN or a rebuild has to deal with: the period of a
/// system-versioned table and sensitivity labels are deployed on their own.
fn material_differences(a: &Column, b: &Column, o: &Options) -> Vec<super::compare::PropDiff> {
    column_differences(a, b, o, false).into_iter().filter(|d| d.name != "GENERATED ALWAYS" && d.name != "Hidden").collect()
}

fn is_clustered(ix: &Index) -> bool {
    matches!(ix.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore)
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

/// Byte range of a trigger's `WITH APPEND` (the words and the space before them).
fn with_append(text: &str) -> Option<std::ops::Range<usize>> {
    use super::text::Tok;
    let tokens = super::text::tokenize(text);
    let mut pos = 0;
    let starts: Vec<usize> = tokens.iter().map(|t| {
        let p = pos;
        pos += t.text.len();
        p
    }).collect();
    let words: Vec<usize> = (0..tokens.len()).filter(|&i| tokens[i].kind == Tok::Word).collect();
    let pair = words.windows(2).find(|w| tokens[w[0]].text.eq_ignore_ascii_case("WITH") && tokens[w[1]].text.eq_ignore_ascii_case("APPEND"))?;
    let from = if pair[0] > 0 && tokens[pair[0] - 1].kind == Tok::Space { starts[pair[0] - 1] } else { starts[pair[0]] };
    Some(from..starts[pair[1]] + tokens[pair[1]].text.len())
}

fn trigger_order(trigger: &ObjectName, order: &str, event: &str) -> String {
    format!("EXEC sp_settriggerorder N'{}', '{}', '{event}', null", trigger.quoted().replace('\'', "''"), title(order))
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
    /// An error check was written, so the script needs the closing success/failure block.
    any_check: bool,
    state: HashMap<&'static str, bool>,
    pending_comment: Option<String>,
}

const ERROR_CHECK: &str = "IF @@ERROR <> 0 SET NOEXEC ON";

impl Writer {
    fn new(errors: bool) -> Self {
        Writer { text: String::new(), batches: Vec::new(), errors, last_was_check: true, any_check: false, state: HashMap::new(), pending_comment: None }
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
        out.push_str(sql.trim_end_matches(['\r', '\n']));
        self.text.push_str(&out.replace("\r\n", "\n").replace('\n', "\r\n"));
        self.text.push_str("\r\nGO\r\n");
        self.batches.push(sql);
        self.last_was_check = false;
    }

    fn step(&mut self, step: &Step, ignore_set: bool) {
        if !step.print.is_empty() {
            self.error_check();
            self.batch(format!("PRINT {}", literal(&step.print)));
        }
        if let Some(s) = &step.settings
            && !ignore_set
        {
            self.set("QUOTED_IDENTIFIER", s.quoted_identifier);
            self.set("ANSI_NULLS", s.ansi_nulls);
        }
        for sql in &step.statements {
            if step.try_catch && self.errors {
                self.batch(ddl::try_catch(sql));
                // The CATCH block stops the script itself.
                self.last_was_check = true;
            } else {
                self.batch(sql.clone());
                self.error_check();
            }
        }
    }

    fn error_check(&mut self) {
        if self.errors && !self.last_was_check && !self.batches.is_empty() {
            self.batch(ERROR_CHECK.into());
            self.last_was_check = true;
            self.any_check = true;
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

    fn col(name: &str, ty: &str) -> Column {
        Column { name: name.into(), data_type: DataType { name: ty.into(), ..Default::default() }, nullable: true, ..Default::default() }
    }

    fn table(name: &str, columns: Vec<Column>) -> Table {
        Table { schema: "dbo".into(), name: name.into(), columns, lock_escalation: "TABLE".into(), ..Default::default() }
    }

    fn db(tables: Vec<Table>) -> Database {
        Database { tables, ..Default::default() }
    }

    fn script(src: &Database, tgt: &Database, opts: &Options) -> DeploymentScript {
        let c = super::super::compare::compare(src, tgt, opts, &Mappings::default());
        let settings = ScriptSettings {
            transaction_isolation_level: "SERIALIZABLE".into(),
            source: "s".into(),
            target: "t".into(),
            target_database: "t".into(),
            target_major_version: 0,
        };
        generate(&c, src, tgt, opts, &Mappings::default(), &settings).unwrap()
    }

    fn has_warning(s: &DeploymentScript, text: &str) -> bool {
        s.warnings.iter().any(|w| w.message.contains(text))
    }

    #[test]
    fn logging_and_cleanup_blocks() {
        let src = db(vec![table("T", vec![col("a", "int")])]);
        let tgt = db(vec![]);
        let s = script(&src, &tgt, &Options::default_set());
        assert!(s.sql.contains("EXECUTE sys.xp_logevent 55000, @eventMessage") && s.sql.contains("DECLARE @Success AS BIT"), "{}", s.sql);
        let s = script(&src, &tgt, &Options::default_set().with(Opt::NoDeploymentLogging));
        assert!(!s.sql.contains("xp_logevent"));
        // Without error checks there is nothing for the closing block to report.
        let s = script(&src, &tgt, &Options::default_set().with(Opt::NoTransactions).with(Opt::NoErrorHandling));
        assert!(!s.sql.contains("@Success") && !s.sql.contains("NOEXEC"), "{}", s.sql);
        assert!(has_warning(&s, "This deployment will run without transactions"));
    }

    #[test]
    fn existence_checks() {
        let mut t = table("T", vec![col("a", "int"), col("b", "int")]);
        t.indexes.push(Index { name: "IX".into(), columns: vec![IndexColumn { name: "a".into(), descending: false }], allow_row_locks: true, allow_page_locks: true, ..Default::default() });
        let mut src = db(vec![t]);
        src.views.push(View { schema: "dbo".into(), name: "V".into(), definition: "CREATE VIEW dbo.V AS SELECT 'x' AS c".into(), ..Default::default() });
        let s = script(&src, &db(vec![]), &Options::default_set().with(Opt::ObjectExistenceChecks));
        assert!(s.sql.contains("IF OBJECT_ID(N'[dbo].[T]', 'U') IS NULL\r\nCREATE TABLE [dbo].[T]"), "{}", s.sql);
        assert!(s.sql.contains("IF NOT EXISTS (SELECT 1 FROM sys.indexes WHERE name = N'IX' AND object_id = OBJECT_ID(N'[dbo].[T]'))\r\nCREATE NONCLUSTERED INDEX [IX]"), "{}", s.sql);
        assert!(s.sql.contains("IF OBJECT_ID(N'[dbo].[V]', 'V') IS NULL\r\nEXEC sp_executesql N'CREATE VIEW [dbo].[V] AS SELECT ''x'' AS c'"), "{}", s.sql);
        // Dropping a column, checked.
        let tgt = db(vec![table("T", vec![col("a", "int"), col("b", "int"), col("c", "int")])]);
        let src = db(vec![table("T", vec![col("a", "int"), col("b", "int")])]);
        let s = script(&src, &tgt, &Options::default_set().with(Opt::ObjectExistenceChecks));
        assert!(s.sql.contains("IF COL_LENGTH(N'[dbo].[T]', N'c') IS NOT NULL\r\nALTER TABLE [dbo].[T] DROP COLUMN [c]"), "{}", s.sql);
        let s = script(&src, &tgt, &Options::default_set());
        assert!(s.sql.contains("ALTER TABLE [dbo].[T] DROP\r\nCOLUMN [c]"), "{}", s.sql);
        assert!(has_warning(&s, "This deployment drops the column(s) [c] from table [dbo].[T]."));
    }

    #[test]
    fn xml_indexes_are_dropped_secondary_first() {
        let key = |n: &str| IndexColumn { name: n.into(), descending: false };
        let pk = Index { name: "PK".into(), kind: IndexKind::Clustered, primary_key: true, unique: true, columns: vec![key("id")], allow_row_locks: true, allow_page_locks: true, ..Default::default() };
        let px = Index { name: "PX".into(), kind: IndexKind::Xml, columns: vec![key("doc")], allow_row_locks: true, allow_page_locks: true, xml: Some(XmlIndex::default()), ..Default::default() };
        let sx = Index {
            name: "SX".into(),
            xml: Some(XmlIndex { kind: XmlIndexKind::Secondary, secondary_type: Some("VALUE".into()), using_index: Some("PX".into()), ..Default::default() }),
            ..px.clone()
        };
        let mut id = col("id", "int");
        id.nullable = false;
        let mut t = table("T", vec![id, col("doc", "xml")]);
        t.indexes = vec![pk.clone(), sx.clone(), px.clone()];
        let tgt = db(vec![t.clone()]);
        t.indexes = vec![pk.clone()];
        let src = db(vec![t.clone()]);
        let s = script(&src, &tgt, &Options::default_set());
        let (a, b) = (s.sql.find("Dropping XML index [SX] from [dbo].[T]").unwrap(), s.sql.find("Dropping XML index [PX] from [dbo].[T]").unwrap());
        assert!(a < b, "{}", s.sql);
        // A changed primary key takes the XML indexes with it and brings them back.
        let mut changed = pk.clone();
        changed.name = "PK2".into();
        t.indexes = vec![changed, px.clone(), sx.clone()];
        let src = db(vec![t]);
        let s = script(&src, &tgt, &Options::default_set());
        let order = ["Dropping XML index [SX]", "Dropping XML index [PX]", "Dropping constraints from [dbo].[T]", "Creating primary key [PK2]", "Creating index [PX]", "Creating index [SX]"];
        let pos: Vec<usize> = order.iter().map(|p| s.sql.find(p).unwrap_or_else(|| panic!("{p}\n{}", s.sql))).collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "{}", s.sql);
        assert!(s.sql.contains("CREATE XML INDEX [SX]\r\nON [dbo].[T] ([doc])\r\nUSING XML INDEX [PX]\r\nFOR VALUE"), "{}", s.sql);
    }

    #[test]
    fn target_version_decides() {
        let view = |text: &str| View { schema: "dbo".into(), name: "V".into(), definition: text.into(), ..Default::default() };
        let mut src = db(vec![]);
        src.views.push(view("CREATE VIEW dbo.V AS SELECT 2 AS c"));
        let mut tgt = db(vec![]);
        tgt.views.push(view("CREATE VIEW dbo.V AS SELECT 1 AS c"));
        let opts = Options::default_set().with(Opt::CreateOrAlterInsteadOfAlter);
        let on = |tgt: &mut Database, version: &str, edition: u32, level: u32| {
            tgt.server = ServerInfo { major_version: version.split('.').next().unwrap().parse().unwrap(), product_version: version.into(), edition: String::new(), engine_edition: edition };
            tgt.compatibility_level = level;
        };
        on(&mut tgt, "13.0.1601.5", 2, 130);
        assert!(script(&src, &tgt, &opts).sql.contains("ALTER VIEW [dbo].[V]"));
        on(&mut tgt, "13.0.4001.0", 2, 130);
        assert!(script(&src, &tgt, &opts).sql.contains("CREATE OR ALTER VIEW [dbo].[V]"));
        on(&mut tgt, "16.0.4135.4", 2, 120);
        assert!(script(&src, &tgt, &opts).sql.contains("CREATE OR ALTER VIEW"));
        assert!(script(&src, &tgt, &opts.clone().with(Opt::UseCompatibilityLevel)).sql.contains("ALTER VIEW [dbo].[V]"));
        // Azure SQL Database has no USE.
        on(&mut tgt, "12.0.2000.8", 5, 150);
        let s = script(&src, &tgt, &opts.clone().with(Opt::AddDatabaseUseStatement));
        assert!(!s.sql.contains("USE [t]") && has_warning(&s, "The use statement is not supported"), "{}", s.sql);
        // Features the target lacks.
        let mut t = table("T", vec![col("a", "int")]);
        t.columns[0].masking_function = Some("default()".into());
        t.indexes.push(Index { name: "CS".into(), kind: IndexKind::ClusteredColumnstore, allow_row_locks: true, allow_page_locks: true, ..Default::default() });
        let src = db(vec![t]);
        let mut tgt = db(vec![]);
        on(&mut tgt, "11.0.7001.0", 2, 110);
        let s = script(&src, &tgt, &Options::default_set().with(Opt::AddOnlineOn));
        assert!(has_warning(&s, "The masking function for column a will be ignored"), "{:?}", s.warnings);
        assert!(has_warning(&s, "The clustered columnstore index CS on table [dbo].[T] is not supported"));
        assert!(!s.sql.contains("MASKED WITH"));
    }

    #[test]
    fn module_encryption() {
        let mut src = db(vec![]);
        src.views.push(View { schema: "dbo".into(), name: "V".into(), definition: "CREATE VIEW dbo.V\r\nAS SELECT 1 AS c".into(), ..Default::default() });
        let s = script(&src, &db(vec![]), &Options::default_set().with(Opt::AddWithEncryption));
        assert!(s.sql.contains("CREATE VIEW [dbo].[V]\r\nWITH ENCRYPTION AS SELECT 1 AS c"), "{}", s.sql);
    }

    #[test]
    fn rebuilds_number_after_recovered_tables() {
        let mut new = col("a", "int");
        new.identity = Some(Identity { seed: "1".into(), increment: "1".into(), not_for_replication: false });
        new.nullable = false;
        let src = db(vec![table("T", vec![new]), table("RG_Recovery_7_T", vec![col("a", "int")])]);
        let mut tgt = db(vec![table("T", vec![col("a", "int")]), table("RG_Recovery_7_T", vec![col("a", "int")])]);
        tgt.tables[0].columns[0].nullable = false;
        let s = script(&src, &tgt, &Options::default_set());
        assert!(s.sql.contains("CREATE TABLE [dbo].[RG_Recovery_8_T]"), "{}", s.sql);
        assert!(has_warning(&s, "recovered from a failed deployment"));
        assert!(has_warning(&s, "Cannot alter IDENTITY property on column [a] on table [dbo].[T]"), "{:?}", s.warnings);
    }

    #[test]
    fn schema_bound_dependants_need_include_dependencies() {
        let mut src = db(vec![table("T", vec![col("a", "bigint")])]);
        let mut tgt = db(vec![table("T", vec![col("a", "int")])]);
        let v = View { schema: "dbo".into(), name: "V".into(), definition: "CREATE VIEW dbo.V WITH SCHEMABINDING AS SELECT a FROM dbo.T".into(), schema_bound: true, ..Default::default() };
        src.views.push(v.clone());
        tgt.views.push(v);
        let dep = Dependency { from: ObjectName::new("dbo", "V"), to: ObjectName::new("dbo", "T"), strong: true };
        src.dependencies.push(dep.clone());
        tgt.dependencies.push(dep);
        let s = script(&src, &tgt, &Options::default_set());
        assert!(s.sql.contains("DROP VIEW [dbo].[V]"), "{}", s.sql);
        let s = script(&src, &tgt, &Options::default_set().without(Opt::IncludeDependencies));
        assert!(!s.sql.contains("DROP VIEW") && has_warning(&s, "aren't included in the deployment:\r\n\r\n[dbo].[V]"), "{}\n{:?}", s.sql, s.warnings);
    }

    #[test]
    fn ddl_triggers_off_during_the_deployment() {
        let src = db(vec![table("T", vec![col("a", "int")])]);
        let mut tgt = db(vec![]);
        tgt.ddl_triggers.push(DdlTrigger { name: "audit".into(), definition: "CREATE TRIGGER audit ON DATABASE FOR CREATE_TABLE AS ROLLBACK".into(), ..Default::default() });
        tgt.ddl_triggers.push(DdlTrigger { name: "off".into(), disabled: true, ..Default::default() });
        let s = script(&src, &tgt, &Options::default_set().with(Opt::DisableAndReenableDdlTriggers));
        let (a, b, c) = (s.sql.find("DISABLE TRIGGER ALL ON DATABASE").unwrap(), s.sql.find("CREATE TABLE").unwrap(), s.sql.find("ENABLE TRIGGER [audit] ON DATABASE").unwrap());
        assert!(a < b && b < c && !s.sql.contains("ENABLE TRIGGER [off]"), "{}", s.sql);
        assert!(!script(&src, &tgt, &Options::default_set()).sql.contains("DISABLE TRIGGER ALL"));
    }

    #[test]
    fn bindings() {
        let mut bound = col("a", "int");
        bound.bound_default = Some("[dbo].[Zero]".into());
        bound.bound_rule = Some("[dbo].[Positive]".into());
        let src = db(vec![table("T", vec![bound])]);
        let tgt = db(vec![table("T", vec![col("a", "int")])]);
        let s = script(&src, &tgt, &Options::default_set());
        assert!(s.sql.contains("EXEC sp_bindefault N'[dbo].[Zero]', N'[dbo].[T].[a]'"), "{}", s.sql);
        assert!(s.sql.contains("EXEC sp_bindrule N'[dbo].[Positive]', N'[dbo].[T].[a]'"), "{}", s.sql);
        assert!(!s.sql.contains("ALTER COLUMN"), "{}", s.sql);
        let s = script(&tgt, &src, &Options::default_set());
        assert!(s.sql.contains("EXEC sp_unbindefault N'[dbo].[T].[a]'") && s.sql.contains("EXEC sp_unbindrule N'[dbo].[T].[a]'"), "{}", s.sql);
        assert!(script(&src, &tgt, &Options::default_set().with(Opt::IgnoreBindings)).is_empty());
    }

    #[test]
    fn synonym_dependencies_can_be_ignored() {
        let mut src = db(vec![]);
        src.synonyms.push(Synonym { schema: "dbo".into(), name: "S".into(), base_object: "[dbo].[T]".into() });
        src.dependencies.push(Dependency { from: ObjectName::new("dbo", "S"), to: ObjectName::new("dbo", "T"), strong: true });
        let settings = ScriptSettings {
            transaction_isolation_level: "SERIALIZABLE".into(),
            source: String::new(),
            target: String::new(),
            target_database: String::new(),
            target_major_version: 0,
        };
        let cmp = Comparison::default();
        let opts = Options::default_set();
        assert_eq!(Planner::new(&cmp, &src, &src, &opts, &Mappings::default(), &settings).src_deps.len(), 1);
        let opts = opts.with(Opt::IgnoreSynonymDependencies);
        assert!(Planner::new(&cmp, &src, &src, &opts, &Mappings::default(), &settings).src_deps.is_empty());
    }

    #[test]
    fn change_tracking_goes_off_while_the_key_changes() {
        let pk = |n: &str, c: &str| Index {
            name: n.into(),
            kind: IndexKind::Clustered,
            primary_key: true,
            unique: true,
            columns: vec![IndexColumn { name: c.into(), descending: false }],
            allow_row_locks: true,
            allow_page_locks: true,
            ..Default::default()
        };
        let mut a = col("a", "int");
        a.nullable = false;
        let mut b = col("b", "int");
        b.nullable = false;
        let mut t = table("T", vec![a, b]);
        t.change_tracking = Some(ChangeTracking { track_columns_updated: false });
        t.indexes = vec![pk("PK", "a")];
        let tgt = db(vec![t.clone()]);
        t.indexes = vec![pk("PK", "b")];
        let src = db(vec![t]);
        let s = script(&src, &tgt, &Options::default_set());
        let order = ["ALTER TABLE [dbo].[T] DISABLE CHANGE_TRACKING", "ALTER TABLE [dbo].[T] DROP CONSTRAINT [PK]", "ADD CONSTRAINT [PK] PRIMARY KEY CLUSTERED ([b])", "ALTER TABLE [dbo].[T] ENABLE CHANGE_TRACKING"];
        let pos: Vec<usize> = order.iter().map(|p| s.sql.find(p).unwrap_or_else(|| panic!("{p}\n{}", s.sql))).collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "{}", s.sql);
        assert!(has_warning(&s, "The change tracking on table [dbo].[T] needs to be disabled and re-enabled."));
    }

    #[test]
    fn with_append_goes_on_new_servers() {
        assert_eq!(with_append("CREATE TRIGGER t ON x FOR INSERT WITH APPEND AS SELECT 1"), Some(32..44));
        assert_eq!(with_append("CREATE TRIGGER t ON x FOR INSERT AS SELECT 1"), None);
    }

    #[test]
    fn recovery_tables() {
        assert_eq!(recovery_number("RG_Recovery_12_Customer"), Some(12));
        assert_eq!(recovery_number("QS_Rebuild_Customer"), Some(0));
        assert_eq!(recovery_number("RG_Recovery_x_Customer"), None);
        assert_eq!(recovery_number("RG_Recovery_3_"), None);
        assert_eq!(recovery_number("Customer"), None);
    }
}
