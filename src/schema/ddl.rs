//! T-SQL for single objects and sub-objects, rendered from the model.
//!
//! The `*_with` functions render for a deployment: the options and the target's features decide
//! clauses (fill factor, filegroups, ONLINE…). The plain functions render everything the model
//! holds, for showing an object's definition.

use super::features::{Feature, Features};
use super::options::{Opt, Options};
use crate::catalog::*;
use crate::db::{literal, quote};

/// What a deployment script is rendered for.
#[derive(Clone, Debug)]
pub struct Render {
    pub opts: Options,
    pub features: Features,
}

impl Render {
    /// Every clause the model has, for display.
    pub fn display() -> Render {
        Render { opts: Options::none(), features: Features::latest() }
    }

    fn has(&self, o: Opt) -> bool {
        self.opts.has(o)
    }
}

/// The table (or view, or table type) an index belongs to, as far as its DDL cares.
#[derive(Clone, Copy, Debug, Default)]
pub struct Host {
    pub view: bool,
    pub table_type: bool,
    pub memory_optimized: bool,
    pub temporal: bool,
    /// Has a text, ntext or image column.
    pub legacy_lob: bool,
    /// Has a geometry or geography column.
    pub spatial_column: bool,
    pub partitioned: bool,
}

impl Host {
    pub fn of_table(t: &Table) -> Host {
        Host {
            memory_optimized: t.memory_optimized,
            temporal: t.temporal.is_some(),
            legacy_lob: t.columns.iter().any(|c| matches!(c.data_type.name.as_str(), "text" | "ntext" | "image") && c.data_type.schema.is_none()),
            spatial_column: t.columns.iter().any(|c| matches!(c.data_type.name.as_str(), "geometry" | "geography")),
            partitioned: t.storage.partition_column.is_some(),
            ..Default::default()
        }
    }

    pub fn view() -> Host {
        Host { view: true, ..Default::default() }
    }
}

/// `[schema].[name]`.
pub fn qname(schema: &str, name: &str) -> String {
    format!("{}.{}", quote(schema), quote(name))
}

/// A T-SQL string literal as SQL Compare writes one for dynamic SQL: quotes doubled, and a
/// backslash before a line break doubled so the break survives.
pub fn sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 3);
    out.push_str("N'");
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            out.push_str("''");
        } else if c == '\\' && (chars.get(i + 1) == Some(&'\n') || (chars.get(i + 1) == Some(&'\r') && chars.get(i + 2) == Some(&'\n'))) {
            let nl = if chars[i + 1] == '\r' { "\r\n" } else { "\n" };
            out.push_str("\\\\");
            out.push_str(nl);
            out.push_str(nl);
            i += 1 + nl.len();
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out.push('\'');
    out
}

/// A column type as SQL Compare writes it: `[nvarchar] (100)`, `[decimal] (18, 2)`, `[dbo].[Phone]`.
pub fn type_sql(t: &DataType) -> String {
    if let Some(schema) = &t.schema {
        return qname(schema, &t.name);
    }
    let mut out = quote(&t.name);
    match t.name.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => {
            if let Some(len) = t.length {
                out.push_str(&if len < 0 { " (max)".to_string() } else { format!(" ({len})") });
            }
        }
        "decimal" | "numeric" => out.push_str(&format!(" ({}, {})", t.precision.unwrap_or(18), t.scale.unwrap_or(0))),
        "datetime2" | "time" | "datetimeoffset" => {
            if let Some(scale) = t.scale.filter(|s| *s != 7) {
                out.push_str(&format!(" ({scale})"));
            }
        }
        "xml" => {
            if let Some(c) = &t.xml_collection {
                out.push_str(&format!(" ({} {c})", if t.xml_document { "DOCUMENT" } else { "CONTENT" }));
            }
        }
        _ => {}
    }
    out
}

pub fn column_definition(c: &Column, with_default: bool) -> String {
    column_definition_with(c, with_default, &Render::display(), false)
}

