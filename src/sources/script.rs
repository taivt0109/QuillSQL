//! Object creation scripts in the layout SQL Compare writes into a scripts folder: one file per
//! object, CRLF line endings, `GO` after every batch, constraints and indexes as separate
//! statements after CREATE TABLE, then permissions and extended properties.

use crate::catalog::*;
use crate::db::{literal, quote};
use crate::schema::ddl::{module_sql, qname};
use crate::schema::{Opt, Options};

/// Builds a file batch by batch.
#[derive(Default)]
struct Out {
    s: String,
}

impl Out {
    /// A statement followed by `GO`.
    fn batch(&mut self, sql: &str) {
        self.s.push_str(&crlf(sql));
        self.s.push_str("\r\nGO\r\n");
    }

    /// A statement without `GO` (SQL Compare ends role files this way).
    fn line(&mut self, sql: &str) {
        self.s.push_str(&crlf(sql));
        self.s.push_str("\r\n");
    }

    fn set_options(&mut self, s: &ModuleSettings) {
        self.batch(&format!("SET QUOTED_IDENTIFIER {}", on_off(s.quoted_identifier)));
        self.batch(&format!("SET ANSI_NULLS {}", on_off(s.ansi_nulls)));
    }

    /// A module's text as stored, then `GO` on a line of its own.
    fn module(&mut self, text: &str) {
        self.s.push_str(text);
        if !text.ends_with("\r\n") {
            self.s.push_str("\r\n");
        }
        self.s.push_str("GO\r\n");
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "ON" } else { "OFF" }
}

/// Generated lines use CRLF; LF-only text from the model is converted too.
fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// `[int]`, `[nvarchar] (100)`, `[decimal] (18, 2)`, `[dbo].[Phone]`.
pub fn type_sql(t: &DataType) -> String {
    if let Some(schema) = &t.schema {
        return qname(schema, &t.name);
    }
    if matches!(t.name.as_str(), "geometry" | "geography" | "hierarchyid") {
        return qname("sys", &t.name);
    }
    let mut s = quote(&t.name);
    match t.name.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => match t.length {
            Some(-1) => s.push_str(" (max)"),
            Some(n) => s.push_str(&format!(" ({n})")),
            None => {}
        },
        "decimal" | "numeric" => s.push_str(&format!(" ({}, {})", t.precision.unwrap_or(18), t.scale.unwrap_or(0))),
        "datetime2" | "time" | "datetimeoffset" => {
            if let Some(sc) = t.scale {
                s.push_str(&format!(" ({sc})"));
            }
        }
        "float" => {
            if let Some(p) = t.precision.filter(|p| *p != 53) {
                s.push_str(&format!(" ({p})"));
            }
        }
        "xml" => {
            if let Some(c) = &t.xml_collection {
                s.push_str(&format!(" ({}{c})", if t.xml_document { "DOCUMENT " } else { "CONTENT " }));
            }
        }
        _ => {}
    }
    s
}

