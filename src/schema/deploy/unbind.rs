//! Tables that use a CLR type or a partition scheme that is dropped and created again can't
//! keep their columns or storage while that happens. As SQL Compare does, such a table is
//! rebuilt twice: first, before the drops, into a copy that no longer uses them (CLR columns
//! as nvarchar(4000) through the type's ToString, the partitioned storage on the default
//! filegroup); then, with the other creates, into its final shape (the nvarchar values parsed
//! back into the type).

use std::collections::BTreeSet;

use super::*;
use crate::schema::clr;

/// What the planner needs to know before it starts: worked out from the comparison alone.
#[derive(Default)]
pub(super) struct Prepared {
    /// Assemblies dropped and created again (not altered).
    pub(super) rebuilt_assemblies: BTreeSet<String>,
    /// CLR types re-created with them: `[schema].[name]`.
    pub(super) clr_types: BTreeSet<ObjectName>,
    /// Partition schemes dropped and created again.
    pub(super) schemes: BTreeSet<String>,
    /// The intermediate copy of every target table rebuilt twice.
    pub(super) unbound: Vec<Table>,
}

pub(super) static NOTHING: Prepared =
    Prepared { rebuilt_assemblies: BTreeSet::new(), clr_types: BTreeSet::new(), schemes: BTreeSet::new(), unbound: Vec::new() };

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether an assembly that changes is dropped and created rather than altered.
pub(super) fn assembly_rebuilt(s: &Assembly, t: &Assembly, opts: &Options) -> bool {
    let identity = |a: &Assembly| {
        a.clr_name.split(',').filter(|p| !p.trim().to_ascii_lowercase().starts_with("version=")).map(|p| p.trim().to_ascii_lowercase()).collect::<Vec<_>>()
    };
    let image = |a: &Assembly| a.files.first().and_then(|f| clr::from_hex(&f.content));
    match (image(s), image(t)) {
        _ if opts.has(Opt::UseClrUdtToStringForClrMigration) || identity(s) != identity(t) => true,
        (Some(a), Some(b)) => !clr::alterable(&a, &b),
        _ => true,
    }
}

/// Whether a partition scheme that differs is dropped and created rather than altered.
pub(super) fn scheme_recreated(s: &PartitionScheme, t: &PartitionScheme, src: &Database, tgt: &Database) -> bool {
    let f = |db: &Database, name: &str| db.partition_functions.iter().find(|f| eq(&f.name, name)).cloned();
    let (sf, tf) = (f(src, &s.function), f(tgt, &t.function));
    if !eq(&s.function, &t.function) {
        return true;
    }
    if let (Some(a), Some(b)) = (&sf, &tf)
        && (a.range_right != b.range_right || a.data_type.sql() != b.data_type.sql())
    {
        return true;
    }
    let right = sf.as_ref().is_some_and(|f| f.range_right);
    let partitions = sf.map(|f| f.boundaries.len() + 1).unwrap_or(s.filegroups.len());
    let edge = |x: &PartitionScheme| if right { x.filegroups.first().cloned() } else { x.filegroups.get(partitions.saturating_sub(1)).cloned() };
    edge(s) != edge(t)
}

