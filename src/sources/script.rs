//! Object creation scripts in the layout SQL Compare writes into a scripts folder: one file per
//! object, CRLF line endings, `GO` after every batch, constraints and indexes as separate
//! statements after CREATE TABLE, then permissions and extended properties.

use crate::catalog::*;
use crate::db::{literal, quote};
use crate::schema::ddl::{Render, module_sql, qname};
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

/// `[int]`, `[nvarchar] (100)`, `[decimal] (18, 2)`, `[datetime2] (3)`, `[dbo].[Phone]`.
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
        // The default scale, 7, is not written.
        "datetime2" | "time" | "datetimeoffset" => {
            if let Some(sc) = t.scale.filter(|sc| *sc != 7) {
                s.push_str(&format!(" ({sc})"));
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
        // A column set has no nullability of its own.
        s.push_str(" COLUMN_SET FOR ALL_SPARSE_COLUMNS");
        return s;
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

/// How the folder renders indexes and keys: the deployment's renderer, for the latest server.
fn render(o: &Options) -> Render {
    Render { opts: o.clone(), features: crate::schema::features::Features::latest() }
}

/// Strings in the order .NET's culture-aware comparison (en-US) gives them, which SQL Compare
/// sorts names with: case is ignored first (lower case wins a tie), and punctuation sorts
/// before digits, digits before letters.
pub fn culture_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |c: char| {
        let class = if c.is_alphabetic() { 2 } else if c.is_numeric() { 1 } else { 0 };
        (class, c.to_lowercase().next().unwrap_or(c))
    };
    let primary = a.chars().map(key).cmp(b.chars().map(key));
    primary.then_with(|| a.chars().map(|c| !c.is_lowercase()).cmp(b.chars().map(|c| !c.is_lowercase())))
}

/// An index or a CREATE STATISTICS object of a table, as SQL Compare sorts them in its file.
enum TablePart<'a> {
    Index(&'a Index),
    Statistic(&'a Statistic),
}

impl TablePart<'_> {
    /// Primary key, other relational indexes, primary XML, secondary XML, spatial, statistics.
    fn rank(&self) -> u8 {
        match self {
            TablePart::Index(ix) if ix.primary_key => 1,
            TablePart::Index(ix) => match ix.kind {
                IndexKind::Xml if ix.xml.as_ref().is_some_and(|x| matches!(x.kind, XmlIndexKind::Secondary | XmlIndexKind::SecondarySelective)) => 4,
                IndexKind::Xml => 3,
                IndexKind::Spatial => 5,
                _ => 2,
            },
            TablePart::Statistic(_) => 6,
        }
    }

    fn name(&self) -> &str {
        match self {
            TablePart::Index(ix) => &ix.name,
            TablePart::Statistic(s) => &s.name,
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            TablePart::Index(ix) => ix.columns.iter().map(|c| c.name.as_str()).collect(),
            TablePart::Statistic(s) => s.columns.iter().map(String::as_str).collect(),
        }
    }

    fn included(&self) -> Vec<&str> {
        match self {
            TablePart::Index(ix) => ix.included.iter().map(String::as_str).collect(),
            TablePart::Statistic(_) => Vec::new(),
        }
    }
}

/// Kind first; within a kind, by key columns, then included columns, then name.
fn part_order(a: &TablePart, b: &TablePart) -> std::cmp::Ordering {
    let list = |x: Vec<&str>, y: Vec<&str>| {
        for (p, q) in x.iter().zip(&y) {
            let c = culture_cmp(p, q);
            if c.is_ne() {
                return c;
            }
        }
        x.len().cmp(&y.len())
    };
    a.rank()
        .cmp(&b.rank())
        .then_with(|| if a.name() == b.name() { std::cmp::Ordering::Equal } else { list(a.keys(), b.keys()) })
        .then_with(|| list(a.included(), b.included()))
        .then_with(|| culture_cmp(a.name(), b.name()))
}

/// XML and spatial indexes, as deployments write them.
fn xml_or_spatial_sql(on: &str, ix: &Index, o: &Options) -> String {
    use crate::schema::ddl::{Host, create_index_with};
    // SQL Compare lists prefixed namespaces before the default one.
    let mut ix = ix.clone();
    if let Some(x) = ix.xml.as_mut() {
        x.namespaces.sort_by_key(|n| n.prefix.is_none());
    }
    create_index_with(on, &ix, &render(o), &Host::default()).join("\r\nGO\r\n")
}

/// The class prefix SQL Compare writes before a securable in GRANT … ON: none for objects.
fn class_prefix(class: &str) -> String {
    match class {
        "OBJECT" | "DATABASE" => String::new(),
        "FULLTEXT STOPLIST" | "SEARCH PROPERTY LIST" => String::new(),
        c => format!("{c}::"),
    }
}

/// GRANT / DENY statements on one securable in the order SQL Compare writes them: by grantee,
/// then by permission, the object-level statement before its column-level ones, columns in
/// table order. Object-level statements read `ON  [s].[o]` (an empty class prefix), column-level
/// ones `ON [s].[o]`.
pub fn securable_grants(db: &Database, class: &str, schema: Option<&str>, object: Option<&str>, columns: &[&str], o: &Options) -> Vec<String> {
    if o.has(Opt::IgnorePermissions) {
        return Vec::new();
    }
    let list: Vec<&Permission> = db.permissions.iter().filter(|p| p.class == class && p.schema.as_deref() == schema && p.object.as_deref() == object).collect();
    let name = match (class, schema, object) {
        ("SCHEMA", Some(s), _) => quote(s),
        (_, Some(s), Some(n)) => qname(s, n),
        (_, _, Some(n)) => quote(n),
        _ => String::new(),
    };
    let mut users: Vec<&str> = list.iter().map(|p| p.grantee.as_str()).collect();
    users.sort_by(|a, b| culture_cmp(a, b));
    users.dedup();
    let mut levels: Vec<&str> = list.iter().map(|p| p.permission.as_str()).collect();
    levels.sort_by(|a, b| culture_cmp(a, b));
    levels.dedup();
    let mut out = Vec::new();
    let statement = |p: &Permission, action: String, on: &str| {
        let (verb, with) = match p.state.as_str() {
            "DENY" => ("DENY", ""),
            "GRANT_WITH_GRANT_OPTION" => ("GRANT", " WITH GRANT OPTION"),
            _ => ("GRANT", ""),
        };
        format!("{verb} {action} ON {on} TO {}{with}", quote(&p.grantee))
    };
    for user in &users {
        for level in &levels {
            let of = |p: &&&Permission| p.grantee == *user && p.permission == *level;
            if let Some(p) = list.iter().filter(of).find(|p| p.column.is_none()) {
                out.push(statement(p, p.permission.clone(), &format!("{} {name}", class_prefix(class))));
            }
            let mut cols: Vec<&&Permission> = list.iter().filter(of).filter(|p| p.column.is_some()).collect();
            cols.sort_by_key(|p| columns.iter().position(|c| Some(*c) == p.column.as_deref()).unwrap_or(usize::MAX));
            for p in cols {
                out.push(statement(p, p.action(), &name));
            }
        }
    }
    out
}

fn permissions(out: &mut Out, db: &Database, class: &str, schema: &str, object: Option<&str>, columns: &[&str], o: &Options) {
    for s in securable_grants(db, class, Some(schema), object, columns, o) {
        out.batch(&s);
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
    format!("{verb} {}{on} TO {}{with}", p.action(), quote(&p.grantee))
}

/// `EXEC sp_addextendedproperty N'name', N'value', 'SCHEMA', N'dbo', 'TABLE', N'T', NULL, NULL`.
/// A value that is not a string goes through a variable of its type first
/// (`DECLARE @xp int` / `SELECT @xp=3`); binary values are written in lower case.
pub fn xprop_sql(p: &ExtendedProperty) -> String {
    let binary = p.value_type.as_deref().is_some_and(|t| t.to_ascii_lowercase().ends_with("binary"));
    let p = &ExtendedProperty { value: p.value.as_ref().map(|v| if binary { v.to_ascii_lowercase() } else { v.clone() }), ..p.clone() };
    let variable = crate::schema::ddl::xprop_variable(p);
    let value = match (p.value.as_deref(), p.value_type.as_deref()) {
        (None, _) => "NULL".into(),
        _ if !variable.is_empty() => "@xp".into(),
        (Some(v), Some(t)) if t.eq_ignore_ascii_case("varchar") => format!("'{}'", v.replace('\'', "''")),
        (Some(v), _) => literal(v),
    };
    let mut args = vec![literal(&p.name), value];
    match p.class.as_str() {
        "DATABASE" => args.extend(["NULL", "NULL", "NULL", "NULL", "NULL", "NULL"].map(String::from)),
        "SCHEMA" => {
            args.push("'SCHEMA'".into());
            args.push(literal(p.schema.as_deref().unwrap_or_default()));
            args.extend(["NULL", "NULL", "NULL", "NULL"].map(String::from));
        }
        class if p.schema.is_none() => {
            args.push(format!("'{class}'"));
            args.push(literal(p.object.as_deref().unwrap_or_default()));
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
    format!("{variable}EXEC sp_addextendedproperty {}", args.join(", "))
}

/// `ALTER AUTHORIZATION ON [s].[o] TO [owner]` (`ON TYPE :: [s].[t]` for types) for an object
/// with an explicit owner, unless IgnoreSchemaObjectAuthorization.
pub fn authorization(class: Option<&str>, schema: &str, name: &str, owner: Option<&str>, o: &Options) -> Option<String> {
    let owner = owner.filter(|_| !o.has(Opt::IgnoreSchemaObjectAuthorization))?;
    let class = class.map(|c| format!("{c} :: ")).unwrap_or_default();
    Some(format!("ALTER AUTHORIZATION ON {class}{} TO {}", qname(schema, name), quote(owner)))
}

/// The grants every database gives public: not written.
fn public_default(p: &Permission) -> bool {
    p.grantee == "public" && p.state == "GRANT" && matches!(p.permission.as_str(), "VIEW ANY COLUMN ENCRYPTION KEY DEFINITION" | "VIEW ANY COLUMN MASTER KEY DEFINITION")
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

/// Each trigger: its settings, its text, its order (AFTER triggers: first delete, insert,
/// update, then last ones, as `'first', 'update', null`), then DISABLE TRIGGER.
fn triggers(out: &mut Out, list: &[Trigger], schema: &str, on: &str, o: &Options) {
    if o.has(Opt::IgnoreTriggers) {
        return;
    }
    for tr in list {
        if tr.encrypted || tr.definition.is_empty() {
            continue;
        }
        if (o.has(Opt::IgnoreInsteadOfTriggers) && tr.instead_of) || (o.has(Opt::IgnoreReplicationTriggers) && tr.not_for_replication) {
            continue;
        }
        out.set_options(&tr.settings);
        out.module(&trigger_sql(&tr.definition, schema, &tr.name, on));
        if !tr.instead_of && !o.has(Opt::IgnoreTriggerOrder) {
            for when in ["FIRST", "LAST"] {
                for event in ["DELETE", "INSERT", "UPDATE"] {
                    if tr.order.iter().any(|(e, w)| e.eq_ignore_ascii_case(event) && w.eq_ignore_ascii_case(when)) {
                        out.batch(&format!(
                            "EXEC sp_settriggerorder N'{}', '{}', '{}', null",
                            qname(schema, &tr.name).replace('\'', "''"),
                            when.to_lowercase(),
                            event.to_lowercase()
                        ));
                    }
                }
            }
        }
        if tr.disabled {
            out.batch(&format!("DISABLE TRIGGER {} ON {on}", qname(schema, &tr.name)));
        }
    }
}

/// `ADD SENSITIVITY CLASSIFICATION … WITH (LABEL = '…', …)`: SQL Compare quotes the values
/// without N and leaves the rank out.
fn sensitivity_sql(table: &str, column: &str, s: &Sensitivity) -> String {
    let mut items = Vec::new();
    for (k, v) in [("LABEL", &s.label), ("LABEL_ID", &s.label_id), ("INFORMATION_TYPE", &s.information_type), ("INFORMATION_TYPE_ID", &s.information_type_id)] {
        if let Some(v) = v {
            items.push(format!("{k} = '{v}'"));
        }
    }
    format!("ADD SENSITIVITY CLASSIFICATION TO {table}.{} WITH ({})", quote(column), items.join(", "))
}

/// The CREATE TABLE statement: columns, the keys and indexes that go inside it (all of a
/// memory-optimized table's, the primary key of a system-versioned table, PK/UQ and checks
/// under InlineTableObjects), storage and the WITH options.
fn create_table_sql(t: &Table, o: &Options, inline: bool, default_fg: &str) -> String {
    let name = qname(&t.schema, &t.name);
    let r = render(o);
    let host = crate::schema::ddl::Host::of_table(t);
    let mut lines: Vec<String> = t.columns.iter().filter(|c| !is_graph_internal(c)).map(column_sql).collect();
    if let Some(tv) = &t.temporal
        && !tv.period_start.is_empty()
    {
        lines.push(format!("PERIOD FOR SYSTEM_TIME ({}, {})", tv.period_start, tv.period_end));
    }
    if !o.has(Opt::IgnoreIndexes) || t.memory_optimized {
        for ix in t.indexes.iter().filter(|i| inlined(t, i, inline)) {
            if ix.primary_key || ix.unique_constraint {
                lines.push(crate::schema::ddl::key_constraint_with(ix, &r, &host));
            } else {
                let unique = if ix.unique { "UNIQUE " } else { "" };
                let kind = if ix.kind == IndexKind::NonclusteredHash { "NONCLUSTERED HASH" } else if ix.kind == IndexKind::Clustered { "CLUSTERED" } else { "NONCLUSTERED" };
                let mut s = format!("INDEX {} {unique}{kind} ({})", quote(&ix.name), ix.columns.iter().map(|c| format!("{}{}", quote(&c.name), if c.descending { " DESC" } else { "" })).collect::<Vec<_>>().join(", "));
                if let Some(b) = ix.bucket_count {
                    s.push_str(&format!(" WITH (BUCKET_COUNT={b})"));
                }
                lines.push(s);
            }
        }
    }
    if inline && !o.has(Opt::IgnoreChecks) {
        for ck in &t.check_constraints {
            // SQL Compare leaves system-generated names out here; we keep them, so the folder
            // reads back with the same names.
            let nfr = if ck.not_for_replication && !o.has(Opt::IgnoreNotForReplication) { "NOT FOR REPLICATION " } else { "" };
            lines.push(format!("CONSTRAINT {} CHECK {nfr}({})", quote(&ck.name), super::expr::rewrite(&ck.definition)));
        }
    }
    let mut s = format!("CREATE TABLE {name}\n(\n{}\n)", lines.join(",\n"));
    if let Some(g) = &t.graph {
        s.push_str(&format!(" AS {g}"));
    }
    if !o.has(Opt::IgnoreFileGroups) && !t.memory_optimized {
        match (&t.storage.data_space, &t.storage.partition_column) {
            (Some(ds), Some(col)) => s.push_str(&format!(" ON {} ({})", quote(ds), quote(col))),
            (Some(ds), None) => s.push_str(&format!(" ON {}", quote(ds))),
            _ => {}
        }
        let table_fg = t.storage.data_space.as_deref().filter(|_| t.storage.partition_column.is_none()).unwrap_or(default_fg);
        if let Some(fg) = t.text_filegroup.as_ref().filter(|fg| !fg.eq_ignore_ascii_case(table_fg)) {
            s.push_str(&format!(" TEXTIMAGE_ON {}", quote(fg)));
        }
        if let Some(fg) = &t.filestream_filegroup {
            s.push_str(&format!(" FILESTREAM_ON {}", quote(fg)));
        }
    }
    let mut with = Vec::new();
    let clustered_inline = t.indexes.iter().any(|i| matches!(i.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore) && inlined(t, i, inline));
    if !clustered_inline
        && !t.memory_optimized
        && let Some(c) = crate::schema::ddl::compression_option(&t.storage, &r)
    {
        with.push(c);
    }
    if t.memory_optimized {
        with.push("MEMORY_OPTIMIZED = ON".into());
        if t.durability.as_deref().is_some_and(|d| d.eq_ignore_ascii_case("SCHEMA_ONLY")) {
            with.push("DURABILITY = SCHEMA_ONLY".into());
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
    s
}

/// Indexes written inside CREATE TABLE: all of a memory-optimized table's, the primary key of
/// a system-versioned table, and PK/UQ constraints under InlineTableObjects.
fn inlined(t: &Table, ix: &Index, inline: bool) -> bool {
    t.memory_optimized || (ix.primary_key && t.temporal.is_some()) || (inline && (ix.primary_key || ix.unique_constraint))
}

/// A table's file, in SQL Compare's order: CREATE TABLE (Stretch, owner), triggers, checks and
/// their NOCHECKs, keys, indexes and statistics (sorted as SQL Compare sorts them), foreign keys
/// and their NOCHECKs, change tracking, bindings, permissions, extended properties, lock
/// escalation, large value types, the full-text index and sensitivity classifications. The
/// history table of a system-versioned table goes first, in the same file.
pub fn table(db: &Database, t: &Table, o: &Options, default_fg: &str) -> String {
    let name = qname(&t.schema, &t.name);
    let mut out = Out::default();
    if let Some(h) = t.temporal.as_ref().and_then(|tv| db.tables.iter().find(|x| x.history_table && x.object_name() == tv.history_table)) {
        out.s.push_str(&table(db, h, o, default_fg));
    }
    let inline = o.has(Opt::InlineTableObjects) && !t.memory_optimized;
    let nocheck_ok = !o.has(Opt::IgnoreNocheckAndWithNocheck);
    let with_nocheck = |not_trusted: bool| if not_trusted && nocheck_ok && !o.has(Opt::IgnoreWithNocheck) { "WITH NOCHECK " } else { "" };
    if let Some(ft) = &t.file_table {
        // A FileTable's columns, keys and defaults are fixed.
        let mut s = format!("CREATE TABLE {name} AS FILETABLE");
        if let Some(fg) = &t.storage.data_space {
            s.push_str(&format!(" ON {}", quote(fg)));
        }
        if let Some(fg) = &t.filestream_filegroup {
            s.push_str(&format!(" FILESTREAM_ON {}", quote(fg)));
        }
        s.push_str(&format!("\nWITH\n(\n\tFILETABLE_DIRECTORY = {}, FILETABLE_COLLATE_FILENAME = {}", literal(&ft.directory), ft.filename_collation));
        if let Some(pk) = &ft.primary_key {
            s.push_str(&format!(", FILETABLE_PRIMARY_KEY_CONSTRAINT_NAME = {}", quote(pk)));
        }
        s.push_str("\n)");
        out.batch(&s);
        if let Some(a) = authorization(None, &t.schema, &t.name, t.owner.as_deref(), o) {
            out.batch(&a);
        }
        triggers(&mut out, &t.triggers, &t.schema, &name, o);
        permissions(&mut out, db, "OBJECT", &t.schema, Some(&t.name), &[], o);
        xprops(&mut out, db, "TABLE", &t.schema, Some(&t.name), o);
        return out.s;
    }
    out.batch(&create_table_sql(t, o, inline, default_fg));
    if t.remote_data_archive {
        out.batch(&format!("ALTER TABLE {name} SET (REMOTE_DATA_ARCHIVE = ON (MIGRATION_STATE = PAUSED))"));
    }
    if let Some(a) = authorization(None, &t.schema, &t.name, t.owner.as_deref(), o) {
        out.batch(&a);
    }
    triggers(&mut out, &t.triggers, &t.schema, &name, o);
    // Checks (named ones by name, then system-named ones by text) and edge constraints.
    if !o.has(Opt::IgnoreChecks) {
        let mut checks: Vec<&CheckConstraint> = t.check_constraints.iter().collect();
        checks.sort_by(|a, b| a.system_named.cmp(&b.system_named).then_with(|| if a.system_named { a.definition.cmp(&b.definition) } else { a.name.cmp(&b.name) }));
        if !inline {
            for ck in &checks {
                let nfr = if ck.not_for_replication && !o.has(Opt::IgnoreNotForReplication) { "NOT FOR REPLICATION " } else { "" };
                out.batch(&format!("ALTER TABLE {name} {}ADD CONSTRAINT {} CHECK {nfr}({})", with_nocheck(ck.not_trusted), quote(&ck.name), super::expr::rewrite(&ck.definition)));
            }
            for e in &t.edge_constraints {
                out.batch(&crate::schema::ddl_objects::add_edge_constraint(&t.object_name(), e, false));
            }
        }
        if nocheck_ok {
            // Inline constraints cannot be created WITH NOCHECK: those are disabled afterwards.
            for ck in checks.iter().filter(|c| c.disabled || (inline && c.not_trusted && !o.has(Opt::IgnoreWithNocheck))) {
                out.batch(&format!("ALTER TABLE {name} NOCHECK CONSTRAINT {}", quote(&ck.name)));
            }
        }
    }
    // Keys, indexes and statistics.
    let r = render(o);
    let host = crate::schema::ddl::Host::of_table(t);
    let mut parts: Vec<TablePart> = Vec::new();
    if !o.has(Opt::IgnoreIndexes) {
        parts.extend(
            t.indexes
                .iter()
                .filter(|i| !inlined(t, i, inline))
                .filter(|i| !(o.has(Opt::IgnorePerformanceIndexes) && !(i.primary_key || i.unique_constraint)))
                .map(TablePart::Index),
        );
    }
    if !o.has(Opt::IgnoreStatistics) {
        parts.extend(t.statistics.iter().map(TablePart::Statistic));
    }
    parts.sort_by(part_order);
    for p in parts {
        match p {
            TablePart::Index(ix) if matches!(ix.kind, IndexKind::Xml | IndexKind::Spatial) => out.module(&xml_or_spatial_sql(&name, ix, o)),
            TablePart::Index(ix) => {
                for s in crate::schema::ddl::create_index_with(&name, ix, &r, &host) {
                    out.batch(&s);
                }
            }
            TablePart::Statistic(s) => out.batch(&crate::schema::ddl_objects::create_statistics(&t.object_name(), s, true)),
        }
    }
    if !o.has(Opt::IgnoreKeys) {
        for fk in &t.foreign_keys {
            out.batch(&format!("ALTER TABLE {name} {}ADD {}", with_nocheck(fk.not_trusted), foreign_key_sql(fk, o)));
        }
        if nocheck_ok {
            for fk in t.foreign_keys.iter().filter(|f| f.disabled) {
                out.batch(&format!("ALTER TABLE {name} NOCHECK CONSTRAINT {}", quote(&fk.name)));
            }
        }
    }
    if let Some(ct) = t.change_tracking.as_ref().filter(|_| !o.has(Opt::IgnoreChangeTracking)) {
        let columns = if ct.track_columns_updated { " WITH (TRACK_COLUMNS_UPDATED = ON)" } else { "" };
        out.batch(&format!("ALTER TABLE {name} ENABLE CHANGE_TRACKING{columns}"));
    }
    // Rules and defaults bound with sp_bindrule / sp_bindefault, in that order.
    if !o.has(Opt::IgnoreBindings) {
        for c in &t.columns {
            let column = literal(&format!("{name}.{}", quote(&c.name)));
            if let Some(r) = &c.bound_rule {
                out.batch(&format!("EXEC sp_bindrule {}, {column}", literal(r)));
            }
            if let Some(d) = &c.bound_default {
                out.batch(&format!("EXEC sp_bindefault {}, {column}", literal(d)));
            }
        }
    }
    let columns: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    permissions(&mut out, db, "OBJECT", &t.schema, Some(&t.name), &columns, o);
    xprops(&mut out, db, "TABLE", &t.schema, Some(&t.name), o);
    if !t.lock_escalation.is_empty() && !t.lock_escalation.eq_ignore_ascii_case("TABLE") {
        out.module(&format!("{}\r\n", crate::schema::ddl::lock_escalation(&name, &t.lock_escalation.to_uppercase())));
    }
    if t.large_value_types_out_of_row {
        out.batch(&crate::schema::ddl::large_values_out_of_row(&name, true));
    }
    if let Some(f) = t.fulltext_index.as_ref().filter(|_| !o.has(Opt::IgnoreFullTextIndexing)) {
        for s in crate::schema::ddl_objects::create_fulltext_index(&t.object_name(), f, true, false, true, o.has(Opt::InlineFulltextFields)) {
            out.batch(&s);
        }
    }
    if !o.has(Opt::IgnoreSensitivityClassification) {
        for c in &t.columns {
            if let Some(s) = &c.sensitivity {
                out.batch(&sensitivity_sql(&name, &c.name, s));
            }
        }
    }
    out.s
}

/// A file of statements, each followed by `GO`.
pub fn statements(list: &[String]) -> String {
    let mut out = Out::default();
    for s in list {
        out.batch(s);
    }
    out.s
}

/// `CONSTRAINT [n] FOREIGN KEY (...) REFERENCES ...`.
fn foreign_key_sql(fk: &ForeignKey, o: &Options) -> String {
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
    if fk.not_for_replication && !o.has(Opt::IgnoreNotForReplication) {
        s.push_str(" NOT FOR REPLICATION");
    }
    s
}

fn is_graph_internal(c: &Column) -> bool {
    c.name.starts_with("graph_id_") || c.name.starts_with("from_obj_id_") || c.name.starts_with("from_id_")
        || c.name.starts_with("to_obj_id_") || c.name.starts_with("to_id_") || c.name.starts_with('$')
}

/// A view's file: its text and owner, triggers, indexes (clustered, then nonclustered, then
/// statistics), permissions, extended properties and the full-text index.
pub fn view(db: &Database, v: &View, o: &Options, _default_fg: &str) -> String {
    let name = qname(&v.schema, &v.name);
    let mut out = Out::default();
    if !v.definition.is_empty() {
        out.set_options(&v.settings);
        out.module(&module_sql(&v.definition, "CREATE", &name));
    }
    if let Some(a) = authorization(None, &v.schema, &v.name, v.owner.as_deref(), o) {
        out.batch(&a);
    }
    triggers(&mut out, &v.triggers, &v.schema, &name, o);
    if !o.has(Opt::IgnoreIndexes) {
        let r = render(o);
        let host = crate::schema::ddl::Host::view();
        let mut ixs: Vec<&Index> = v.indexes.iter().filter(|i| !(o.has(Opt::IgnorePerformanceIndexes) && !(i.primary_key || i.unique_constraint))).collect();
        ixs.sort_by_key(|i| i.kind != IndexKind::Clustered);
        for ix in ixs {
            for s in crate::schema::ddl::create_index_with(&name, ix, &r, &host) {
                out.batch(&s);
            }
        }
        if !o.has(Opt::IgnoreStatistics) {
            for s in &v.statistics {
                out.batch(&crate::schema::ddl_objects::create_statistics(&ObjectName::new(&v.schema, &v.name), s, true));
            }
        }
    }
    let columns: Vec<&str> = v.columns.iter().map(|c| c.name.as_str()).collect();
    permissions(&mut out, db, "OBJECT", &v.schema, Some(&v.name), &columns, o);
    xprops(&mut out, db, "VIEW", &v.schema, Some(&v.name), o);
    if let Some(f) = v.fulltext_index.as_ref().filter(|_| !o.has(Opt::IgnoreFullTextIndexing)) {
        for s in crate::schema::ddl_objects::create_fulltext_index(&ObjectName::new(&v.schema, &v.name), f, true, false, true, o.has(Opt::InlineFulltextFields)) {
            out.batch(&s);
        }
    }
    out.s
}

pub fn routine(db: &Database, r: &Routine, o: &Options) -> String {
    let name = qname(&r.schema, &r.name);
    let mut out = Out::default();
    if r.clr_method.is_some() {
        // CLR modules have no session settings: both are written OFF.
        out.set_options(&ModuleSettings { ansi_nulls: false, quoted_identifier: false });
        out.module(&clr_routine(r));
    } else if !r.definition.is_empty() {
        out.set_options(&r.settings);
        out.module(&module_sql(&r.definition, "CREATE", &name));
    }
    if let Some(a) = authorization(None, &r.schema, &r.name, r.owner.as_deref(), o) {
        out.batch(&a);
    }
    permissions(&mut out, db, "OBJECT", &r.schema, Some(&r.name), &[], o);
    let class = if matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure) { "PROCEDURE" } else { "FUNCTION" };
    xprops(&mut out, db, class, &r.schema, Some(&r.name), o);
    out.s
}

/// A parameter or return type as SQL Compare writes it for CLR modules: `[nvarchar] (50)`,
/// `[decimal] (10, 2)`, `[dbo].[Point]`.
fn clr_type(t: &str) -> String {
    let t = t.trim();
    if t.starts_with('[') {
        return t.to_string();
    }
    let (base, args) = match t.split_once('(') {
        Some((b, a)) => (b.trim(), Some(a.trim_end_matches(')').trim())),
        None => (t, None),
    };
    let mut s = quote(base);
    match (base.to_ascii_lowercase().as_str(), args) {
        ("datetime2" | "time" | "datetimeoffset", Some("7")) | (_, None) => {}
        ("decimal" | "numeric", Some(a)) => s.push_str(&format!(" ({})", a.split(',').map(str::trim).collect::<Vec<_>>().join(", "))),
        (_, Some(a)) => s.push_str(&format!(" ({})", if a.eq_ignore_ascii_case("max") || a == "-1" { "max" } else { a })),
    }
    s
}

/// CREATE of a CLR procedure, function or aggregate, ending with a line break.
fn clr_routine(r: &Routine) -> String {
    let name = qname(&r.schema, &r.name);
    let method = r.clr_method.clone().unwrap_or_default();
    let params = r
        .parameters
        .iter()
        .map(|p| format!("{} {}{}{}", p.name, clr_type(&p.data_type), p.default.as_ref().map(|d| format!("={d}")).unwrap_or_default(), if p.output { " OUTPUT" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ");
    let execute_as = match r.execute_as.as_deref() {
        None => "CALLER".to_string(),
        Some(x @ ("OWNER" | "SELF")) => x.to_string(),
        Some(u) => literal(u),
    };
    match r.kind {
        RoutineKind::ClrProcedure => {
            let params = if r.parameters.is_empty() { String::new() } else { format!(" ({params})") };
            format!("CREATE PROCEDURE {name}{params}\r\nWITH EXECUTE AS {execute_as}\r\nAS EXTERNAL NAME {method}\r\n")
        }
        RoutineKind::ClrAggregate => format!("CREATE AGGREGATE {name} ({params})\r\nRETURNS {}\r\nEXTERNAL NAME {method}\r\n", clr_type(r.returns.as_deref().unwrap_or_default())),
        _ => {
            let returns = if r.kind == RoutineKind::ClrTableFunction {
                let cols: Vec<String> = r
                    .result_columns
                    .iter()
                    .map(|c| column_sql(&Column { identity: None, default: None, rowguidcol: false, ..c.clone() }))
                    .collect();
                format!("TABLE (\r\n{})", cols.join(",\r\n"))
            } else {
                clr_type(r.returns.as_deref().unwrap_or_default())
            };
            let null_input = if r.null_on_null_input { ", \r\nRETURNS NULL ON NULL INPUT" } else { "" };
            format!("CREATE FUNCTION {name} ({params})\r\nRETURNS {returns}\r\nWITH EXECUTE AS {execute_as}{null_input}\r\nEXTERNAL NAME {method}\r\n")
        }
    }
}

pub fn user_type(db: &Database, t: &UserType, o: &Options) -> String {
    let name = qname(&t.schema, &t.name);
    let mut out = Out::default();
    match t.kind {
        UserTypeKind::Alias => {
            let base = t.base_type.as_ref().map(|b| type_sql(b).replacen('[', "", 1).replacen(']', "", 1)).unwrap_or_default();
            out.batch(&format!("CREATE TYPE {name} FROM {base} {}", if t.nullable { "NULL" } else { "NOT NULL" }));
        }
        UserTypeKind::Clr => out.batch(&format!("CREATE TYPE {name}\nEXTERNAL NAME {}", t.clr_class.clone().unwrap_or_default())),
        UserTypeKind::Table => {
            // Constraint names are not written in a table type, defaults' included.
            let mut lines: Vec<String> = t
                .columns
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    let default = c.default.take();
                    let mut s = column_sql(&c);
                    if let Some(d) = default {
                        s.push_str(&format!(" DEFAULT {}", d.definition));
                    }
                    s
                })
                .collect();
            if !o.has(Opt::IgnoreIndexes) || t.memory_optimized {
                let host = crate::schema::ddl::Host { table_type: true, memory_optimized: t.memory_optimized, ..Default::default() };
                // The primary key first, then other indexes, then unique constraints.
                let mut ixs: Vec<&Index> = t.indexes.iter().collect();
                ixs.sort_by_key(|i| if i.primary_key { 0 } else if i.unique_constraint { 2 } else { 1 });
                for ix in ixs {
                    if ix.primary_key || ix.unique_constraint {
                        let unnamed = Index { system_named: true, ..ix.clone() };
                        lines.push(crate::schema::ddl::key_constraint_with(&unnamed, &render(&o.clone().with(Opt::IgnoreSystemNamedConstraintNames)), &host));
                    } else {
                        let unique = if ix.unique { "UNIQUE " } else { "" };
                        let kind = if ix.kind == IndexKind::NonclusteredHash { "NONCLUSTERED HASH" } else if ix.kind == IndexKind::Clustered { "CLUSTERED" } else { "NONCLUSTERED" };
                        let mut s = format!("INDEX {} {unique}{kind} ({})", quote(&ix.name), ix.columns.iter().map(|c| format!("{}{}", quote(&c.name), if c.descending { " DESC" } else { "" })).collect::<Vec<_>>().join(", "));
                        if let Some(b) = ix.bucket_count {
                            s.push_str(&format!(" WITH (BUCKET_COUNT={b})"));
                        }
                        lines.push(s);
                    }
                }
            }
            if !o.has(Opt::IgnoreChecks) {
                let mut checks: Vec<String> = t.check_constraints.iter().map(|ck| super::expr::rewrite(&ck.definition)).collect();
                checks.sort();
                lines.extend(checks.into_iter().map(|c| format!("CHECK ({c})")));
            }
            let mut s = format!("CREATE TYPE {name} AS TABLE\n(\n{}\n)", lines.join(",\n"));
            if t.memory_optimized {
                s.push_str("\nWITH\n(\nMEMORY_OPTIMIZED = ON\n)");
            }
            out.batch(&s);
        }
    }
    if let Some(a) = authorization(Some("TYPE"), &t.schema, &t.name, t.owner.as_deref(), o) {
        out.batch(&a);
    }
    permissions(&mut out, db, "TYPE", &t.schema, Some(&t.name), &[], o);
    xprops(&mut out, db, "TYPE", &t.schema, Some(&t.name), o);
    if !o.has(Opt::IgnoreBindings) {
        if let Some(r) = &t.bound_rule {
            out.batch(&format!("EXEC sp_bindrule {}, {}", literal(r), literal(&name)));
        }
        if let Some(d) = &t.bound_default {
            out.batch(&format!("EXEC sp_bindefault {}, {}", literal(d), literal(&name)));
        }
    }
    out.s
}

pub fn sequence(db: &Database, s: &Sequence, o: &Options) -> String {
    let mut out = Out::default();
    out.batch(&format!(
        "CREATE SEQUENCE {}\nAS {}\nSTART WITH {}\nINCREMENT BY {}\nMINVALUE {}\nMAXVALUE {}\n{}\n{}",
        qname(&s.schema, &s.name),
        match s.data_type.name.to_ascii_lowercase().as_str() {
            "decimal" | "numeric" => format!("{}({},{})", s.data_type.name, s.data_type.precision.unwrap_or(18), s.data_type.scale.unwrap_or(0)),
            _ => s.data_type.name.clone(),
        },
        s.start,
        s.increment,
        s.minimum,
        s.maximum,
        if s.cycle { "CYCLE" } else { "NO CYCLE" },
        match (s.cached, s.cache_size) {
            (false, _) => "NO CACHE".to_string(),
            (true, Some(n)) => format!("CACHE {n}"),
            (true, None) => "CACHE ".to_string(),
        }
    ));
    if let Some(a) = authorization(None, &s.schema, &s.name, s.owner.as_deref(), o) {
        out.batch(&a);
    }
    permissions(&mut out, db, "OBJECT", &s.schema, Some(&s.name), &[], o);
    xprops(&mut out, db, "SEQUENCE", &s.schema, Some(&s.name), o);
    out.s
}

pub fn synonym(db: &Database, s: &Synonym, o: &Options) -> String {
    let mut out = Out::default();
    out.batch(&format!("CREATE SYNONYM {} FOR {}", qname(&s.schema, &s.name), s.base_object));
    if let Some(a) = authorization(None, &s.schema, &s.name, s.owner.as_deref(), o) {
        out.batch(&a);
    }
    permissions(&mut out, db, "OBJECT", &s.schema, Some(&s.name), &[], o);
    xprops(&mut out, db, "SYNONYM", &s.schema, Some(&s.name), o);
    out.s
}

pub fn schema(db: &Database, s: &Schema, o: &Options) -> String {
    let mut out = Out::default();
    // dbo exists in every database: its file holds only its permissions and properties.
    if s.name != "dbo" {
        out.batch(&format!("CREATE SCHEMA {}\nAUTHORIZATION {}", quote(&s.name), quote(&s.owner)));
    }
    permissions(&mut out, db, "SCHEMA", &s.name, None, &[], o);
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
    let mut list: Vec<&Permission> = db.permissions.iter().filter(|p| p.class == "DATABASE" && p.grantee == grantee && !public_default(p)).collect();
    list.sort_by_key(|p| (state_order(&p.state), p.permission.clone()));
    for p in list {
        out.line(&grant_sql(p, ""));
    }
}

pub fn role(db: &Database, p: &Principal, o: &Options) -> String {
    let mut out = Out::default();
    match p.kind.as_str() {
        "APPLICATION_ROLE" => {
            let schema = p.default_schema.as_deref().map(|s| format!(", DEFAULT_SCHEMA = {}", quote(s))).unwrap_or_default();
            out.batch(&format!("CREATE APPLICATION ROLE {} WITH PASSWORD = 'p@ssw0rd'{schema}", quote(&p.name)))
        }
        _ => out.batch(&format!("CREATE ROLE {}\nAUTHORIZATION {}", quote(&p.name), quote(p.owner.as_deref().unwrap_or("dbo")))),
    }
    members(&mut out, db, &p.name);
    database_permissions(&mut out, db, &p.name, o);
    principal_xprops(&mut out, db, &p.name, o);
    out.s
}

/// Extended properties of a user or role (level 0 USER). They follow the database
/// permissions, which end without `GO`, in the same batch.
fn principal_xprops(out: &mut Out, db: &Database, name: &str, o: &Options) {
    if o.has(Opt::IgnoreExtendedProperties) {
        return;
    }
    for p in db.extended_properties.iter().filter(|p| p.class == "USER" && p.schema.is_none() && p.object.as_deref() == Some(name)) {
        out.batch(&xprop_sql(p));
    }
}

/// A fixed role's file (public, db_datareader...) holds its members and database permissions.
pub fn fixed_role(db: &Database, role: &str, o: &Options) -> String {
    let mut out = Out::default();
    members(&mut out, db, role);
    database_permissions(&mut out, db, role, o);
    out.s
}

/// guest's file holds only its database permissions, after an empty batch where the other
/// users have CREATE USER.
pub fn guest(db: &Database, o: &Options) -> String {
    let mut text = Out::default();
    database_permissions(&mut text, db, "guest", o);
    if text.s.is_empty() {
        return String::new();
    }
    let mut out = Out::default();
    out.batch("");
    out.s.push_str(&text.s);
    out.s
}

pub fn user(db: &Database, p: &Principal, o: &Options) -> String {
    let mut out = Out::default();
    let mut s = format!("CREATE USER {}", quote(&p.name));
    match (p.kind.as_str(), &p.login) {
        ("EXTERNAL_USER" | "EXTERNAL_GROUP", _) => s.push_str(" FROM EXTERNAL PROVIDER"),
        ("CERTIFICATE_MAPPED_USER", _) => s.push_str(&format!(" FOR CERTIFICATE {}", quote(p.certificate.as_deref().unwrap_or("UNKNOWN")))),
        ("ASYMMETRIC_KEY_MAPPED_USER", _) => s.push_str(&format!(" FOR ASYMMETRIC KEY {}", quote(p.asymmetric_key.as_deref().unwrap_or("UNKNOWN")))),
        (_, Some(l)) => s.push_str(&format!(" FOR LOGIN {}", quote(l))),
        (_, None) => s.push_str(" WITHOUT LOGIN"),
    }
    if let Some(ds) = p.default_schema.as_ref().filter(|s| *s != "dbo") {
        s.push_str(&format!(" WITH DEFAULT_SCHEMA={}", quote(ds)));
    }
    out.batch(&s);
    // CREATE USER gives CONNECT: SQL Compare writes only its absence, Data Tools style
    // (WriteAsDataToolsPermissions) writes the grant.
    if !o.has(Opt::IgnorePermissions) {
        let denied = db.permissions.iter().any(|x| x.class == "DATABASE" && x.grantee == p.name && x.permission.eq_ignore_ascii_case("CONNECT"));
        if p.no_connect && !o.has(Opt::WriteAsDataToolsPermissions) {
            out.line(&format!("REVOKE CONNECT TO {}", quote(&p.name)));
        } else if !p.no_connect && !denied && o.has(Opt::WriteAsDataToolsPermissions) {
            out.line(&format!("GRANT CONNECT TO {}", quote(&p.name)));
        }
    }
    database_permissions(&mut out, db, &p.name, o);
    principal_xprops(&mut out, db, &p.name, o);
    out.s
}

/// A database DDL trigger: its text, then `DISABLE TRIGGER … ON DATABASE` when disabled.
pub fn ddl_trigger(t: &DdlTrigger) -> String {
    let mut out = Out::default();
    out.set_options(&ModuleSettings::default());
    out.module(&module_sql(&t.definition, "CREATE", &quote(&t.name)));
    if t.disabled {
        // SQL Compare writes the name as it is here, without brackets.
        out.batch(&format!("DISABLE TRIGGER {} ON DATABASE", t.name));
    }
    out.s
}

/// A default's or rule's file head: its stored text as it is (a default after
/// `SET QUOTED_IDENTIFIER OFF`), then `GO`.
pub fn bindable(b: &BindableObject, default: bool) -> String {
    let mut out = Out::default();
    if default {
        out.batch("SET QUOTED_IDENTIFIER OFF");
    }
    out.module(&crate::schema::ddl_objects::create_bindable(b));
    out.s
}

/// A security policy as SQL Compare writes it in a folder, line breaks included: LF between
/// a predicate and its ON, CRLF elsewhere, and a batch end of LF, `GO`, LF.
pub fn security_policy(p: &SecurityPolicy) -> String {
    let predicates: Vec<String> = p
        .predicates
        .iter()
        .map(|x| {
            let op = x.operation.as_ref().map(|o| format!(" {o}")).unwrap_or_default();
            format!("ADD {} PREDICATE {}{}\nON {}{op}", x.kind, x.function, x.arguments, x.table)
        })
        .collect();
    let binding = if p.schema_bound { "" } else { ", SCHEMABINDING = OFF" };
    let nfr = if p.not_for_replication { "NOT FOR REPLICATION\r\n" } else { "" };
    format!(
        "CREATE SECURITY POLICY {}\r\n{}\r\nWITH (STATE = {}{binding})\r\n{nfr}\nGO\n",
        qname(&p.schema, &p.name),
        predicates.join(",\r\n"),
        on_off(p.enabled)
    )
}

/// CREATE XML SCHEMA COLLECTION with the schemas indented as SQL Compare writes them.
pub fn xml_schema_collection(x: &XmlSchemaCollection) -> String {
    format!("CREATE XML SCHEMA COLLECTION {}\r\nAS {}", qname(&x.schema, &x.name), crate::schema::ddl::sql_string(&indent_xml(&x.definition)))
}

/// XML laid out the way .NET's indenting XML writer does it: each element on its own line,
/// two spaces per level, `<e />` for empty elements, an element holding text kept on one
/// line; comments, processing instructions and whitespace between tags dropped.
pub fn indent_xml(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() * 2);
    // Per open element: whether it holds text (then its contents are not indented), and
    // whether it has child elements.
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let mut rest = xml;
    let newline = |out: &mut String, depth: usize| {
        if !out.is_empty() {
            out.push_str("\r\n");
        }
        out.push_str(&"  ".repeat(depth));
    };
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map(|i| &r[i + 3..]).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix("<?") {
            rest = r.find("?>").map(|i| &r[i + 2..]).unwrap_or("");
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").map(|i| i + 3).unwrap_or(rest.len());
            if let Some(top) = stack.last_mut() {
                top.0 = true;
            }
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').unwrap_or(r.len());
            let (text, children) = stack.pop().unwrap_or((false, false));
            if children && !text {
                newline(&mut out, stack.len());
            }
            out.push_str("</");
            out.push_str(r[..end].trim());
            out.push('>');
            rest = r.get(end + 1..).unwrap_or("");
        } else if rest.starts_with('<') {
            // Find the end of the tag outside attribute values.
            let mut quote = None;
            let mut end = rest.len();
            for (i, c) in rest.char_indices().skip(1) {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, '>') => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let mut tag = rest[1..end].trim_end();
            let empty = tag.ends_with('/');
            if empty {
                tag = tag[..tag.len() - 1].trim_end();
            }
            let mixed = stack.last().is_some_and(|t| t.0);
            if let Some(top) = stack.last_mut() {
                top.1 = true;
            }
            if !mixed {
                newline(&mut out, stack.len());
            }
            out.push('<');
            out.push_str(&normalize_tag(tag));
            if empty {
                out.push_str(" />");
            } else {
                out.push('>');
                stack.push((false, false));
            }
            rest = rest.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = &rest[..end];
            if !text.trim().is_empty() {
                if let Some(top) = stack.last_mut() {
                    top.0 = true;
                }
                out.push_str(text);
            }
            rest = &rest[end..];
        }
    }
    out
}

/// A start tag's name and attributes with single spaces between them and double quotes.
fn normalize_tag(tag: &str) -> String {
    let mut out = String::new();
    let mut chars = tag.chars().peekable();
    let mut quote: Option<char> = None;
    let mut space = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                out.push('"');
            }
            (Some(q), c) if c == q => {
                quote = None;
                out.push('"');
            }
            (Some('\''), '"') => out.push_str("&quot;"),
            (None, c) if c.is_whitespace() => space = true,
            (None, '=') => {
                space = false;
                out.push('=');
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            (None, c) => {
                if space && !out.ends_with('=') {
                    out.push(' ');
                }
                space = false;
                out.push(c);
            }
            (Some(_), c) => out.push(c),
        }
    }
    out
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
