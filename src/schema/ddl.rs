//! T-SQL for single objects and sub-objects, rendered from the model.

use crate::catalog::*;
use crate::db::{literal, quote};

/// `[schema].[name]`.
pub fn qname(schema: &str, name: &str) -> String {
    format!("{}.{}", quote(schema), quote(name))
}

pub fn column_definition(c: &Column, with_default: bool) -> String {
    let mut s = quote(&c.name);
    if let Some(comp) = &c.computed {
        s.push_str(" AS ");
        s.push_str(&comp.definition);
        if comp.persisted {
            s.push_str(" PERSISTED");
            if !c.nullable {
                s.push_str(" NOT NULL");
            }
        }
        return s;
    }
    s.push(' ');
    s.push_str(&c.data_type.sql());
    if c.column_set {
        s.push_str(" COLUMN_SET FOR ALL_SPARSE_COLUMNS");
    }
    if c.filestream {
        s.push_str(" FILESTREAM");
    }
    if let Some(coll) = &c.collation {
        s.push_str(" COLLATE ");
        s.push_str(coll);
    }
    if c.sparse {
        s.push_str(" SPARSE");
    }
    if let Some(m) = &c.masking_function {
        s.push_str(&format!(" MASKED WITH (FUNCTION = '{}')", m.replace('\'', "''")));
    }
    if let Some(g) = &c.generated_always {
        s.push_str(&format!(" GENERATED ALWAYS AS {g}"));
    }
    if c.hidden {
        s.push_str(" HIDDEN");
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    if let Some(id) = &c.identity {
        s.push_str(&format!(" IDENTITY({}, {})", id.seed, id.increment));
        if id.not_for_replication {
            s.push_str(" NOT FOR REPLICATION");
        }
    }
    if c.rowguidcol {
        s.push_str(" ROWGUIDCOL");
    }
    // System-generated names are kept too, so a later comparison finds the same name.
    if with_default && let Some(d) = &c.default {
        s.push_str(&format!(" CONSTRAINT {} DEFAULT {}", quote(&d.name), d.definition));
    }
    s
}

fn storage_clause(st: &Storage) -> String {
    match (&st.data_space, &st.partition_column) {
        (Some(ds), Some(col)) => format!(" ON {} ({})", quote(ds), quote(col)),
        (Some(ds), None) => format!(" ON {}", quote(ds)),
        _ => String::new(),
    }
}

fn compression_option(st: &Storage) -> Option<String> {
    match st.compression.as_slice() {
        [] => None,
        [one] if one.partition == 0 => Some(format!("DATA_COMPRESSION = {}", one.compression)),
        many => Some(
            many.iter()
                .map(|p| format!("DATA_COMPRESSION = {} ON PARTITIONS ({})", p.compression, p.partition))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn index_options(ix: &Index, include_storage_opts: bool) -> Vec<String> {
    let mut opts = Vec::new();
    if ix.pad_index {
        opts.push("PAD_INDEX = ON".to_string());
    }
    if ix.fill_factor != 0 {
        opts.push(format!("FILLFACTOR = {}", ix.fill_factor));
    }
    if ix.ignore_dup_key {
        opts.push("IGNORE_DUP_KEY = ON".into());
    }
    if ix.no_recompute {
        opts.push("STATISTICS_NORECOMPUTE = ON".into());
    }
    if ix.incremental {
        opts.push("STATISTICS_INCREMENTAL = ON".into());
    }
    if !ix.allow_row_locks {
        opts.push("ALLOW_ROW_LOCKS = OFF".into());
    }
    if !ix.allow_page_locks {
        opts.push("ALLOW_PAGE_LOCKS = OFF".into());
    }
    if ix.optimize_for_sequential_key {
        opts.push("OPTIMIZE_FOR_SEQUENTIAL_KEY = ON".into());
    }
    if let Some(b) = ix.bucket_count {
        opts.push(format!("BUCKET_COUNT = {b}"));
    }
    if include_storage_opts && let Some(c) = compression_option(&ix.storage) {
        opts.push(c);
    }
    opts
}

fn key_list(ix: &Index) -> String {
    ix.columns
        .iter()
        .map(|c| format!("{}{}", quote(&c.name), if c.descending { " DESC" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ")
}

fn kind_words(ix: &Index) -> &'static str {
    match ix.kind {
        IndexKind::Clustered => "CLUSTERED",
        IndexKind::ClusteredColumnstore => "CLUSTERED COLUMNSTORE",
        IndexKind::NonclusteredColumnstore => "NONCLUSTERED COLUMNSTORE",
        IndexKind::NonclusteredHash => "NONCLUSTERED HASH",
        _ => "NONCLUSTERED",
    }
}

/// PK/UQ as a table constraint (inside CREATE TABLE or ALTER TABLE ADD).
pub fn key_constraint(ix: &Index) -> String {
    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
    let mut s = format!("CONSTRAINT {} {what} {} ({})", quote(&ix.name), kind_words(ix), key_list(ix));
    let opts = index_options(ix, true);
    if !opts.is_empty() {
        s.push_str(&format!(" WITH ({})", opts.join(", ")));
    }
    s.push_str(&storage_clause(&ix.storage));
    s
}

/// Statements that create an index or key on `table`.
pub fn create_index(table: &str, ix: &Index) -> Vec<String> {
    let mut out = Vec::new();
    if ix.primary_key || ix.unique_constraint {
        out.push(format!("ALTER TABLE {table} ADD {}", key_constraint(ix)));
    } else {
        let columnstore = matches!(ix.kind, IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore);
        let unique = if ix.unique { "UNIQUE " } else { "" };
        let mut s = format!("CREATE {unique}{} INDEX {} ON {table}", kind_words(ix), quote(&ix.name));
        if ix.kind != IndexKind::ClusteredColumnstore {
            if columnstore {
                s.push_str(&format!(" ({})", ix.columns.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ")));
            } else {
                s.push_str(&format!(" ({})", key_list(ix)));
            }
        }
        if !ix.included.is_empty() {
            s.push_str(&format!(" INCLUDE ({})", ix.included.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")));
        }
        if let Some(f) = &ix.filter {
            s.push_str(&format!(" WHERE {f}"));
        }
        let opts = index_options(ix, true);
        if !opts.is_empty() {
            s.push_str(&format!(" WITH ({})", opts.join(", ")));
        }
        s.push_str(&storage_clause(&ix.storage));
        out.push(s);
    }
    if ix.disabled {
        out.push(format!("ALTER INDEX {} ON {table} DISABLE", quote(&ix.name)));
    }
    out
}

pub fn drop_index(table: &str, ix: &Index) -> String {
    if ix.primary_key || ix.unique_constraint {
        format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote(&ix.name))
    } else {
        format!("DROP INDEX {} ON {table}", quote(&ix.name))
    }
}

pub fn add_foreign_key(table: &str, fk: &ForeignKey, ignore_nocheck: bool) -> Vec<String> {
    let nocheck = fk.not_trusted && !ignore_nocheck;
    let mut s = format!(
        "ALTER TABLE {table}{} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        if nocheck { " WITH NOCHECK" } else { "" },
        quote(&fk.name),
        fk.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
        fk.referenced_table.quoted(),
        fk.referenced_columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
    );
    if fk.on_delete != "NO_ACTION" && !fk.on_delete.is_empty() {
        s.push_str(&format!(" ON DELETE {}", fk.on_delete.replace('_', " ")));
    }
    if fk.on_update != "NO_ACTION" && !fk.on_update.is_empty() {
        s.push_str(&format!(" ON UPDATE {}", fk.on_update.replace('_', " ")));
    }
    if fk.not_for_replication {
        s.push_str(" NOT FOR REPLICATION");
    }
    let mut out = vec![s];
    if fk.disabled && !ignore_nocheck {
        out.push(format!("ALTER TABLE {table} NOCHECK CONSTRAINT {}", quote(&fk.name)));
    }
    out
}

pub fn add_check(table: &str, ck: &CheckConstraint, ignore_nocheck: bool) -> Vec<String> {
    let nocheck = ck.not_trusted && !ignore_nocheck;
    let mut out = vec![format!(
        "ALTER TABLE {table}{} ADD CONSTRAINT {} CHECK {}{}",
        if nocheck { " WITH NOCHECK" } else { "" },
        quote(&ck.name),
        if ck.not_for_replication { "NOT FOR REPLICATION " } else { "" },
        ck.definition
    )];
    if ck.disabled && !ignore_nocheck {
        out.push(format!("ALTER TABLE {table} NOCHECK CONSTRAINT {}", quote(&ck.name)));
    }
    out
}

pub fn add_default(table: &str, column: &str, d: &DefaultConstraint) -> String {
    format!("ALTER TABLE {table} ADD CONSTRAINT {} DEFAULT {} FOR {}", quote(&d.name), d.definition, quote(column))
}

pub fn drop_constraint(table: &str, name: &str) -> String {
    format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote(name))
}

/// CREATE TABLE with columns, defaults and (for memory-optimized tables) inline indexes.
/// Other indexes, keys, checks and foreign keys are created separately.
pub fn create_table(t: &Table, name_override: Option<&str>, inline_keys: bool) -> String {
    let name = qname(&t.schema, name_override.unwrap_or(&t.name));
    let mut lines: Vec<String> = t.columns.iter().filter(|c| !is_graph_internal(c)).map(|c| column_definition(c, true)).collect();
    if let Some(temporal) = &t.temporal
        && !temporal.period_start.is_empty()
    {
        lines.push(format!("PERIOD FOR SYSTEM_TIME ({}, {})", quote(&temporal.period_start), quote(&temporal.period_end)));
    }
    let inline_all = t.memory_optimized;
    for ix in &t.indexes {
        if inline_all {
            let mut s = if ix.primary_key || ix.unique_constraint {
                key_constraint(ix)
            } else {
                format!("INDEX {} {} ({})", quote(&ix.name), kind_words(ix), key_list(ix))
            };
            if !ix.primary_key && !ix.unique_constraint && let Some(b) = ix.bucket_count {
                s.push_str(&format!(" WITH (BUCKET_COUNT = {b})"));
            }
            lines.push(s);
        } else if inline_keys && ix.primary_key {
            lines.push(key_constraint(ix));
        }
    }
    let mut s = format!("CREATE TABLE {name}\n(\n{}\n)", lines.join(",\n"));
    if let Some(g) = &t.graph {
        s.push_str(&format!(" AS {g}"));
    }
    if !t.memory_optimized {
        s.push_str(&storage_clause(&t.storage));
        if let Some(fg) = &t.text_filegroup {
            s.push_str(&format!(" TEXTIMAGE_ON {}", quote(fg)));
        }
        if let Some(fg) = &t.filestream_filegroup {
            s.push_str(&format!(" FILESTREAM_ON {}", quote(fg)));
        }
    }
    let mut with = Vec::new();
    let clustered_inline = inline_keys && t.indexes.iter().any(|i| i.primary_key && i.kind == IndexKind::Clustered);
    if !clustered_inline && !t.indexes.iter().any(|i| i.kind == IndexKind::Clustered || i.kind == IndexKind::ClusteredColumnstore)
        && let Some(c) = compression_option(&t.storage)
    {
        with.push(c);
    }
    if t.memory_optimized {
        with.push("MEMORY_OPTIMIZED = ON".into());
        if let Some(d) = &t.durability {
            with.push(format!("DURABILITY = {d}"));
        }
    }
    if !with.is_empty() {
        s.push_str(&format!("\nWITH\n(\n{}\n)", with.join(",\n")));
    }
    s
}

fn is_graph_internal(c: &Column) -> bool {
    c.name.starts_with("graph_id_") || c.name.starts_with("from_obj_id_") || c.name.starts_with("from_id_")
        || c.name.starts_with("to_obj_id_") || c.name.starts_with("to_id_") || c.name.starts_with("$")
}

/// A module's CREATE text turned into ALTER or CREATE OR ALTER (`verb`), keeping everything else.
pub fn module_with_verb(definition: &str, verb: &str) -> String {
    let tokens = super::text::tokenize(definition);
    let mut out = String::with_capacity(definition.len());
    let mut done = false;
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if !done && t.kind == super::text::Tok::Word && (t.text.eq_ignore_ascii_case("CREATE") || t.text.eq_ignore_ascii_case("ALTER")) {
            // Skip an existing "OR ALTER".
            let mut j = i + 1;
            let next = |from: usize| (from..tokens.len()).find(|&k| tokens[k].kind != super::text::Tok::Space);
            if let Some(k) = next(j)
                && tokens[k].text.eq_ignore_ascii_case("OR")
                && let Some(m) = next(k + 1)
                && tokens[m].text.eq_ignore_ascii_case("ALTER")
            {
                j = m + 1;
            }
            out.push_str(verb);
            done = true;
            i = j;
            continue;
        }
        out.push_str(&t.text);
        i += 1;
    }
    out
}

/// A module's text with `verb` (CREATE, ALTER or CREATE OR ALTER) and the header naming `name`:
/// the stored text of an object renamed with sp_rename still carries its old name.
pub fn module_sql(definition: &str, verb: &str, name: &str) -> String {
    use super::text::Tok;
    let tokens = super::text::tokenize(definition);
    let sig: Vec<usize> = (0..tokens.len())
        .filter(|&i| !matches!(tokens[i].kind, Tok::Space | Tok::LineComment | Tok::BlockComment))
        .collect();
    let Some(p) = sig.iter().position(|&i| tokens[i].text.eq_ignore_ascii_case("CREATE") || tokens[i].text.eq_ignore_ascii_case("ALTER")) else {
        return definition.to_string();
    };
    let mut k = p + 1;
    if sig.get(k).is_some_and(|&i| tokens[i].text.eq_ignore_ascii_case("OR")) && sig.get(k + 1).is_some_and(|&i| tokens[i].text.eq_ignore_ascii_case("ALTER")) {
        k += 2;
    }
    let verb_range = (sig[p], sig[k - 1]);
    let kind = k;
    let first_name = kind + 1;
    let mut last_name = first_name;
    while sig.get(last_name + 1).is_some_and(|&i| tokens[i].text == ".") && sig.get(last_name + 2).is_some() {
        last_name += 2;
    }
    let (Some(&n0), Some(&n1)) = (sig.get(first_name), sig.get(last_name)) else { return definition.to_string() };
    let mut out = String::with_capacity(definition.len() + 16);
    for (i, t) in tokens.iter().enumerate() {
        if i == verb_range.0 {
            out.push_str(verb);
        } else if i > verb_range.0 && i <= verb_range.1 {
            continue;
        } else if i == n0 {
            out.push_str(name);
        } else if i > n0 && i <= n1 {
            continue;
        } else {
            out.push_str(&t.text);
        }
    }
    out
}

pub fn drop_module(kind: &str, name: &str) -> String {
    format!("DROP {kind} {name}")
}

pub fn routine_keyword(r: &Routine) -> &'static str {
    match r.kind {
        RoutineKind::Procedure | RoutineKind::ClrProcedure => "PROCEDURE",
        RoutineKind::ClrAggregate => "AGGREGATE",
        _ => "FUNCTION",
    }
}

pub fn create_sequence(s: &Sequence) -> String {
    format!(
        "CREATE SEQUENCE {}\nAS {}\nSTART WITH {}\nINCREMENT BY {}\nMINVALUE {}\nMAXVALUE {}\n{}\n{}",
        qname(&s.schema, &s.name),
        s.data_type.sql(),
        s.start,
        s.increment,
        s.minimum,
        s.maximum,
        if s.cycle { "CYCLE" } else { "NO CYCLE" },
        match (s.cached, s.cache_size) {
            (false, _) => "NO CACHE".to_string(),
            (true, Some(n)) => format!("CACHE {n}"),
            (true, None) => "CACHE".to_string(),
        }
    )
}

pub fn alter_sequence(a: &Sequence, target: &Sequence) -> Option<String> {
    let mut clauses = Vec::new();
    if a.increment != target.increment {
        clauses.push(format!("INCREMENT BY {}", a.increment));
    }
    if a.minimum != target.minimum {
        clauses.push(format!("MINVALUE {}", a.minimum));
    }
    if a.maximum != target.maximum {
        clauses.push(format!("MAXVALUE {}", a.maximum));
    }
    if a.cycle != target.cycle {
        clauses.push(if a.cycle { "CYCLE".into() } else { "NO CYCLE".into() });
    }
    if a.cached != target.cached || a.cache_size != target.cache_size {
        clauses.push(match (a.cached, a.cache_size) {
            (false, _) => "NO CACHE".into(),
            (true, Some(n)) => format!("CACHE {n}"),
            (true, None) => "CACHE".into(),
        });
    }
    if a.start != target.start {
        clauses.push(format!("RESTART WITH {}", a.start));
    }
    (!clauses.is_empty()).then(|| format!("ALTER SEQUENCE {} {}", qname(&a.schema, &a.name), clauses.join(" ")))
}

pub fn create_synonym(s: &Synonym) -> String {
    format!("CREATE SYNONYM {} FOR {}", qname(&s.schema, &s.name), s.base_object)
}

pub fn create_type(t: &UserType) -> String {
    let name = qname(&t.schema, &t.name);
    match t.kind {
        UserTypeKind::Alias => format!(
            "CREATE TYPE {name} FROM {}{}",
            t.base_type.as_ref().map(DataType::sql).unwrap_or_default(),
            if t.nullable { " NULL" } else { " NOT NULL" }
        ),
        UserTypeKind::Clr => format!("CREATE TYPE {name} EXTERNAL NAME {}", t.clr_class.clone().unwrap_or_default()),
        UserTypeKind::Table => {
            let mut lines: Vec<String> = t.columns.iter().map(|c| column_definition(c, true)).collect();
            for ix in &t.indexes {
                if ix.primary_key || ix.unique_constraint {
                    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
                    let named = if ix.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ix.name)) };
                    lines.push(format!("{named}{what} {} ({})", kind_words(ix), key_list(ix)));
                } else {
                    lines.push(format!("INDEX {} {} ({})", quote(&ix.name), kind_words(ix), key_list(ix)));
                }
            }
            for ck in &t.check_constraints {
                let named = if ck.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ck.name)) };
                lines.push(format!("{named}CHECK {}", ck.definition));
            }
            let mut s = format!("CREATE TYPE {name} AS TABLE\n(\n{}\n)", lines.join(",\n"));
            if t.memory_optimized {
                s.push_str("\nWITH (MEMORY_OPTIMIZED = ON)");
            }
            s
        }
    }
}