/// A column inside CREATE TABLE / ALTER TABLE ADD (`full`), or for ALTER COLUMN (not `full`:
/// no IDENTITY, ROWGUIDCOL or default).
pub fn column_definition_with(c: &Column, full: bool, r: &Render, memory_optimized: bool) -> String {
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
    s.push_str(&type_sql(&c.data_type));
    if c.column_set {
        if full {
            s.push_str(" COLUMN_SET FOR ALL_SPARSE_COLUMNS");
        }
        return s;
    }
    if c.filestream {
        s.push_str(" FILESTREAM");
    }
    if let Some(coll) = &c.collation
        && (memory_optimized || !r.has(Opt::IgnoreCollations))
    {
        s.push_str(" COLLATE ");
        s.push_str(coll);
    }
    if c.sparse {
        s.push_str(" SPARSE");
    }
    if let Some(m) = &c.masking_function
        && r.features.has(Feature::DataMasking)
        && !r.has(Opt::IgnoreDynamicDataMasking)
    {
        s.push_str(&format!(" MASKED WITH (FUNCTION = '{}')", m.replace('\'', "''")));
    }
    if r.features.has(Feature::Temporal) {
        if let Some(g) = &c.generated_always {
            s.push_str(&format!(" GENERATED ALWAYS AS {g}"));
        }
        if c.hidden {
            s.push_str(" HIDDEN");
        }
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    if full {
        if let Some(id) = &c.identity {
            s.push_str(&format!(" IDENTITY({}, {})", id.seed, id.increment));
            if id.not_for_replication && !r.has(Opt::IgnoreNotForReplication) {
                s.push_str(" NOT FOR REPLICATION");
            }
        }
        if c.rowguidcol {
            s.push_str(" ROWGUIDCOL");
        }
        // System-generated names are kept too, so a later comparison finds the same name.
        if let Some(d) = &c.default {
            if d.system_named && r.has(Opt::IgnoreSystemNamedConstraintNames) {
                s.push_str(&format!(" DEFAULT {}", d.definition));
            } else {
                s.push_str(&format!(" CONSTRAINT {} DEFAULT {}", quote(&d.name), d.definition));
            }
        }
    }
    s
}

fn storage_clause(st: &Storage, r: &Render) -> String {
    if r.has(Opt::IgnoreFileGroups) || !(r.features.has(Feature::Filegroups) || st.partition_column.is_some()) {
        return String::new();
    }
    match (&st.data_space, &st.partition_column) {
        (Some(ds), Some(col)) => format!(" ON {} ({})", quote(ds), quote(col)),
        (Some(ds), None) => format!(" ON {}", quote(ds)),
        _ => String::new(),
    }
}

fn compression_option(st: &Storage, r: &Render) -> Option<String> {
    if r.has(Opt::IgnoreDataCompression) {
        return None;
    }
    match st.compression.as_slice() {
        [] => None,
        [one] if one.partition == 0 => (one.compression != "NONE").then(|| format!("DATA_COMPRESSION = {}", one.compression)),
        many if r.has(Opt::IgnoreFileGroups) => {
            many.first().filter(|p| p.compression != "NONE").map(|p| format!("DATA_COMPRESSION = {}", p.compression))
        }
        many => Some(
            many.iter()
                .map(|p| format!("DATA_COMPRESSION = {} ON PARTITIONS ({})", p.compression, p.partition))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn is_columnstore(ix: &Index) -> bool {
    matches!(ix.kind, IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore)
}

fn is_clustered(ix: &Index) -> bool {
    matches!(ix.kind, IndexKind::Clustered | IndexKind::ClusteredColumnstore)
}

/// ONLINE = ON can be written for this index (asked for by AddOnlineOn).
pub fn online_allowed(ix: &Index, host: &Host, f: &Features) -> bool {
    let prevented = matches!(ix.kind, IndexKind::Xml | IndexKind::Spatial)
        || (ix.unique && is_clustered(ix) && host.view)
        || (is_clustered(ix) && host.legacy_lob)
        || host.spatial_column
        || host.table_type
        || (ix.primary_key && host.temporal)
        || match ix.kind {
            IndexKind::ClusteredColumnstore => !f.has(Feature::OnlineClusteredColumnstore),
            IndexKind::NonclusteredColumnstore => !f.has(Feature::OnlineNonclusteredColumnstore),
            _ => !f.has(Feature::Online),
        };
    !prevented
}

/// STATISTICS_INCREMENTAL can be written for this index.
fn incremental_allowed(ix: &Index, host: &Host, r: &Render) -> bool {
    r.features.has(Feature::IncrementalStatistics)
        && !host.view
        && !host.table_type
        && !host.memory_optimized
        && ix.filter.is_none()
        && !is_columnstore(ix)
        && !matches!(ix.kind, IndexKind::Xml | IndexKind::Spatial)
        && (ix.storage.partition_column.is_none() || host.partitioned)
}

/// The WITH (…) items every kind of index shares.
fn index_options(ix: &Index, r: &Render, host: &Host) -> Vec<String> {
    let mut opts = Vec::new();
    if host.memory_optimized {
        if let Some(b) = ix.bucket_count {
            opts.push(format!("BUCKET_COUNT={b}"));
        }
        return opts;
    }
    let columnstore = is_columnstore(ix);
    if !r.has(Opt::IgnoreFillFactor) {
        if ix.fill_factor != 0 {
            opts.push(format!("FILLFACTOR={}", ix.fill_factor));
        }
        if ix.pad_index {
            opts.push("PAD_INDEX=ON".into());
        }
    }
    if ix.ignore_dup_key {
        opts.push("IGNORE_DUP_KEY=ON".into());
    }
    if !r.has(Opt::IgnoreStatisticsNorecompute) && ix.no_recompute && !columnstore {
        opts.push("STATISTICS_NORECOMPUTE=ON".into());
    }
    if ix.incremental && !r.has(Opt::IgnoreStatisticsIncremental) && incremental_allowed(ix, host, r) {
        opts.push("STATISTICS_INCREMENTAL=ON".into());
    }
    if !r.has(Opt::IgnoreIndexLockProperties) && !columnstore {
        if !ix.allow_row_locks {
            opts.push("ALLOW_ROW_LOCKS=OFF".into());
        }
        if !ix.allow_page_locks {
            opts.push("ALLOW_PAGE_LOCKS=OFF".into());
        }
    }
    if ix.optimize_for_sequential_key {
        opts.push("OPTIMIZE_FOR_SEQUENTIAL_KEY=ON".into());
    }
    if r.has(Opt::AddOnlineOn) && online_allowed(ix, host, &r.features) {
        opts.push("ONLINE = ON".into());
    }
    opts
}

fn with_clause(opts: &[String]) -> String {
    if opts.is_empty() { String::new() } else { format!(" WITH ({})", opts.join(", ")) }
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

/// ` ([a], [b] DESC) INCLUDE ([c])` — columnstore indexes list their columns without order.
fn fields(ix: &Index) -> String {
    let mut s = String::new();
    if is_columnstore(ix) {
        if ix.kind != IndexKind::ClusteredColumnstore && !ix.columns.is_empty() {
            s.push_str(&format!(" ({})", ix.columns.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ")));
        }
        return s;
    }
    if !ix.columns.is_empty() {
        s.push_str(&format!(" ({})", key_list(ix)));
    }
    if !ix.included.is_empty() {
        s.push_str(&format!(" INCLUDE ({})", ix.included.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")));
    }
    s
}

/// PK/UQ as a table constraint (inside CREATE TABLE or ALTER TABLE ADD).
pub fn key_constraint(ix: &Index) -> String {
    key_constraint_with(ix, &Render::display(), &Host::default())
}

pub fn key_constraint_with(ix: &Index, r: &Render, host: &Host) -> String {
    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
    let named = !(ix.system_named && r.has(Opt::IgnoreSystemNamedConstraintNames));
    let mut s = if named { format!("CONSTRAINT {} ", quote(&ix.name)) } else { String::new() };
    s.push_str(&format!("{what} {}", kind_words(ix)));
    s.push_str(&fields(ix));
    if host.table_type {
        return s + &with_clause(&index_options(ix, r, host));
    }
    let mut opts = index_options(ix, r, host);
    opts.extend(compression_option(&ix.storage, r));
    s.push_str(&with_clause(&opts));
    s.push_str(&storage_clause(&ix.storage, r));
    s
}

/// Statements that create an index or key on `table`.
pub fn create_index(table: &str, ix: &Index) -> Vec<String> {
    create_index_with(table, ix, &Render::display(), &Host::default())
}

pub fn create_index_with(table: &str, ix: &Index, r: &Render, host: &Host) -> Vec<String> {
    let mut out = vec![if ix.primary_key || ix.unique_constraint {
        format!("ALTER TABLE {table} ADD {}", key_constraint_with(ix, r, host))
    } else {
        format!("CREATE {}", index_body(table, ix, r, host))
    }];
    if ix.disabled {
        out.push(format!("ALTER INDEX {} ON {table} DISABLE", quote(&ix.name)));
    }
    out
}

/// Everything after CREATE for an index that is not a key.
fn index_body(table: &str, ix: &Index, r: &Render, host: &Host) -> String {
    match ix.kind {
        IndexKind::Xml => xml_index_body(table, ix, r, host),
        IndexKind::Spatial => spatial_index_body(table, ix, r, host),
        _ => {
            let unique = if ix.unique { "UNIQUE " } else { "" };
            let mut s = format!("{unique}{} INDEX {} ON {table}", kind_words(ix), quote(&ix.name));
            s.push_str(&fields(ix));
            if let Some(f) = &ix.filter {
                s.push_str(&format!(" WHERE {f}"));
            }
            let mut opts = index_options(ix, r, host);
            opts.extend(compression_option(&ix.storage, r));
            s.push_str(&with_clause(&opts));
            s.push_str(&storage_clause(&ix.storage, r));
            s
        }
    }
}

fn xml_index_body(table: &str, ix: &Index, r: &Render, host: &Host) -> String {
    let x = ix.xml.clone().unwrap_or_default();
    let mut s = String::new();
    match x.kind {
        XmlIndexKind::Primary => s.push_str("PRIMARY "),
        XmlIndexKind::Selective => s.push_str("SELECTIVE "),
        _ => {}
    }
    s.push_str(&format!("XML INDEX {}\r\nON {table} ({})\r\n", quote(&ix.name), ix.columns.iter().map(|c| quote(&c.name)).collect::<Vec<_>>().join(", ")));
    match x.kind {
        XmlIndexKind::Secondary | XmlIndexKind::SecondarySelective => {
            s.push_str(&format!("USING XML INDEX {}\r\nFOR ", quote(x.using_index.as_deref().unwrap_or_default())));
            if x.kind == XmlIndexKind::SecondarySelective {
                s.push_str(&format!("(\r\n{}\r\n)\r\n", x.using_path.as_deref().unwrap_or_default()));
            } else {
                s.push_str(x.secondary_type.as_deref().unwrap_or("VALUE"));
            }
            s.push_str("\r\n");
        }
        XmlIndexKind::Selective => {
            if !x.namespaces.is_empty() {
                let ns: Vec<String> = x
                    .namespaces
                    .iter()
                    .map(|n| match &n.prefix {
                        Some(p) => format!("{} AS {}", literal(&n.uri), quote(p)),
                        None => format!("DEFAULT {}", literal(&n.uri)),
                    })
                    .collect();
                s.push_str(&format!("WITH XMLNAMESPACES (\r\n{}\r\n)\r\n", ns.join(",\r\n")));
            }
            let paths: Vec<String> = x.paths.iter().map(selective_path).collect();
            s.push_str(&format!("FOR (\r\n{}\r\n)\r\n", paths.join(",\r\n")));
        }
        XmlIndexKind::Primary => {}
    }
    s.push_str(&with_clause(&index_options(ix, r, host)));
    s
}

fn selective_path(p: &SelectivePath) -> String {
    let mut s = format!("{} = '{}' AS ", p.name, p.path);
    if p.sql {
        s.push_str("SQL ");
        s.push_str(&p.data_type.as_ref().map(type_sql).unwrap_or_default());
    } else {
        s.push_str("XQUERY ");
        if p.node_hint {
            s.push_str("'node()'");
        } else if let Some(t) = &p.xquery_type {
            s.push_str(&format!("'{t}'"));
            if let Some(n) = p.max_length.filter(|n| *n > 0) {
                s.push_str(&format!(" MAXLENGTH({n})"));
            }
        }
    }
    if p.singleton {
        s.push_str(" SINGLETON");
    }
    s
}

fn spatial_index_body(table: &str, ix: &Index, r: &Render, host: &Host) -> String {
    let sp = ix.spatial.clone().unwrap_or_default();
    let mut s = format!("SPATIAL INDEX {} ON {table}{}", quote(&ix.name), fields(ix));
    let scheme = sp.tessellation.to_ascii_uppercase();
    let auto = scheme.ends_with("AUTO_GRID");
    if !scheme.is_empty() && scheme != "GEOGRAPHY_AUTO_GRID" && r.features.has(Feature::GeographyTessellation) {
        s.push_str(&format!(" USING {} ", scheme.to_ascii_lowercase()));
    }
    let mut opts = Vec::new();
    if !sp.geography && sp.bounding_box.len() == 4 {
        opts.push(format!("BOUNDING_BOX = ({})", sp.bounding_box.join(", ")));
    }
    if !auto || !r.features.has(Feature::AutoGrid) {
        let grid = |i: usize| sp.grids.get(i).cloned().unwrap_or_else(|| "MEDIUM".into());
        opts.push(format!("GRIDS = ({}, {}, {}, {})", grid(0), grid(1), grid(2), grid(3)));
    }
    opts.push(format!("CELLS_PER_OBJECT = {}", if sp.cells_per_object == 0 { 16 } else { sp.cells_per_object }));
    opts.extend(compression_option(&ix.storage, r));
    opts.extend(index_options(ix, r, host));
    s.push_str(&with_clause(&opts));
    s.push_str(&storage_clause(&ix.storage, r));
    s
}

pub fn drop_index(table: &str, ix: &Index) -> String {
    if ix.primary_key || ix.unique_constraint {
        format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote(&ix.name))
    } else {
        format!("DROP INDEX {} ON {table}", quote(&ix.name))
    }
}

/// `CONSTRAINT [fk] FOREIGN KEY (…) REFERENCES … (…)` with its actions.
pub fn foreign_key_body(fk: &ForeignKey, opts: &Options) -> String {
    let mut s = String::new();
    if !(fk.system_named && opts.has(Opt::IgnoreSystemNamedConstraintNames)) {
        s.push_str(&format!("CONSTRAINT {} ", quote(&fk.name)));
    }
    s.push_str(&format!(
        "FOREIGN KEY ({}) REFERENCES {} ({})",
        fk.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
        fk.referenced_table.quoted(),
        fk.referenced_columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
    ));
    if fk.on_delete != "NO_ACTION" && !fk.on_delete.is_empty() {
        s.push_str(&format!(" ON DELETE {}", fk.on_delete.replace('_', " ")));
    }
    if fk.on_update != "NO_ACTION" && !fk.on_update.is_empty() {
        s.push_str(&format!(" ON UPDATE {}", fk.on_update.replace('_', " ")));
    }
    if fk.not_for_replication && !opts.has(Opt::IgnoreNotForReplication) {
        s.push_str(" NOT FOR REPLICATION");
    }
    s
}

pub fn add_foreign_key(table: &str, fk: &ForeignKey, ignore_nocheck: bool) -> Vec<String> {
    let opts = if ignore_nocheck { Options::none().with(Opt::IgnoreWithNocheck) } else { Options::none() };
    add_foreign_key_with(table, fk, &opts)
}

/// ALTER TABLE … ADD for a foreign key, then NOCHECK when it is disabled.
pub fn add_foreign_key_with(table: &str, fk: &ForeignKey, opts: &Options) -> Vec<String> {
    let ignore = opts.has(Opt::IgnoreWithNocheck) || opts.has(Opt::IgnoreNocheckAndWithNocheck);
    let nocheck = if fk.not_trusted && !ignore { " WITH NOCHECK " } else { "" };
    let mut out = vec![format!("ALTER TABLE {table}{nocheck} ADD {}", foreign_key_body(fk, opts))];
    if fk.disabled && !opts.has(Opt::IgnoreNocheckAndWithNocheck) {
        out.push(format!("ALTER TABLE {table} NOCHECK CONSTRAINT {}", quote(&fk.name)));
    }
    out
}

/// `CONSTRAINT [ck] CHECK (…)`.
pub fn check_body(ck: &CheckConstraint, opts: &Options) -> String {
    let mut s = String::new();
    if !(ck.system_named && opts.has(Opt::IgnoreSystemNamedConstraintNames)) {
        s.push_str(&format!("CONSTRAINT {} ", quote(&ck.name)));
    }
    s.push_str("CHECK ");
    if ck.not_for_replication && !opts.has(Opt::IgnoreNotForReplication) {
        s.push_str("NOT FOR REPLICATION ");
    }
    s.push_str(&ck.definition);
    s
}

pub fn add_check(table: &str, ck: &CheckConstraint, ignore_nocheck: bool) -> Vec<String> {
    let opts = if ignore_nocheck { Options::none().with(Opt::IgnoreNocheckAndWithNocheck) } else { Options::none() };
    add_check_with(table, ck, &opts)
}

/// ALTER TABLE … ADD for a check constraint, then NOCHECK when it is disabled.
pub fn add_check_with(table: &str, ck: &CheckConstraint, opts: &Options) -> Vec<String> {
    let ignore = opts.has(Opt::IgnoreWithNocheck) || opts.has(Opt::IgnoreNocheckAndWithNocheck);
    let nocheck = if ck.not_trusted && !ignore { " WITH NOCHECK" } else { "" };
    let mut out = vec![format!("ALTER TABLE {table}{nocheck} ADD {}", check_body(ck, opts))];
    if ck.disabled && !opts.has(Opt::IgnoreNocheckAndWithNocheck) {
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

/// `ALTER TABLE [s].[t] SET ( LOCK_ESCALATION = AUTO )`.
pub fn lock_escalation(table: &str, value: &str) -> String {
    format!("ALTER TABLE {table} SET ( LOCK_ESCALATION = {value} )")
}

/// CREATE TABLE with columns, defaults and (for memory-optimized tables) inline indexes.
/// Other indexes, keys, checks and foreign keys are created separately.
pub fn create_table(t: &Table, name_override: Option<&str>, inline_keys: bool) -> String {
    create_table_with(t, name_override, inline_keys, &Render::display())
}

pub fn create_table_with(t: &Table, name_override: Option<&str>, inline_keys: bool, r: &Render) -> String {
    let name = qname(&t.schema, name_override.unwrap_or(&t.name));
    let host = Host::of_table(t);
    let mut lines: Vec<String> = t
        .columns
        .iter()
        .filter(|c| !is_graph_internal(c))
        .map(|c| column_definition_with(c, true, r, t.memory_optimized))
        .collect();
    if let Some(temporal) = &t.temporal
        && !temporal.period_start.is_empty()
        && r.features.has(Feature::Temporal)
    {
        lines.push(format!("PERIOD FOR SYSTEM_TIME ({}, {})", quote(&temporal.period_start), quote(&temporal.period_end)));
    }
    let inline_all = t.memory_optimized;
    for ix in &t.indexes {
        if inline_all {
            let s = if ix.primary_key || ix.unique_constraint {
                key_constraint_with(ix, r, &host)
            } else {
                let unique = if ix.unique { "UNIQUE " } else { "" };
                format!("INDEX {} {unique}{}{}{}", quote(&ix.name), kind_words(ix), fields(ix), with_clause(&index_options(ix, r, &host)))
            };
            lines.push(s);
        } else if inline_keys && ix.primary_key {
            lines.push(key_constraint_with(ix, r, &host));
        }
    }
    let mut s = format!("CREATE TABLE {name}\r\n(\r\n{}\r\n)", lines.join(",\r\n"));
    if let Some(g) = &t.graph
        && r.features.has(Feature::Graph)
    {
        s.push_str(&format!(" AS {g}"));
    }
    if !t.memory_optimized && !r.has(Opt::IgnoreFileGroups) {
        s.push_str(&storage_clause(&t.storage, r));
        if r.features.has(Feature::Filegroups) {
            if let Some(fg) = &t.text_filegroup
                && Some(fg) != t.storage.data_space.as_ref()
            {
                s.push_str(&format!(" TEXTIMAGE_ON {}", quote(fg)));
            }
            if let Some(fg) = &t.filestream_filegroup {
                s.push_str(&format!(" FILESTREAM_ON {}", quote(fg)));
            }
        }
    }
    let mut with = Vec::new();
    let clustered_inline = (inline_keys || inline_all) && t.indexes.iter().any(|i| i.primary_key && is_clustered(i));
    if !clustered_inline && !t.indexes.iter().any(is_clustered)
        && let Some(c) = compression_option(&t.storage, r)
    {
        with.push(c);
    }
    if t.memory_optimized {
        with.push("MEMORY_OPTIMIZED = ON".into());
        if t.durability.as_deref().is_some_and(|d| d.eq_ignore_ascii_case("SCHEMA_ONLY")) {
            with.push("DURABILITY = SCHEMA_ONLY".into());
        }
    }
    if !with.is_empty() {
        s.push_str(&format!("\r\nWITH\r\n(\r\n{}\r\n)", with.join(",\r\n")));
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

/// Adds or removes ENCRYPTION in a module's header WITH clause.
pub fn with_encryption(definition: &str, add: bool) -> String {
    use super::text::Tok;
    let tokens = super::text::tokenize(definition);
    let is_word = |i: usize, w: &str| tokens[i].kind == Tok::Word && tokens[i].text.eq_ignore_ascii_case(w);
    let body = |i: usize| ["AS", "FOR", "INSTEAD", "AFTER", "NOT", "RETURN", "BEGIN"].iter().any(|w| is_word(i, w));
    // AS and RETURN right after a variable belong to a parameter list.
    let after_variable = |i: usize| {
        (0..i).rev().find(|&k| !matches!(tokens[k].kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).is_some_and(|k| tokens[k].text.starts_with('@'))
    };
    let mut depth = 0;
    let mut head_end = tokens.len();
    let mut with_at = None;
    for (i, t) in tokens.iter().enumerate() {
        match t.text.as_str() {
            "(" if t.kind == Tok::Punct => depth += 1,
            ")" if t.kind == Tok::Punct => depth -= 1,
            _ => {}
        }
        if depth != 0 {
            continue;
        }
        if is_word(i, "WITH") {
            head_end = i;
            with_at = Some(i);
            break;
        }
        if body(i) && !((is_word(i, "AS") || is_word(i, "RETURN")) && after_variable(i)) {
            head_end = i;
            break;
        }
    }
    let mut rest = head_end;
    let mut items = String::new();
    let mut has = false;
    if let Some(w) = with_at {
        rest = w + 1;
        while rest < tokens.len() && !body(rest) {
            has |= is_word(rest, "ENCRYPTION");
            items.push_str(&tokens[rest].text);
            rest += 1;
        }
    }
    if add {
        if items.trim().is_empty() {
            items = " ENCRYPTION ".into();
        } else if !has {
            items = format!(" ENCRYPTION, {items}");
        }
    } else if has {
        let kept: Vec<&str> = items.split(',').map(str::trim).filter(|w| !w.to_ascii_uppercase().contains("ENCRYPTION")).collect();
        items = format!(" {} ", kept.join(", "));
    }
    let head: String = tokens[..head_end].iter().map(|t| t.text.as_str()).collect();
    let tail: String = tokens[rest..].iter().map(|t| t.text.as_str()).collect();
    let clause = if items.trim().is_empty() { String::new() } else { format!("WITH{items}") };
    format!("{head}{clause}{tail}")
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
        "CREATE SEQUENCE {}\r\nAS {}\r\nSTART WITH {}\r\nINCREMENT BY {}\r\nMINVALUE {}\r\nMAXVALUE {}\r\n{}\r\n{}",
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
            (true, None) => "CACHE ".to_string(),
        }
    )
}

/// ALTER SEQUENCE with the clauses that change; RESTART WITH the new minimum when the current
/// start falls outside the new range.
pub fn alter_sequence(a: &Sequence, target: &Sequence) -> Option<String> {
    let mut s = String::new();
    let num = |v: &str| v.trim().parse::<i128>().ok();
    if a.start != target.start {
        s.push_str(&format!(" RESTART WITH {}", a.start));
    } else if let (Some(start), Some(min), Some(max)) = (num(&target.start), num(&a.minimum), num(&a.maximum))
        && (start < min || start > max)
    {
        s.push_str(&format!(" RESTART WITH {}", a.minimum));
    }
    if a.increment != target.increment {
        s.push_str(&format!(" INCREMENT BY {}", a.increment));
    }
    if a.minimum != target.minimum {
        s.push_str(&format!(" MINVALUE {}", a.minimum));
    }
    if a.maximum != target.maximum {
        s.push_str(&format!(" MAXVALUE {}", a.maximum));
    }
    if a.cycle != target.cycle {
        s.push_str(if a.cycle { " CYCLE " } else { " NO CYCLE " });
    }
    if a.cached != target.cached || a.cache_size != target.cache_size {
        s.push_str(&match (a.cached, a.cache_size) {
            (false, _) => " NO CACHE ".to_string(),
            (true, Some(n)) => format!(" CACHE {n}"),
            (true, None) => " CACHE ".to_string(),
        });
    }
    (!s.is_empty()).then(|| format!("ALTER SEQUENCE {}{s}", qname(&a.schema, &a.name)))
}

pub fn create_synonym(s: &Synonym) -> String {
    format!("CREATE SYNONYM {} FOR {}", qname(&s.schema, &s.name), s.base_object)
}

pub fn create_type(t: &UserType) -> String {
    let name = qname(&t.schema, &t.name);
    match t.kind {
        UserTypeKind::Alias => format!(
            "CREATE TYPE {name} FROM {}{}",
            t.base_type.as_ref().map(type_sql).unwrap_or_default(),
            if t.nullable { " NULL" } else { " NOT NULL" }
        ),
        UserTypeKind::Clr => format!("CREATE TYPE {name} EXTERNAL NAME {}", t.clr_class.clone().unwrap_or_default()),
        UserTypeKind::Table => {
            let r = Render::display();
            let mut lines: Vec<String> = t.columns.iter().map(|c| column_definition_with(c, true, &r, t.memory_optimized)).collect();
            let host = Host { table_type: true, memory_optimized: t.memory_optimized, ..Default::default() };
            for ix in &t.indexes {
                if ix.primary_key || ix.unique_constraint {
                    let what = if ix.primary_key { "PRIMARY KEY" } else { "UNIQUE" };
                    let named = if ix.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ix.name)) };
                    lines.push(format!("{named}{what} {}{}{}", kind_words(ix), fields(ix), with_clause(&index_options(ix, &r, &host))));
                } else {
                    lines.push(format!("INDEX {} {}{}{}", quote(&ix.name), kind_words(ix), fields(ix), with_clause(&index_options(ix, &r, &host))));
                }
            }
            for ck in &t.check_constraints {
                let named = if ck.system_named { String::new() } else { format!("CONSTRAINT {} ", quote(&ck.name)) };
                lines.push(format!("{named}CHECK {}", ck.definition));
            }
            let mut s = format!("CREATE TYPE {name} AS TABLE\r\n(\r\n{}\r\n)", lines.join(",\r\n"));
            if t.memory_optimized {
                s.push_str("\r\nWITH (MEMORY_OPTIMIZED = ON)");
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

/// SQL Compare's TRY/CATCH around a statement that must not stop the script silently.
pub fn try_catch(sql: &str) -> String {
    format!(
        "BEGIN TRY\r\n\t{sql}\r\nEND TRY\r\nBEGIN CATCH\r\n\tDECLARE @msg nvarchar(max);\r\n\tDECLARE @severity int;\r\n\tDECLARE @state int;\r\n\tSELECT @msg = ERROR_MESSAGE(), @severity = ERROR_SEVERITY(), @state = ERROR_STATE();\r\n\tRAISERROR(@msg, @severity, @state);\r\n\r\n\tSET NOEXEC ON\r\nEND CATCH"
    )
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
        assert_eq!(column_definition(&c, true), "[Id] [int] NOT NULL IDENTITY(1, 1)");
        let d = Column {
            name: "Bal".into(),
            data_type: DataType { name: "decimal".into(), precision: Some(18), scale: Some(2), ..Default::default() },
            nullable: true,
            default: Some(DefaultConstraint { name: "DF_Bal".into(), system_named: false, definition: "((0))".into() }),
            ..Default::default()
        };
        assert_eq!(column_definition(&d, true), "[Bal] [decimal] (18, 2) NULL CONSTRAINT [DF_Bal] DEFAULT ((0))");
        let t = |name: &str, length, scale| DataType { name: name.into(), length, scale, ..Default::default() };
        assert_eq!(type_sql(&t("nvarchar", Some(-1), None)), "[nvarchar] (max)");
        assert_eq!(type_sql(&t("datetime2", None, Some(7))), "[datetime2]");
        assert_eq!(type_sql(&t("datetime2", None, Some(3))), "[datetime2] (3)");
    }

    #[test]
    fn strings_for_dynamic_sql() {
        assert_eq!(sql_string("it's"), "N'it''s'");
        assert_eq!(sql_string("a\\\r\nb"), "N'a\\\\\r\n\r\nb'");
    }

    #[test]
    fn index_options_follow_the_options() {
        let ix = Index {
            name: "IX".into(),
            columns: vec![IndexColumn { name: "a".into(), descending: false }],
            fill_factor: 80,
            allow_row_locks: true,
            allow_page_locks: false,
            storage: Storage { data_space: Some("PRIMARY".into()), ..Default::default() },
            ..Default::default()
        };
        let r = Render::display();
        assert_eq!(create_index_with("[dbo].[T]", &ix, &r, &Host::default())[0], "CREATE NONCLUSTERED INDEX [IX] ON [dbo].[T] ([a]) WITH (FILLFACTOR=80, ALLOW_PAGE_LOCKS=OFF) ON [PRIMARY]");
        let r = Render { opts: Options::default_set().with(Opt::AddOnlineOn), features: Features::latest() };
        assert_eq!(create_index_with("[dbo].[T]", &ix, &r, &Host::default())[0], "CREATE NONCLUSTERED INDEX [IX] ON [dbo].[T] ([a]) WITH (ALLOW_PAGE_LOCKS=OFF, ONLINE = ON)");
    }

    #[test]
    fn xml_and_spatial_indexes() {
        let col = vec![IndexColumn { name: "Doc".into(), descending: false }];
        let primary = Index { name: "PX".into(), kind: IndexKind::Xml, columns: col.clone(), allow_row_locks: true, allow_page_locks: true, xml: Some(XmlIndex::default()), ..Default::default() };
        let r = Render::display();
        assert_eq!(create_index_with("[dbo].[T]", &primary, &r, &Host::default())[0], "CREATE PRIMARY XML INDEX [PX]\r\nON [dbo].[T] ([Doc])\r\n");
        let secondary = Index {
            name: "SX".into(),
            xml: Some(XmlIndex { kind: XmlIndexKind::Secondary, secondary_type: Some("PATH".into()), using_index: Some("PX".into()), ..Default::default() }),
            ..primary.clone()
        };
        assert_eq!(
            create_index_with("[dbo].[T]", &secondary, &r, &Host::default())[0],
            "CREATE XML INDEX [SX]\r\nON [dbo].[T] ([Doc])\r\nUSING XML INDEX [PX]\r\nFOR PATH\r\n"
        );
        let selective = Index {
            name: "SEL".into(),
            xml: Some(XmlIndex {
                kind: XmlIndexKind::Selective,
                paths: vec![
                    SelectivePath { id: 1, name: "p1".into(), path: "/a/b".into(), xquery_type: Some("xs:string".into()), max_length: Some(20), singleton: true, ..Default::default() },
                    SelectivePath { id: 2, name: "p2".into(), path: "/a/c".into(), sql: true, data_type: Some(DataType { name: "int".into(), ..Default::default() }), ..Default::default() },
                ],
                ..Default::default()
            }),
            ..primary.clone()
        };
        assert_eq!(
            create_index_with("[dbo].[T]", &selective, &r, &Host::default())[0],
            "CREATE SELECTIVE XML INDEX [SEL]\r\nON [dbo].[T] ([Doc])\r\nFOR (\r\np1 = '/a/b' AS XQUERY 'xs:string' MAXLENGTH(20) SINGLETON,\r\np2 = '/a/c' AS SQL [int]\r\n)\r\n"
        );
        let spatial = Index {
            name: "SP".into(),
            kind: IndexKind::Spatial,
            columns: vec![IndexColumn { name: "Shape".into(), descending: false }],
            allow_row_locks: true,
            allow_page_locks: true,
            spatial: Some(SpatialIndex {
                tessellation: "GEOMETRY_GRID".into(),
                bounding_box: vec!["0".into(), "0".into(), "100".into(), "50.5".into()],
                grids: vec!["LOW".into(), "MEDIUM".into(), "HIGH".into(), "MEDIUM".into()],
                cells_per_object: 16,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            create_index_with("[dbo].[T]", &spatial, &r, &Host::default())[0],
            "CREATE SPATIAL INDEX [SP] ON [dbo].[T] ([Shape]) USING geometry_grid  WITH (BOUNDING_BOX = (0, 0, 100, 50.5), GRIDS = (LOW, MEDIUM, HIGH, MEDIUM), CELLS_PER_OBJECT = 16)"
        );
        let geo = Index { spatial: Some(SpatialIndex { geography: true, tessellation: "GEOGRAPHY_AUTO_GRID".into(), cells_per_object: 12, ..Default::default() }), ..spatial };
        assert_eq!(create_index_with("[dbo].[T]", &geo, &r, &Host::default())[0], "CREATE SPATIAL INDEX [SP] ON [dbo].[T] ([Shape]) WITH (CELLS_PER_OBJECT = 12)");
    }

    #[test]
    fn encryption_in_headers() {
        assert_eq!(with_encryption("CREATE VIEW v\nAS SELECT 1", true), "CREATE VIEW v\nWITH ENCRYPTION AS SELECT 1");
        assert_eq!(with_encryption("CREATE VIEW v WITH SCHEMABINDING AS SELECT 1", true), "CREATE VIEW v WITH ENCRYPTION,  SCHEMABINDING AS SELECT 1");
        assert_eq!(with_encryption("CREATE PROC p @a AS int AS SELECT @a", true), "CREATE PROC p @a AS int WITH ENCRYPTION AS SELECT @a");
        assert_eq!(with_encryption("CREATE VIEW v WITH ENCRYPTION, SCHEMABINDING AS SELECT 1", false), "CREATE VIEW v WITH SCHEMABINDING AS SELECT 1");
        assert_eq!(with_encryption("CREATE VIEW v WITH ENCRYPTION AS SELECT 1", false), "CREATE VIEW v AS SELECT 1");
        assert_eq!(with_encryption("CREATE TRIGGER t ON x AFTER INSERT AS SELECT 1", true), "CREATE TRIGGER t ON x WITH ENCRYPTION AFTER INSERT AS SELECT 1");
        assert_eq!(with_encryption("CREATE FUNCTION f(@a int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN @a END", true), "CREATE FUNCTION f(@a int) RETURNS int WITH ENCRYPTION,  SCHEMABINDING AS BEGIN RETURN @a END");
    }
}