/// One column as SQL Compare writes it inside CREATE TABLE.
pub fn column_sql(c: &Column) -> String {
    let mut s = quote(&c.name);
    if let Some(comp) = &c.computed {
        s.push_str(" AS ");
        s.push_str(&super::expr::rewrite(&comp.definition));
        if comp.persisted {
            s.push_str(" PERSISTED");
            if !c.nullable {
                s.push_str(" NOT NULL");
            }
        }
        return s;
    }
    s.push(' ');
    s.push_str(&type_sql(&c.data_type));
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
    if let Some(d) = &c.default {
        s.push_str(&format!(" CONSTRAINT {} DEFAULT {}", quote(&d.name), d.definition));
    }
    s
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

fn key_list(ix: &Index) -> String {
    ix.columns.iter().map(|c| format!("{}{}", quote(&c.name), if c.descending { " DESC" } else { "" })).collect::<Vec<_>>().join(", ")
}

fn compression_sql(st: &Storage) -> Option<String> {
    match st.compression.as_slice() {
        [] => None,
        [one] if one.partition == 0 && one.compression == "COLUMNSTORE" => None,
        [one] if one.partition == 0 => Some(format!("DATA_COMPRESSION = {}", one.compression)),
        many => Some(
            many.iter()
                .map(|p| format!("DATA_COMPRESSION = {} ON PARTITIONS ({})", p.compression, p.partition))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// ` WITH (...)` and ` ON [fg]` for an index or key.
fn index_tail(ix: &Index, o: &Options, default_fg: &str) -> String {
    let mut opts = Vec::new();
    if !o.has(Opt::IgnoreFillFactor) {
        if ix.pad_index {
            opts.push("PAD_INDEX = ON".to_string());
        }
        if ix.fill_factor != 0 {
            opts.push(format!("FILLFACTOR = {}", ix.fill_factor));
        }
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
    if let Some(c) = compression_sql(&ix.storage) {
        opts.push(c);
    }
    let mut s = String::new();
    if !opts.is_empty() {
        s.push_str(&format!(" WITH ({})", opts.join(", ")));
    }
    s.push_str(&storage_sql(&ix.storage, o, default_fg));
    s
}

fn storage_sql(st: &Storage, o: &Options, default_fg: &str) -> String {
    if o.has(Opt::IgnoreFileGroups) {
        return String::new();
    }
    match (&st.data_space, &st.partition_column) {
        (Some(ds), Some(col)) => format!(" ON {} ({})", quote(ds), quote(col)),
        (Some(ds), None) if !ds.eq_ignore_ascii_case(default_fg) => format!(" ON {}", quote(ds)),
        _ => String::new(),
    }
}

/// The PK/UQ clause after `ADD CONSTRAINT [name]`.
fn key_sql(ix: &Index, o: &Options, default_fg: &str) -> String {
    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
    format!("{what} {} ({}){}", kind_words(ix), key_list(ix), index_tail(ix, o, default_fg))
}

/// XML and spatial indexes, as deployments write them.
fn xml_or_spatial_sql(on: &str, ix: &Index, o: &Options) -> String {
    use crate::schema::ddl::{Host, Render, create_index_with};
    let r = Render { opts: o.clone(), features: crate::schema::features::Features::latest() };
    // SQL Compare lists prefixed namespaces before the default one.
    let mut ix = ix.clone();
    if let Some(x) = ix.xml.as_mut() {
        x.namespaces.sort_by_key(|n| n.prefix.is_none());
    }
    create_index_with(on, &ix, &r, &Host::default()).join("\r\nGO\r\n")
}

/// Order of indexes in a file: clustered, other relational, primary XML, other XML, spatial.
fn index_rank(ix: &Index) -> u8 {
    match ix.kind {
        IndexKind::Clustered | IndexKind::ClusteredColumnstore => 0,
        IndexKind::Xml if ix.xml.as_ref().is_none_or(|x| matches!(x.kind, XmlIndexKind::Primary | XmlIndexKind::Selective)) => 2,
        IndexKind::Xml => 3,
        IndexKind::Spatial => 4,
        _ => 1,
    }
}

fn create_index_sql(on: &str, ix: &Index, o: &Options, default_fg: &str) -> String {
    let unique = if ix.unique { "UNIQUE " } else { "" };
    let mut s = format!("CREATE {unique}{} INDEX {} ON {on}", kind_words(ix), quote(&ix.name));
    match ix.kind {
        IndexKind::ClusteredColumnstore => {}
        IndexKind::NonclusteredColumnstore => {
            s.push_str(&format!(" ({})", ix.columns.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ")));
        }
        _ => s.push_str(&format!(" ({})", key_list(ix))),
    }
    if !ix.included.is_empty() {
        s.push_str(&format!(" INCLUDE ({})", ix.included.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(f) = &ix.filter {
        s.push_str(&format!(" WHERE {f}"));
    }
    s.push_str(&index_tail(ix, o, default_fg));
    s
}

/// Permissions on one securable: `GRANT SELECT ON  [dbo].[T] TO [app]`.
fn permissions(out: &mut Out, db: &Database, class: &str, schema: &str, object: Option<&str>, o: &Options) {
    if o.has(Opt::IgnorePermissions) {
        return;
    }
    let on = match class {
        "OBJECT" => format!(" ON  {}", qname(schema, object.unwrap_or_default())),
        _ => format!(" ON SCHEMA:: {}", quote(schema)),
    };
    let mut list: Vec<&Permission> = db
        .permissions
        .iter()
        .filter(|p| p.class == class && p.schema.as_deref() == Some(schema) && p.object.as_deref() == object)
        .collect();
    list.sort_by_key(|p| (p.grantee.to_lowercase(), state_order(&p.state), p.permission.clone()));
    for p in list {
        out.batch(&grant_sql(p, &on));
    }
}

fn state_order(s: &str) -> u8 {
    match s {
        "GRANT" => 0,
        "GRANT_WITH_GRANT_OPTION" => 1,
        _ => 2,
    }
}

fn grant_sql(p: &Permission, on: &str) -> String {
    let (verb, with) = match p.state.as_str() {
        "DENY" => ("DENY", ""),
        "GRANT_WITH_GRANT_OPTION" => ("GRANT", " WITH GRANT OPTION"),
        _ => ("GRANT", ""),
    };
    format!("{verb} {}{on} TO {}{with}", p.permission, quote(&p.grantee))
}

/// `EXEC sp_addextendedproperty N'name', N'value', 'SCHEMA', N'dbo', 'TABLE', N'T', NULL, NULL`.
pub fn xprop_sql(p: &ExtendedProperty) -> String {
    let value = p.value.as_deref().map(literal).unwrap_or_else(|| "NULL".into());
    let mut args = vec![literal(&p.name), value];
    match p.class.as_str() {
        "DATABASE" => args.extend(["NULL", "NULL", "NULL", "NULL", "NULL", "NULL"].map(String::from)),
        "SCHEMA" => {
            args.push("'SCHEMA'".into());
            args.push(literal(p.schema.as_deref().unwrap_or_default()));
            args.extend(["NULL", "NULL", "NULL", "NULL"].map(String::from));
        }
        class => {
            args.push("'SCHEMA'".into());
            args.push(literal(p.schema.as_deref().unwrap_or_default()));
            args.push(format!("'{class}'"));
            args.push(literal(p.object.as_deref().unwrap_or_default()));
            match (&p.minor_type, &p.minor_name) {
                (Some(t), Some(n)) => {
                    args.push(format!("'{t}'"));
                    args.push(literal(n));
                }
                _ => args.extend(["NULL", "NULL"].map(String::from)),
            }
        }
    }
    format!("EXEC sp_addextendedproperty {}", args.join(", "))
}

fn xprops(out: &mut Out, db: &Database, class: &str, schema: &str, object: Option<&str>, o: &Options) {
    if o.has(Opt::IgnoreExtendedProperties) {
        return;
    }
    let mut list: Vec<&ExtendedProperty> = db
        .extended_properties
        .iter()
        .filter(|p| p.class == class && p.schema.as_deref() == Some(schema) && p.object.as_deref() == object)
        .collect();
    // Object-level properties first, then those on columns, constraints, indexes...
    list.sort_by_key(|p| (p.minor_type.is_some(), p.minor_type.clone(), p.minor_name.clone(), p.name.clone()));
    for p in list {
        out.batch(&xprop_sql(p));
    }
}

/// A trigger's text with its name and table written as `[schema].[name]`.
fn trigger_sql(definition: &str, schema: &str, name: &str, on: &str) -> String {
    let text = module_sql(definition, "CREATE", &qname(schema, name));
    // Rewrite the table after ON as well.
    let tokens = crate::schema::text::tokenize(&text);
    use crate::schema::text::Tok;
    let sig: Vec<usize> = (0..tokens.len()).filter(|&i| !matches!(tokens[i].kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).collect();
    let Some(p) = sig.iter().position(|&i| tokens[i].kind == Tok::Word && tokens[i].text.eq_ignore_ascii_case("ON")) else { return text };
    let first = p + 1;
    let mut last = first;
    while sig.get(last + 1).is_some_and(|&i| tokens[i].text == ".") && sig.get(last + 2).is_some() {
        last += 2;
    }
    let (Some(&a), Some(&b)) = (sig.get(first), sig.get(last)) else { return text };
    let mut out = String::with_capacity(text.len());
    for (i, t) in tokens.iter().enumerate() {
        if i == a {
            out.push_str(on);
        } else if i > a && i <= b {
            continue;
        } else {
            out.push_str(&t.text);
        }
    }
    out
}

fn triggers(out: &mut Out, list: &[Trigger], schema: &str, on: &str, o: &Options) {
    if o.has(Opt::IgnoreTriggers) {
        return;
    }
    for tr in list {
        if tr.encrypted || tr.definition.is_empty() {
            continue;
        }
        out.set_options(&ModuleSettings::default());
        out.module(&trigger_sql(&tr.definition, schema, &tr.name, on));
        for (event, order) in &tr.order {
            out.batch(&format!(
                "EXEC sp_settriggerorder N'{}', N'{}', '{event}'",
                qname(schema, &tr.name).replace('\'', "''"),
                title(order)
            ));
        }
        if tr.disabled {
            out.batch(&format!("DISABLE TRIGGER {} ON {on}", qname(schema, &tr.name)));
        }
    }
}

fn title(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub fn table(db: &Database, t: &Table, o: &Options, default_fg: &str) -> String {
    let name = qname(&t.schema, &t.name);
    let mut out = Out::default();
    let mut lines: Vec<String> = t.columns.iter().filter(|c| !is_graph_internal(c)).map(column_sql).collect();
    if let Some(tv) = &t.temporal
        && !tv.period_start.is_empty()
    {
        lines.push(format!("PERIOD FOR SYSTEM_TIME ({}, {})", quote(&tv.period_start), quote(&tv.period_end)));
    }
    // InlineTableObjects: keys, checks and foreign keys inside CREATE TABLE.
    let inline = o.has(Opt::InlineTableObjects) && !t.memory_optimized;
    let nocheck_ok = !o.has(Opt::IgnoreNocheckAndWithNocheck);
    let with_nocheck = |not_trusted: bool| if not_trusted && nocheck_ok && !o.has(Opt::IgnoreWithNocheck) { "WITH NOCHECK " } else { "" };
    let checks: Vec<(&CheckConstraint, String)> = if o.has(Opt::IgnoreChecks) {
        Vec::new()
    } else {
        t.check_constraints
            .iter()
            .map(|ck| {
                let nfr = if ck.not_for_replication { "NOT FOR REPLICATION " } else { "" };
                (ck, format!("CONSTRAINT {} CHECK {nfr}({})", quote(&ck.name), super::expr::rewrite(&ck.definition)))
            })
            .collect()
    };
    let mut keys: Vec<&Index> = if t.memory_optimized { Vec::new() } else { t.indexes.iter().filter(|i| i.primary_key || i.unique_constraint).collect() };
    keys.sort_by_key(|i| (!i.primary_key, i.name.clone()));
    let fks: Vec<(&ForeignKey, String)> = if o.has(Opt::IgnoreKeys) { Vec::new() } else { t.foreign_keys.iter().map(|fk| (fk, foreign_key_sql(fk))).collect() };
    if inline {
        lines.extend(keys.iter().map(|ix| format!("CONSTRAINT {} {}", quote(&ix.name), key_sql(ix, o, default_fg))));
        lines.extend(checks.iter().map(|(_, s)| s.clone()));
        lines.extend(fks.iter().map(|(_, s)| s.clone()));
    }
    if t.memory_optimized {
        for ix in &t.indexes {
            if ix.primary_key || ix.unique_constraint {
                lines.push(format!("CONSTRAINT {} {}", quote(&ix.name), key_sql(ix, o, default_fg)));
            } else {
                let mut s = format!("INDEX {} {} ({})", quote(&ix.name), kind_words(ix), key_list(ix));
                if let Some(b) = ix.bucket_count {
                    s.push_str(&format!(" WITH (BUCKET_COUNT = {b})"));
                }
                lines.push(s);
            }
        }
    }
    let mut s = format!("CREATE TABLE {name}\n(\n{}\n)", lines.join(",\n"));
    if let Some(g) = &t.graph {
        s.push_str(&format!(" AS {g}"));
    }
    if !t.memory_optimized {
        let clustered = t.indexes.iter().any(|i| matches!(i.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore));
        if !clustered {
            s.push_str(&storage_sql(&t.storage, o, default_fg));
        }
        if !o.has(Opt::IgnoreFileGroups) {
            if let Some(fg) = t.text_filegroup.as_ref().filter(|fg| !fg.eq_ignore_ascii_case(default_fg)) {
                s.push_str(&format!(" TEXTIMAGE_ON {}", quote(fg)));
            }
            if let Some(fg) = &t.filestream_filegroup {
                s.push_str(&format!(" FILESTREAM_ON {}", quote(fg)));
            }
        }
    }
    let mut with = Vec::new();
    if !t.memory_optimized
        && let Some(c) = compression_sql(&t.storage)
    {
        with.push(c);
    }
    if t.memory_optimized {
        with.push("MEMORY_OPTIMIZED = ON".into());
        if let Some(d) = &t.durability {
            with.push(format!("DURABILITY = {d}"));
        }
    }
    if let Some(tv) = &t.temporal {
        let mut v = format!("SYSTEM_VERSIONING = ON (HISTORY_TABLE = {}", tv.history_table.quoted());
        if let Some(r) = &tv.retention {
            v.push_str(&format!(", HISTORY_RETENTION_PERIOD = {r}"));
        }
        v.push(')');
        with.push(v);
    }
    if !with.is_empty() {
        s.push_str(&format!("\nWITH\n(\n{}\n)", with.join(",\n")));
    }
    out.batch(&s);
    if t.lock_escalation != "TABLE" && !t.lock_escalation.is_empty() {
        out.batch(&format!("ALTER TABLE {name} SET (LOCK_ESCALATION = {})", t.lock_escalation));
    }
    triggers(&mut out, &t.triggers, &t.schema, &name, o);
    if !inline {
        for (ck, s) in &checks {
            out.batch(&format!("ALTER TABLE {name} {}ADD {s}", with_nocheck(ck.not_trusted)));
        }
    }
    if !t.memory_optimized {
        for ix in keys {
            if !inline {
                out.batch(&format!("ALTER TABLE {name} ADD CONSTRAINT {} {}", quote(&ix.name), key_sql(ix, o, default_fg)));
            }
            if ix.disabled {
                out.batch(&format!("ALTER INDEX {} ON {name} DISABLE", quote(&ix.name)));
            }
        }
        if !o.has(Opt::IgnoreIndexes) {
            let mut rest: Vec<&Index> = t.indexes.iter().filter(|i| !(i.primary_key || i.unique_constraint)).collect();
            rest.sort_by_key(|i| (index_rank(i), i.name.clone()));
            for ix in rest {
                if matches!(ix.kind, IndexKind::Xml | IndexKind::Spatial) {
                    out.module(&xml_or_spatial_sql(&name, ix, o));
                    continue;
                }
                out.batch(&create_index_sql(&name, ix, o, default_fg));
                if ix.disabled {
                    out.batch(&format!("ALTER INDEX {} ON {name} DISABLE", quote(&ix.name)));
                }
            }
        }
    }
    if !inline {
        for (fk, s) in &fks {
            out.batch(&format!("ALTER TABLE {name} {}ADD {s}", with_nocheck(fk.not_trusted)));
        }
    }
    if nocheck_ok {
        // Inline constraints cannot be created WITH NOCHECK: those are disabled afterwards.
        let off = |disabled: bool, not_trusted: bool| disabled || (inline && not_trusted && !o.has(Opt::IgnoreWithNocheck));
        let mut disabled: Vec<&str> = t.foreign_keys.iter().filter(|f| off(f.disabled, f.not_trusted)).map(|f| f.name.as_str()).collect();
        disabled.extend(t.check_constraints.iter().filter(|c| off(c.disabled, c.not_trusted)).map(|c| c.name.as_str()));
        for n in disabled {
            out.batch(&format!("ALTER TABLE {name} NOCHECK CONSTRAINT {}", quote(n)));
        }
    }
    // Defaults and rules bound with sp_bindefault / sp_bindrule.
    for c in &t.columns {
        let column = literal(&format!("{name}.{}", quote(&c.name)));
        if let Some(d) = &c.bound_default {
            out.batch(&format!("EXEC sp_bindefault {}, {column}", literal(d)));
        }
        if let Some(r) = &c.bound_rule {
            out.batch(&format!("EXEC sp_bindrule {}, {column}", literal(r)));
        }
    }
    if let Some(ct) = &t.change_tracking {
        out.batch(&format!(
            "ALTER TABLE {name} ENABLE CHANGE_TRACKING WITH (TRACK_COLUMNS_UPDATED = {})",
            on_off(ct.track_columns_updated)
        ));
    }
    permissions(&mut out, db, "OBJECT", &t.schema, Some(&t.name), o);
    xprops(&mut out, db, "TABLE", &t.schema, Some(&t.name), o);
    out.s
}

/// `CONSTRAINT [n] FOREIGN KEY (...) REFERENCES ...`.
fn foreign_key_sql(fk: &ForeignKey) -> String {
    let mut s = format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        quote(&fk.name),
        fk.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
        fk.referenced_table.quoted(),
        fk.referenced_columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
    );
    for (what, action) in [("DELETE", &fk.on_delete), ("UPDATE", &fk.on_update)] {
        if !action.is_empty() && action != "NO_ACTION" {
            s.push_str(&format!(" ON {what} {}", action.replace('_', " ")));
        }
    }
    if fk.not_for_replication {
        s.push_str(" NOT FOR REPLICATION");
    }
    s
}

fn is_graph_internal(c: &Column) -> bool {
    c.name.starts_with("graph_id_") || c.name.starts_with("from_obj_id_") || c.name.starts_with("from_id_")
        || c.name.starts_with("to_obj_id_") || c.name.starts_with("to_id_") || c.name.starts_with('$')
}

pub fn view(db: &Database, v: &View, o: &Options, default_fg: &str) -> String {
    let name = qname(&v.schema, &v.name);
    let mut out = Out::default();
    if !v.definition.is_empty() {
        out.set_options(&v.settings);
        out.module(&module_sql(&v.definition, "CREATE", &name));
    }
    let mut ixs: Vec<&Index> = v.indexes.iter().collect();
    ixs.sort_by_key(|i| (i.kind != IndexKind::Clustered, i.name.clone()));
    for ix in ixs {
        out.batch(&create_index_sql(&name, ix, o, default_fg));
    }
    triggers(&mut out, &v.triggers, &v.schema, &name, o);
    permissions(&mut out, db, "OBJECT", &v.schema, Some(&v.name), o);
    xprops(&mut out, db, "VIEW", &v.schema, Some(&v.name), o);
    out.s
}

pub fn routine(db: &Database, r: &Routine, o: &Options) -> String {
    let name = qname(&r.schema, &r.name);
    let mut out = Out::default();
    if !r.definition.is_empty() {
        out.set_options(&r.settings);
        out.module(&module_sql(&r.definition, "CREATE", &name));
    }
    permissions(&mut out, db, "OBJECT", &r.schema, Some(&r.name), o);
    let class = if matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure) { "PROCEDURE" } else { "FUNCTION" };
    xprops(&mut out, db, class, &r.schema, Some(&r.name), o);
    out.s
}

pub fn user_type(db: &Database, t: &UserType, o: &Options) -> String {
    let name = qname(&t.schema, &t.name);
    let mut out = Out::default();
    match t.kind {
        UserTypeKind::Alias => {
            let base = t.base_type.as_ref().map(|b| type_sql(b).replacen('[', "", 1).replacen(']', "", 1)).unwrap_or_default();
            out.batch(&format!("CREATE TYPE {name} FROM {base} {}", if t.nullable { "NULL" } else { "NOT NULL" }));
        }
        UserTypeKind::Clr => out.batch(&format!("CREATE TYPE {name} EXTERNAL NAME {}", t.clr_class.clone().unwrap_or_default())),
        UserTypeKind::Table => {
            let mut lines: Vec<String> = t.columns.iter().map(column_sql).collect();
            for ix in &t.indexes {
                let named = if ix.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ix.name)) };
                if ix.primary_key || ix.unique_constraint {
                    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
                    lines.push(format!("{named}{what} {} ({})", kind_words(ix), key_list(ix)));
                } else {
                    lines.push(format!("INDEX {} {} ({})", quote(&ix.name), kind_words(ix), key_list(ix)));
                }
            }
            for ck in &t.check_constraints {
                let named = if ck.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ck.name)) };
                lines.push(format!("{named}CHECK ({})", super::expr::rewrite(&ck.definition)));
            }
            let mut s = format!("CREATE TYPE {name} AS TABLE\n(\n{}\n)", lines.join(",\n"));
            if t.memory_optimized {
                s.push_str("\nWITH (MEMORY_OPTIMIZED = ON)");
            }
            out.batch(&s);
        }
    }
    xprops(&mut out, db, "TYPE", &t.schema, Some(&t.name), o);
    out.s
}

pub fn sequence(db: &Database, s: &Sequence, o: &Options) -> String {
    let mut out = Out::default();
    out.batch(&format!(
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
    ));
    permissions(&mut out, db, "OBJECT", &s.schema, Some(&s.name), o);
    xprops(&mut out, db, "SEQUENCE", &s.schema, Some(&s.name), o);
    out.s
}

pub fn synonym(db: &Database, s: &Synonym, o: &Options) -> String {
    let mut out = Out::default();
    out.batch(&format!("CREATE SYNONYM {} FOR {}", qname(&s.schema, &s.name), s.base_object));
    permissions(&mut out, db, "OBJECT", &s.schema, Some(&s.name), o);
    xprops(&mut out, db, "SYNONYM", &s.schema, Some(&s.name), o);
    out.s
}

pub fn schema(db: &Database, s: &Schema, o: &Options) -> String {
    let mut out = Out::default();
    out.batch(&format!("CREATE SCHEMA {}\nAUTHORIZATION {}", quote(&s.name), quote(&s.owner)));
    permissions(&mut out, db, "SCHEMA", &s.name, None, o);
    if !o.has(Opt::IgnoreExtendedProperties) {
        for p in db.extended_properties.iter().filter(|p| p.class == "SCHEMA" && p.schema.as_deref() == Some(s.name.as_str())) {
            out.batch(&xprop_sql(p));
        }
    }
    out.s
}

/// Members of a role, written in the role's file; also used for fixed roles (db_datareader...).
fn members(out: &mut Out, db: &Database, role: &str) {
    let mut m: Vec<&str> = db.principals.iter().filter(|p| p.member_of.iter().any(|r| r == role)).map(|p| p.name.as_str()).collect();
    m.sort();
    for name in m {
        out.batch(&format!("ALTER ROLE {} ADD MEMBER {}", quote(role), quote(name)));
    }
}

/// Database-level permissions of a principal; SQL Compare ends the file without `GO`.
fn database_permissions(out: &mut Out, db: &Database, grantee: &str, o: &Options) {
    if o.has(Opt::IgnorePermissions) {
        return;
    }
    let mut list: Vec<&Permission> = db.permissions.iter().filter(|p| p.class == "DATABASE" && p.grantee == grantee).collect();
    list.sort_by_key(|p| (state_order(&p.state), p.permission.clone()));
    for p in list {
        out.line(&grant_sql(p, ""));
    }
}

pub fn role(db: &Database, p: &Principal, o: &Options) -> String {
    let mut out = Out::default();
    match p.kind.as_str() {
        "APPLICATION_ROLE" => out.batch(&format!("CREATE APPLICATION ROLE {} WITH PASSWORD = N''", quote(&p.name))),
        _ => out.batch(&format!("CREATE ROLE {}\nAUTHORIZATION {}", quote(&p.name), quote(p.owner.as_deref().unwrap_or("dbo")))),
    }
    members(&mut out, db, &p.name);
    database_permissions(&mut out, db, &p.name, o);
    out.s
}

/// A fixed role's file holds only its members.
pub fn fixed_role(db: &Database, role: &str) -> String {
    let mut out = Out::default();
    members(&mut out, db, role);
    out.s
}

pub fn user(db: &Database, p: &Principal, o: &Options) -> String {
    let mut out = Out::default();
    let mut s = format!("CREATE USER {}", quote(&p.name));
    match (p.kind.as_str(), &p.login) {
        ("EXTERNAL_USER" | "EXTERNAL_GROUP", _) => s.push_str(" FROM EXTERNAL PROVIDER"),
        (_, Some(l)) => s.push_str(&format!(" FOR LOGIN {}", quote(l))),
        (_, None) => s.push_str(" WITHOUT LOGIN"),
    }
    if let Some(ds) = p.default_schema.as_ref().filter(|s| *s != "dbo") {
        s.push_str(&format!(" WITH DEFAULT_SCHEMA={}", quote(ds)));
    }
    out.batch(&s);
    database_permissions(&mut out, db, &p.name, o);
    out.s
}

/// A database DDL trigger: its text, then `DISABLE TRIGGER … ON DATABASE` when disabled.
pub fn ddl_trigger(t: &DdlTrigger) -> String {
    let mut out = Out::default();
    out.set_options(&ModuleSettings::default());
    out.module(&module_sql(&t.definition, "CREATE", &quote(&t.name)));
    if t.disabled {
        out.batch(&format!("DISABLE TRIGGER {} ON DATABASE", quote(&t.name)));
    }
    out.s
}

/// A database-level extended property, in its own file.
pub fn database_property(p: &ExtendedProperty) -> String {
    let mut out = Out::default();
    out.batch(&xprop_sql(p));
    out.s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_and_types() {
        let c = Column {
            name: "Name".into(),
            data_type: DataType { name: "nvarchar".into(), length: Some(100), ..Default::default() },
            collation: Some("Latin1_General_CI_AS".into()),
            ..Default::default()
        };
        assert_eq!(column_sql(&c), "[Name] [nvarchar] (100) COLLATE Latin1_General_CI_AS NOT NULL");
        let d = Column {
            name: "Balance".into(),
            data_type: DataType { name: "decimal".into(), precision: Some(18), scale: Some(2), ..Default::default() },
            default: Some(DefaultConstraint { name: "DF_B".into(), system_named: false, definition: "((0))".into() }),
            identity: None,
            ..Default::default()
        };
        assert_eq!(column_sql(&d), "[Balance] [decimal] (18, 2) NOT NULL CONSTRAINT [DF_B] DEFAULT ((0))");
        let id = Column {
            name: "Id".into(),
            data_type: DataType { name: "int".into(), ..Default::default() },
            identity: Some(Identity { seed: "1".into(), increment: "1".into(), not_for_replication: false }),
            ..Default::default()
        };
        assert_eq!(column_sql(&id), "[Id] [int] NOT NULL IDENTITY(1, 1)");
    }

    #[test]
    fn trigger_header() {
        assert_eq!(
            trigger_sql("CREATE TRIGGER Sales.Order_Audit ON Sales.[Order] AFTER INSERT AS SELECT 1", "Sales", "Order_Audit", "[Sales].[Order]"),
            "CREATE TRIGGER [Sales].[Order_Audit] ON [Sales].[Order] AFTER INSERT AS SELECT 1"
        );
    }

    #[test]
    fn extended_property() {
        let p = ExtendedProperty {
            class: "TABLE".into(),
            schema: Some("dbo".into()),
            object: Some("Customer".into()),
            name: "MS_Description".into(),
            value: Some("People".into()),
            ..Default::default()
        };
        assert_eq!(xprop_sql(&p), "EXEC sp_addextendedproperty N'MS_Description', N'People', 'SCHEMA', N'dbo', 'TABLE', N'Customer', NULL, NULL");
    }
}