/// Arguments that name an extended property's object for the sp_*extendedproperty procedures.
pub fn xprop_levels(p: &ExtendedProperty, schema_map: impl Fn(&str) -> String) -> String {
    let lit = |s: &str| literal(s);
    let mut args = Vec::new();
    match p.class.as_str() {
        "DATABASE" => {}
        "SCHEMA" => args.push(format!("'SCHEMA', {}", lit(&schema_map(p.schema.as_deref().unwrap_or_default())))),
        class => {
            args.push(format!("'SCHEMA', {}", lit(&schema_map(p.schema.as_deref().unwrap_or_default()))));
            args.push(format!("'{class}', {}", lit(p.object.as_deref().unwrap_or_default())));
            if let (Some(t), Some(n)) = (&p.minor_type, &p.minor_name) {
                args.push(format!("'{t}', {}", lit(n)));
            }
        }
    }
    while args.len() < 3 {
        args.push("NULL, NULL".into());
    }
    args.join(", ")
}

pub fn add_xprop(p: &ExtendedProperty, levels: &str) -> String {
    let value = p.value.as_deref().map(literal).unwrap_or_else(|| "NULL".into());
    format!("EXEC sp_addextendedproperty {}, {value}, {levels}", literal(&p.name))
}

pub fn update_xprop(p: &ExtendedProperty, levels: &str) -> String {
    let value = p.value.as_deref().map(literal).unwrap_or_else(|| "NULL".into());
    format!("EXEC sp_updateextendedproperty {}, {value}, {levels}", literal(&p.name))
}

