//! Tables paired under different names (a table mapping, or an accepted rename suggestion) are
//! renamed first thing in the transaction, the way SQL Compare does: sp_rename for the name,
//! ALTER SCHEMA … TRANSFER for the schema. The rest of the plan then works on the target as
//! it is after the renames.

use super::*;

/// One table to rename: its target name now and the name it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TableRename {
    pub old: ObjectName,
    pub new: ObjectName,
}

/// The renames the selected table differences ask for. A schema that differs only because of a
/// schema mapping is not transferred.
pub(super) fn table_renames(cmp: &Comparison, src: &Database, tgt: &Database, opts: &Options, mappings: &Mappings) -> Vec<TableRename> {
    let case = opts.has(Opt::CaseSensitiveObjectDefinition);
    let same = |a: &str, b: &str| if case { a == b } else { a.eq_ignore_ascii_case(b) };
    let mut out = Vec::new();
    for d in cmp.differences.iter().filter(|d| d.selected && d.object_type == ObjectType::Table && d.kind == DiffKind::Different) {
        let Some(tn) = &d.target_name else { continue };
        let (Some(s), Some(t)) = (src.table(&d.name.schema, &d.name.name), tgt.table(&tn.schema, &tn.name)) else { continue };
        let mapped_schema = mappings.schemas.iter().any(|(a, b)| same(a, &d.name.schema) && same(b, &tn.schema));
        let schema = if same(&d.name.schema, &tn.schema) || mapped_schema { tn.schema.clone() } else { d.name.schema.clone() };
        let new = ObjectName::new(&schema, &d.name.name);
        if new == *tn {
            continue;
        }
        // The history table of a system-versioned pair goes first.
        if let (Some(x), Some(y)) = (&s.temporal, &t.temporal)
            && (x.history_table.name != y.history_table.name || !same(&x.history_table.schema, &y.history_table.schema))
            && tgt.table(&y.history_table.schema, &y.history_table.name).is_some()
        {
            out.push(TableRename { old: y.history_table.clone(), new: x.history_table.clone() });
        }
        out.push(TableRename { old: tn.clone(), new });
    }
    out
}

/// The comparison and the target as they are once the renames have run.
pub(super) fn after_renames(cmp: &Comparison, tgt: &Database, renames: &[TableRename]) -> (Comparison, Database) {
    let mut cmp = cmp.clone();
    let mut db = tgt.clone();
    let renamed = |n: &ObjectName| renames.iter().find(|r| r.old == *n).map(|r| r.new.clone());
    for d in cmp.differences.iter_mut().filter(|d| d.object_type == ObjectType::Table) {
        if let Some(new) = d.target_name.as_ref().and_then(renamed) {
            d.target_name = (new != d.name).then_some(new);
        }
    }
    for t in db.tables.iter_mut() {
        if let Some(new) = renamed(&t.object_name()) {
            t.schema = new.schema;
            t.name = new.name;
        }
        for fk in t.foreign_keys.iter_mut() {
            if let Some(new) = renamed(&fk.referenced_table) {
                fk.referenced_table = new;
            }
        }
        if let Some(x) = t.temporal.as_mut()
            && let Some(new) = renamed(&x.history_table)
        {
            x.history_table = new;
        }
    }
    for d in db.dependencies.iter_mut() {
        if let Some(new) = renamed(&d.from) {
            d.from = new;
        }
        if let Some(new) = renamed(&d.to) {
            d.to = new;
        }
    }
    let rename_parts = |class: &str, schema: &mut Option<String>, object: &mut Option<String>, wanted: &str| {
        if class != wanted {
            return;
        }
        if let (Some(s), Some(o)) = (schema.as_ref(), object.as_ref())
            && let Some(new) = renamed(&ObjectName::new(s, o))
        {
            *schema = Some(new.schema);
            *object = Some(new.name);
        }
    };
    for p in db.permissions.iter_mut() {
        rename_parts(&p.class.clone(), &mut p.schema, &mut p.object, "OBJECT");
    }
    for p in db.extended_properties.iter_mut() {
        rename_parts(&p.class.clone(), &mut p.schema, &mut p.object, "TABLE");
    }
    (cmp, db)
}

impl<'a> Planner<'a> {
    /// sp_rename and ALTER SCHEMA TRANSFER for each renamed table.
    pub(super) fn rename_tables(&mut self) {
        let renames = std::mem::take(&mut self.renames);
        for r in &renames {
            let mut statements = Vec::new();
            let moved = ObjectName::new(&r.old.schema, &r.new.name);
            if r.old.name != r.new.name {
                let sql = format!("EXEC sp_rename {}, {}, N'OBJECT'", literal(&r.old.quoted()), literal(&r.new.name));
                statements.push(self.ex.rename_table(&r.old, &moved, &sql));
            }
            if r.old.schema != r.new.schema {
                let sql = format!("ALTER SCHEMA {} TRANSFER {}", quote(&r.new.schema), moved.quoted());
                statements.push(self.ex.rename_table(&moved, &r.new, &sql));
            }
            self.step(Phase::RenameTables, "", statements);
        }
        self.renames = renames;
    }
}

/// Warnings for writing the selected differences to a scripts folder rather than a script.
pub fn folder_warnings(cmp: &Comparison, src: &Database) -> Vec<Warning> {
    let mut out = Vec::new();
    for d in cmp.differences.iter().filter(|d| d.selected && d.object_type == ObjectType::User && d.kind != DiffKind::OnlyIn2) {
        if let Some(u) = src.principals.iter().find(|p| p.name == d.name.name)
            && u.login.is_none()
            && matches!(u.kind.as_str(), "SQL_USER" | "WINDOWS_USER" | "WINDOWS_GROUP")
            && !u.name.eq_ignore_ascii_case("guest")
        {
            out.push(warn::user_without_login_folder(&u.name));
        }
    }
    out
}