/// Works out the assemblies, CLR types and partition schemes that are re-created, and the
/// intermediate copies of the tables that use them.
pub(super) fn prepare(cmp: &Comparison, src: &Database, tgt: &Database, opts: &Options) -> Prepared {
    let mut p = Prepared::default();
    let chosen = |ty: ObjectType| cmp.differences.iter().filter(move |d| d.object_type == ty && d.selected && d.kind != DiffKind::Equal);
    for d in chosen(ObjectType::Assembly) {
        let tn = d.target_name.as_ref().unwrap_or(&d.name);
        let s = src.assemblies.iter().find(|a| eq(&a.name, &d.name.name));
        let t = tgt.assemblies.iter().find(|a| eq(&a.name, &tn.name));
        if let (Some(s), Some(t)) = (s, t)
            && assembly_rebuilt(s, t, opts)
        {
            p.rebuilt_assemblies.insert(t.name.clone());
        }
    }
    // An assembly that references a re-created one goes with it.
    loop {
        let more: Vec<String> = tgt
            .assemblies
            .iter()
            .filter(|a| !p.rebuilt_assemblies.contains(&a.name) && a.references.iter().any(|r| p.rebuilt_assemblies.contains(r)))
            .map(|a| a.name.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        p.rebuilt_assemblies.extend(more);
    }
    for ty in tgt.types.iter() {
        let Some(class) = &ty.clr_class else { continue };
        let assembly = class.split("].[").next().unwrap_or_default().trim_start_matches('[');
        if p.rebuilt_assemblies.iter().any(|a| eq(a, &assembly.replace("]]", "]"))) {
            p.clr_types.insert(ObjectName::new(&ty.schema, &ty.name));
        }
    }
    for d in chosen(ObjectType::UserDefinedType) {
        let tn = d.target_name.as_ref().unwrap_or(&d.name);
        if tgt.types.iter().any(|t| t.kind == UserTypeKind::Clr && t.schema == tn.schema && t.name == tn.name) {
            p.clr_types.insert(tn.clone());
        }
    }
    if !opts.has(Opt::IgnoreFileGroups) {
        for d in chosen(ObjectType::PartitionScheme) {
            let tn = d.target_name.as_ref().unwrap_or(&d.name);
            let Some(t) = tgt.partition_schemes.iter().find(|x| eq(&x.name, &tn.name)) else { continue };
            match src.partition_schemes.iter().find(|x| eq(&x.name, &d.name.name)) {
                Some(s) if !scheme_recreated(s, t, src, tgt) => {}
                _ => {
                    p.schemes.insert(t.name.clone());
                }
            }
        }
        // A function that is re-created takes its schemes with it.
        for d in chosen(ObjectType::PartitionFunction) {
            let tn = d.target_name.as_ref().unwrap_or(&d.name);
            let s = src.partition_functions.iter().find(|f| eq(&f.name, &d.name.name));
            let t = tgt.partition_functions.iter().find(|f| eq(&f.name, &tn.name));
            if s.zip(t).is_none_or(|(s, t)| s.range_right != t.range_right || s.data_type.sql() != t.data_type.sql()) {
                p.schemes.extend(tgt.partition_schemes.iter().filter(|x| t.is_some_and(|t| eq(&x.function, &t.name))).map(|x| x.name.clone()));
            }
        }
    }
    // Tables dropped by the deployment need no copy.
    let dropped = |t: &Table| cmp.differences.iter().any(|d| d.object_type == ObjectType::Table && d.selected && d.kind == DiffKind::OnlyIn2 && d.name == t.object_name());
    for t in tgt.tables.iter().filter(|t| !t.history_table && !t.memory_optimized && t.temporal.is_none() && !dropped(t)) {
        let clr_column = |c: &Column| c.data_type.schema.as_ref().is_some_and(|s| p.clr_types.contains(&ObjectName::new(s, &c.data_type.name)));
        let on_scheme = |st: &Storage| st.partition_column.is_some() && st.data_space.as_deref().is_some_and(|ds| p.schemes.iter().any(|s| eq(s, ds)));
        let clr = t.columns.iter().any(clr_column);
        let partitioned = on_scheme(&t.storage) || t.indexes.iter().any(|i| on_scheme(&i.storage));
        if !clr && !partitioned {
            continue;
        }
        let mut copy = Table {
            schema: t.schema.clone(),
            name: t.name.clone(),
            lock_escalation: t.lock_escalation.clone(),
            text_filegroup: t.text_filegroup.clone(),
            filestream_filegroup: t.filestream_filegroup.clone(),
            graph: t.graph.clone(),
            ..Default::default()
        };
        copy.storage = if on_scheme(&t.storage) { Storage::default() } else { Storage { compression: Vec::new(), ..t.storage.clone() } };
        let mut ordinal = 0;
        for c in t.columns.iter().filter(|c| c.computed.is_none()) {
            let mut c = c.clone();
            ordinal += 1;
            c.ordinal = ordinal;
            c.default = None;
            c.bound_default = None;
            c.bound_rule = None;
            c.sensitivity = None;
            if clr_column(&c) {
                c.data_type = DataType { name: "nvarchar".into(), length: Some(4000), ..Default::default() };
                c.collation = Some(tgt.collation.clone()).filter(|x| !x.is_empty());
            }
            copy.columns.push(c);
        }
        p.unbound.push(copy);
    }
    p
}

impl<'a> Planner<'a> {
    /// The intermediate copy of a target table rebuilt twice.
    pub(super) fn unbound_copy(&self, t: &ObjectName) -> Option<&'a Table> {
        self.prepared.unbound.iter().find(|c| c.schema == t.schema && c.name == t.name)
    }

    /// The first rebuild of every table that is rebuilt twice, before the drops.
    pub(super) fn unbind_by_rebuild(&mut self) {
        let tgt = self.tgt;
        let copies: Vec<&'a Table> = self.prepared.unbound.iter().collect();
        for copy in copies {
            let Some(t) = tgt.table(&copy.schema, &copy.name) else { continue };
            let name = t.object_name();
            let label = name.to_string();
            for c in &t.columns {
                if copy.columns.iter().any(|x| x.name == c.name && x.data_type != c.data_type) {
                    self.warn(Warning {
                        severity: Severity::Medium,
                        object: label.clone(),
                        message: format!("The column [{}] on table {label} depends on a CLR type that has been modified. The table must be rebuilt. The data in the table apart from dropped columns will be preserved.", c.name),
                    });
                }
            }
            if t.storage.partition_column.is_some() && copy.storage.partition_column.is_none() {
                self.warn(warn::rebuild::partitioning(&label));
            }
            let pairs = self.column_pairs(copy, t);
            // The copy is a step on the way: its casts and dropped columns are not the user's concern.
            let (warnings, steps) = (self.warnings.len(), self.steps.len());
            self.rebuild_table(copy, t, &pairs);
            self.warnings.truncate(warnings);
            for step in &mut self.steps[steps..] {
                if step.phase == Phase::CreateObjects && step.print == format!("Rebuilding {name}") {
                    step.phase = Phase::UnbindRebuild;
                    step.print = format!("Rebuild table (Unbind CLR types and partition schemes) {name}");
                }
            }
        }
    }

    /// The second rebuild, into the source's shape, of a table that is not deployed itself.
    pub(super) fn rebind_by_rebuild(&mut self) {
        let (src, tgt) = (self.src, self.tgt);
        let copies: Vec<&'a Table> = self.prepared.unbound.iter().collect();
        for copy in copies {
            let name = copy.object_name();
            let deployed = self.cmp.differences.iter().any(|d| d.object_type == ObjectType::Table && d.selected && d.kind != DiffKind::Equal && self.target_name(d) == name);
            if deployed {
                continue;
            }
            let Some(s) = src.table(&name.schema, &name.name).or_else(|| tgt.table(&name.schema, &name.name)) else { continue };
            self.rebuild_from_copy(s, copy);
            // Its grants and extended properties went with the old table.
            self.regrant(ObjectType::Table, &name);
            if !self.opts.has(Opt::IgnoreExtendedProperties) {
                let props: Vec<&ExtendedProperty> =
                    src.extended_properties.iter().filter(|p| p.class == "TABLE" && p.schema.as_deref() == Some(name.schema.as_str()) && p.object.as_deref() == Some(name.name.as_str())).collect();
                for p in props {
                    self.add_xprop(p, &ddl::xprop_levels(p, |x| x.to_string()));
                }
            }
        }
    }

    /// Rebuilds `s` from the intermediate copy.
    pub(super) fn rebuild_from_copy(&mut self, s: &'a Table, copy: &'a Table) {
        let pairs = self.column_pairs(s, copy);
        let warnings = self.warnings.len();
        self.rebuild_table(s, copy, &pairs);
        // Casting the nvarchar copy back into its CLR type is what the copy is for.
        let label = s.object_name().to_string();
        let cast_back: Vec<String> = copy.columns.iter().filter(|c| c.data_type.name == "nvarchar" && c.data_type.length == Some(4000)).map(|c| format!("[{}]", c.name)).collect();
        let kept: Vec<Warning> = self.warnings.drain(warnings..).filter(|w| !(w.object == label && cast_back.iter().any(|c| w.message.contains(c.as_str())))).collect();
        self.warnings.extend(kept);
    }

    /// CLR types of a re-created assembly are dropped and created again with it.
    pub(super) fn recreate_clr_types(&mut self) {
        let (src, tgt) = (self.src, self.tgt);
        let names: Vec<ObjectName> = self.prepared.clr_types.iter().cloned().collect();
        for n in names {
            if self.cmp.differences.iter().any(|d| d.object_type == ObjectType::UserDefinedType && d.selected && d.kind != DiffKind::Equal && self.target_name(d) == n) {
                continue;
            }
            if tgt.types.iter().any(|t| t.schema == n.schema && t.name == n.name) {
                self.disturbed.insert(n.clone());
                let sql = self.ex.user_type(&n, Check::Drop, &format!("DROP TYPE {n}"));
                self.group(Phase::DropTypes, "Dropping types", vec![sql]);
            }
            if let Some(s) = src.types.iter().find(|t| t.schema == n.schema && t.name == n.name) {
                let sql = self.ex.user_type(&n, Check::Create, &ddl::create_type(s));
                self.group(Phase::CreateTypes, "Creating types", vec![sql]);
            }
        }
    }
}