pub fn drop_xprop(p: &ExtendedProperty, levels: &str) -> String {
    format!("EXEC sp_dropextendedproperty {}, {levels}", literal(&p.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_verbs() {
        assert_eq!(module_with_verb("CREATE VIEW v AS SELECT 1", "ALTER"), "ALTER VIEW v AS SELECT 1");
        assert_eq!(module_with_verb("-- c\ncreate or alter proc p as select 1", "ALTER"), "-- c\nALTER proc p as select 1");
        assert_eq!(module_with_verb("CREATE PROCEDURE p AS SELECT 'CREATE'", "CREATE OR ALTER"), "CREATE OR ALTER PROCEDURE p AS SELECT 'CREATE'");
    }

    #[test]
    fn module_rename() {
        assert_eq!(
            module_sql("CREATE PROCEDURE dbo.Old @a int AS SELECT 1", "ALTER", "[dbo].[New]"),
            "ALTER PROCEDURE [dbo].[New] @a int AS SELECT 1"
        );
        assert_eq!(module_sql("create or alter view [s] . [v] as select 1", "CREATE", "[s].[v]"), "CREATE view [s].[v] as select 1");
    }

    #[test]
    fn columns() {
        let c = Column {
            name: "Id".into(),
            data_type: DataType { name: "int".into(), ..Default::default() },
            identity: Some(Identity { seed: "1".into(), increment: "1".into(), not_for_replication: false }),
            ..Default::default()
        };
        assert_eq!(column_definition(&c, true), "[Id] int NOT NULL IDENTITY(1, 1)");
        let d = Column {
            name: "Bal".into(),
            data_type: DataType { name: "decimal".into(), precision: Some(18), scale: Some(2), ..Default::default() },
            nullable: true,
            default: Some(DefaultConstraint { name: "DF_Bal".into(), system_named: false, definition: "((0))".into() }),
            ..Default::default()
        };
        assert_eq!(column_definition(&d, true), "[Bal] decimal(18, 2) NULL CONSTRAINT [DF_Bal] DEFAULT ((0))");
    }
}
