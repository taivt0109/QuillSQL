//! Catalog reads for the objects of [`super::objects`]: one set-based query per kind, gated by
//! the server version (and left out on Azure SQL Database where the feature does not exist).

use std::collections::HashMap;

use super::model::*;
use super::objects::*;
use super::read::{Owner, Reader, SQL2012, SQL2016, SQL2019, USER_OBJECT, Version, flag, i, int, s, system_type, text};
use crate::db::{self, Connection};

/// A boundary value or parameter default (an sql_variant `v`) as a T-SQL literal.
const VARIANT_LITERAL: &str = "CASE WHEN {v} IS NULL THEN N'NULL'
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) IN (N'char', N'varchar', N'nchar', N'nvarchar')
          THEN N'N''' + REPLACE(CONVERT(nvarchar(4000), {v}), N'''', N'''''') + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) = N'date' THEN N'N''' + CONVERT(nvarchar(10), CONVERT(date, {v}), 23) + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) IN (N'datetime', N'smalldatetime')
          THEN N'N''' + CONVERT(nvarchar(23), CONVERT(datetime, {v}), 121) + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) = N'datetime2' THEN N'N''' + CONVERT(nvarchar(27), CONVERT(datetime2, {v}), 121) + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) = N'time' THEN N'N''' + CONVERT(nvarchar(16), CONVERT(time, {v}), 121) + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) = N'datetimeoffset'
          THEN N'N''' + CONVERT(nvarchar(34), CONVERT(datetimeoffset, {v}), 121) + N''''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) IN (N'binary', N'varbinary') THEN CONVERT(nvarchar(4000), CONVERT(varbinary(4000), {v}), 1)
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) = N'uniqueidentifier' THEN N'''{' + CONVERT(nvarchar(36), {v}) + N'}'''
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) IN (N'float', N'real') THEN CONVERT(nvarchar(40), CONVERT(float, {v}), 2)
     WHEN CONVERT(sysname, SQL_VARIANT_PROPERTY({v}, 'BaseType')) IN (N'money', N'smallmoney') THEN CONVERT(nvarchar(40), CONVERT(money, {v}), 2)
     ELSE CONVERT(nvarchar(4000), {v}) END";

fn variant_literal(v: &str) -> String {
    VARIANT_LITERAL.replace("{v}", v)
}

/// A type from catalog columns `name, schema-if-user-defined, max_length, precision, scale` at `at`.
fn data_type(r: &tiberius::Row, at: usize) -> DataType {
    match text(r, at + 1) {
        Some(schema) => DataType { name: s(r, at), schema: Some(schema), ..Default::default() },
        None => system_type(&s(r, at), i(r, at + 2), i(r, at + 3), i(r, at + 4)),
    }
}

/// `[schema].[name]` of a quoted two-part name read from the catalog (`[dbo].[p]`).
fn two_part(quoted: &str) -> ObjectName {
    let parts = split_quoted(quoted);
    match parts.as_slice() {
        [schema, name] => ObjectName::new(schema, name),
        [name] => ObjectName::new("dbo", name),
        _ => ObjectName::new("", quoted),
    }
}

fn split_quoted(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut inside = false;
    while let Some(c) = chars.next() {
        match c {
            '[' if !inside => inside = true,
            ']' if inside => {
                if chars.peek() == Some(&']') {
                    chars.next();
                    cur.push(']');
                } else {
                    inside = false;
                }
            }
            '.' if !inside => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

/// `([dbo].[fn]([TenantId]))` → (`[dbo].[fn]`, `([TenantId])`).
pub(crate) fn split_predicate(definition: &str) -> (String, String) {
    let mut d = definition.trim();
    while d.starts_with('(') && d.ends_with(')') && balanced(&d[1..d.len() - 1]) {
        d = d[1..d.len() - 1].trim();
    }
    let mut inside = false;
    for (at, c) in d.char_indices() {
        match c {
            '[' => inside = true,
            ']' => inside = false,
            '(' if !inside => return (d[..at].trim().to_string(), d[at..].trim().to_string()),
            _ => {}
        }
    }
    (d.to_string(), String::new())
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn permission_set(desc: &str) -> String {
    match desc {
        "EXTERNAL_ACCESS" => "EXTERNAL_ACCESS",
        "UNSAFE_ACCESS" => "UNSAFE",
        _ => "SAFE",
    }
    .into()
}

impl Reader {
    /// External tables own columns like tables do, so they are read before the columns.
    pub(super) async fn external_tables(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        if v.major < SQL2016 {
            return Ok(());
        }
        let rows = db::query(
            conn,
            "SELECT et.object_id, SCHEMA_NAME(et.schema_id), et.name, ds.name, ff.name, et.location,
                    et.reject_type, CONVERT(nvarchar(40), et.reject_value), CONVERT(nvarchar(40), et.reject_sample_value),
                    et.remote_schema_name, et.remote_object_name, CAST(et.distribution_type AS int), COL_NAME(et.object_id, et.sharding_col_id)
             FROM sys.external_tables AS et
             JOIN sys.external_data_sources AS ds ON ds.data_source_id = et.data_source_id
             LEFT JOIN sys.external_file_formats AS ff ON ff.file_format_id = et.file_format_id
             WHERE et.is_ms_shipped = 0
             ORDER BY 2, 3",
        )
        .await?;
        for r in rows {
            let distribution = match int(&r, 11) {
                Some(0) => Some(format!("SHARDED({})", crate::db::quote(&s(&r, 12)))),
                Some(1) => Some("REPLICATED".into()),
                Some(2) => Some("ROUND_ROBIN".into()),
                _ => None,
            };
            self.owners.insert(i(&r, 0), Owner::ExternalTable(self.db.external_tables.len()));
            self.db.external_tables.push(ExternalTable {
                schema: s(&r, 1),
                name: s(&r, 2),
                columns: Vec::new(),
                data_source: s(&r, 3),
                file_format: text(&r, 4),
                location: text(&r, 5),
                reject_type: text(&r, 6).map(|t| t.to_ascii_uppercase()),
                reject_value: text(&r, 7),
                reject_sample_value: text(&r, 8),
                remote_schema: text(&r, 9),
                remote_object: text(&r, 10),
                distribution,
            });
        }
        Ok(())
    }

    /// Everything else of [`super::objects`], once tables, modules and principals are read.
    pub(super) async fn more_objects(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let azure_db = self.db.server.is_azure_sql_database();
        self.assemblies(conn).await?;
        self.clr_signatures(conn).await?;
        self.xml_schema_collections(conn).await?;
        self.bindables(conn).await?;
        self.partitioning(conn, v).await?;
        self.statistics(conn, v).await?;
        self.fulltext(conn, v, azure_db).await?;
        if v.major >= SQL2019 {
            self.edge_constraints(conn).await?;
        }
        self.sensitivity(conn, v).await?;
        if !azure_db {
            self.service_broker(conn).await?;
        }
        self.keys(conn).await?;
        if v.major >= SQL2016 {
            self.security_policies(conn).await?;
            self.polybase(conn, v).await?;
        }
        Ok(())
    }

    async fn assemblies(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT a.assembly_id, a.name, USER_NAME(a.principal_id), a.clr_name, a.is_visible, a.permission_set_desc
             FROM sys.assemblies AS a WHERE a.is_user_defined = 1 ORDER BY a.name",
        )
        .await?;
        let mut ids = HashMap::new();
        for r in rows {
            ids.insert(i(&r, 0), self.db.assemblies.len());
            self.db.assemblies.push(Assembly {
                name: s(&r, 1),
                owner: text(&r, 2),
                clr_name: s(&r, 3),
                visible: flag(&r, 4),
                permission_set: permission_set(&s(&r, 5)),
                files: Vec::new(),
                references: Vec::new(),
            });
        }
        if ids.is_empty() {
            return Ok(());
        }
        let rows = db::query(
            conn,
            "SELECT f.assembly_id, f.name, CONVERT(varchar(max), f.content, 2)
             FROM sys.assembly_files AS f JOIN sys.assemblies AS a ON a.assembly_id = f.assembly_id
             WHERE a.is_user_defined = 1 ORDER BY f.assembly_id, f.file_id",
        )
        .await?;
        for r in rows {
            if let Some(&a) = ids.get(&i(&r, 0)) {
                self.db.assemblies[a].files.push(AssemblyFile { name: s(&r, 1), content: s(&r, 2) });
            }
        }
        let rows = db::query(
            conn,
            "SELECT ar.assembly_id, ra.name FROM sys.assembly_references AS ar
             JOIN sys.assemblies AS ra ON ra.assembly_id = ar.referenced_assembly_id
             WHERE ra.is_user_defined = 1 ORDER BY ar.assembly_id, ra.name",
        )
        .await?;
        for r in rows {
            if let Some(&a) = ids.get(&i(&r, 0)) {
                self.db.assemblies[a].references.push(s(&r, 1));
            }
        }
        Ok(())
    }

    /// Parameters, return types and result columns of CLR modules (T-SQL modules have them in their text).
    async fn clr_signatures(&mut self, conn: &mut Connection) -> Result<(), String> {
        if !self.db.routines.iter().any(|r| r.clr_method.is_some()) {
            return Ok(());
        }
        let sql = format!(
            "SELECT p.object_id, p.parameter_id, p.name, TYPE_NAME(p.user_type_id),
                    CASE WHEN t.is_user_defined = 1 THEN SCHEMA_NAME(t.schema_id) END,
                    CAST(p.max_length AS int), CAST(p.precision AS int), CAST(p.scale AS int),
                    p.is_output, p.has_default_value, {default}
             FROM sys.parameters AS p
             JOIN sys.objects AS o ON o.object_id = p.object_id AND o.type IN ('PC', 'FS', 'FT', 'AF')
             JOIN sys.types AS t ON t.user_type_id = p.user_type_id
             ORDER BY p.object_id, p.parameter_id",
            default = variant_literal("p.default_value"),
        );
        let rows = db::query(conn, &sql).await?;
        let index: HashMap<ObjectName, usize> =
            self.db.routines.iter().enumerate().map(|(k, r)| (ObjectName::new(&r.schema, &r.name), k)).collect();
        for r in rows {
            let Some(name) = self.objects.get(&i(&r, 0)).map(|(_, n)| n.clone()) else { continue };
            let Some(&k) = index.get(&name) else { continue };
            let ty = data_type(&r, 3).sql();
            let routine = &mut self.db.routines[k];
            if i(&r, 1) == 0 {
                routine.returns = Some(ty);
                continue;
            }
            routine.parameters.push(Parameter {
                name: s(&r, 2),
                data_type: ty,
                output: flag(&r, 8),
                default: if flag(&r, 9) { text(&r, 10) } else { None },
            });
        }
        let rows = db::query(
            conn,
            "SELECT c.object_id, c.name, TYPE_NAME(c.user_type_id), CASE WHEN t.is_user_defined = 1 THEN SCHEMA_NAME(t.schema_id) END,
                    CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int), c.is_nullable, c.collation_name
             FROM sys.columns AS c
             JOIN sys.objects AS o ON o.object_id = c.object_id AND o.type = 'FT'
             JOIN sys.types AS t ON t.user_type_id = c.user_type_id
             ORDER BY c.object_id, c.column_id",
        )
        .await?;
        for r in rows {
            let Some(name) = self.objects.get(&i(&r, 0)).map(|(_, n)| n.clone()) else { continue };
            let Some(&k) = index.get(&name) else { continue };
            let routine = &mut self.db.routines[k];
            let ordinal = routine.result_columns.len() as u32 + 1;
            routine.result_columns.push(Column {
                name: s(&r, 1),
                ordinal,
                data_type: data_type(&r, 2),
                nullable: flag(&r, 7),
                collation: text(&r, 8),
                ..Default::default()
            });
        }
        Ok(())
    }

    async fn xml_schema_collections(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT SCHEMA_NAME(x.schema_id), x.name, USER_NAME(x.principal_id),
                    CONVERT(nvarchar(max), XML_SCHEMA_NAMESPACE(SCHEMA_NAME(x.schema_id), x.name))
             FROM sys.xml_schema_collections AS x WHERE x.xml_collection_id > 65535 ORDER BY 1, 2",
        )
        .await?;
        for r in rows {
            self.db.xml_schema_collections.push(XmlSchemaCollection { schema: s(&r, 0), name: s(&r, 1), owner: text(&r, 2), definition: s(&r, 3) });
        }
        Ok(())
    }

    /// CREATE DEFAULT and CREATE RULE objects (default constraints have a parent).
    async fn bindables(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            &format!(
                "SELECT RTRIM(o.type), SCHEMA_NAME(o.schema_id), o.name, m.definition
                 FROM sys.objects AS o JOIN sys.sql_modules AS m ON m.object_id = o.object_id
                 WHERE ((o.type = 'D' AND o.parent_object_id = 0) OR o.type = 'R') AND {USER_OBJECT}
                 ORDER BY 2, 3"
            ),
        )
        .await?;
        for r in rows {
            let object = BindableObject { schema: s(&r, 1), name: s(&r, 2), definition: s(&r, 3) };
            if s(&r, 0) == "R" { self.db.rules.push(object) } else { self.db.defaults.push(object) }
        }
        Ok(())
    }

    async fn partitioning(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let system = v.pick(SQL2012, "WHERE pf.is_system = 0", "");
        let rows = db::query(
            conn,
            &format!(
                "SELECT pf.function_id, pf.name, pf.boundary_value_on_right, TYPE_NAME(pp.system_type_id),
                        CAST(pp.max_length AS int), CAST(pp.precision AS int), CAST(pp.scale AS int)
                 FROM sys.partition_functions AS pf
                 JOIN sys.partition_parameters AS pp ON pp.function_id = pf.function_id AND pp.parameter_id = 1
                 {system} ORDER BY pf.name"
            ),
        )
        .await?;
        let mut ids = HashMap::new();
        for r in rows {
            ids.insert(i(&r, 0), self.db.partition_functions.len());
            self.db.partition_functions.push(PartitionFunction {
                name: s(&r, 1),
                range_right: flag(&r, 2),
                data_type: system_type(&s(&r, 3), i(&r, 4), i(&r, 5), i(&r, 6)),
                boundaries: Vec::new(),
            });
        }
        if ids.is_empty() {
            return Ok(());
        }
        let rows = db::query(
            conn,
            &format!(
                "SELECT rv.function_id, {value} FROM sys.partition_range_values AS rv
                 WHERE rv.parameter_id = 1 ORDER BY rv.function_id, rv.boundary_id",
                value = variant_literal("rv.value")
            ),
        )
        .await?;
        for r in rows {
            if let Some(&f) = ids.get(&i(&r, 0)) {
                self.db.partition_functions[f].boundaries.push(s(&r, 1));
            }
        }
        let system = v.pick(SQL2012, "WHERE ps.is_system = 0", "");
        let rows = db::query(
            conn,
            &format!(
                "SELECT ps.name, pf.name, ds.name FROM sys.partition_schemes AS ps
                 JOIN sys.partition_functions AS pf ON pf.function_id = ps.function_id
                 JOIN sys.destination_data_spaces AS dds ON dds.partition_scheme_id = ps.data_space_id
                 JOIN sys.data_spaces AS ds ON ds.data_space_id = dds.data_space_id
                 {system} ORDER BY ps.name, dds.destination_id"
            ),
        )
        .await?;
        for r in rows {
            let name = s(&r, 0);
            if self.db.partition_schemes.last().is_none_or(|p| p.name != name) {
                self.db.partition_schemes.push(PartitionScheme { name, function: s(&r, 1), filegroups: Vec::new() });
            }
            self.db.partition_schemes.last_mut().unwrap().filegroups.push(s(&r, 2));
        }
        Ok(())
    }

    /// CREATE STATISTICS objects: user-created statistics that no index stands behind.
    async fn statistics(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let sql = format!(
            "SELECT st.object_id, st.stats_id, st.name, st.filter_definition, st.no_recompute, {incremental}
             FROM sys.stats AS st JOIN sys.objects AS o ON o.object_id = st.object_id AND o.type IN ('U', 'V')
             WHERE st.user_created = 1 AND NOT EXISTS (SELECT 1 FROM sys.indexes AS ix WHERE ix.object_id = st.object_id AND ix.name = st.name)
             ORDER BY st.object_id, st.name",
            incremental = v.pick(super::read::SQL2014, "st.is_incremental", "CAST(0 AS bit)"),
        );
        let mut slots = HashMap::new();
        for r in db::query(conn, &sql).await? {
            let stat = Statistic { name: s(&r, 2), filter: text(&r, 3), no_recompute: flag(&r, 4), incremental: flag(&r, 5), columns: Vec::new() };
            let list = match self.owners.get(&i(&r, 0)).copied() {
                Some(Owner::Table(t)) => &mut self.db.tables[t].statistics,
                Some(Owner::View(x)) => &mut self.db.views[x].statistics,
                _ => continue,
            };
            list.push(stat);
            slots.insert((i(&r, 0), i(&r, 1)), list.len() - 1);
        }
        if slots.is_empty() {
            return Ok(());
        }
        let rows = db::query(
            conn,
            "SELECT sc.object_id, sc.stats_id, c.name FROM sys.stats_columns AS sc
             JOIN sys.stats AS st ON st.object_id = sc.object_id AND st.stats_id = sc.stats_id AND st.user_created = 1
             JOIN sys.columns AS c ON c.object_id = sc.object_id AND c.column_id = sc.column_id
             ORDER BY sc.object_id, sc.stats_id, sc.stats_column_id",
        )
        .await?;
        for r in rows {
            let Some(&slot) = slots.get(&(i(&r, 0), i(&r, 1))) else { continue };
            let list = match self.owners.get(&i(&r, 0)).copied() {
                Some(Owner::Table(t)) => &mut self.db.tables[t].statistics,
                Some(Owner::View(x)) => &mut self.db.views[x].statistics,
                _ => continue,
            };
            list[slot].columns.push(s(&r, 2));
        }
        Ok(())
    }

    async fn fulltext(&mut self, conn: &mut Connection, v: &Version, azure_db: bool) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT c.name, USER_NAME(c.principal_id), c.is_default, c.is_accent_sensitivity_on FROM sys.fulltext_catalogs AS c ORDER BY c.name",
        )
        .await?;
        for r in rows {
            self.db.fulltext_catalogs.push(FullTextCatalog { name: s(&r, 0), owner: text(&r, 1), is_default: flag(&r, 2), accent_sensitive: flag(&r, 3) });
        }
        let rows = db::query(conn, "SELECT sl.name, USER_NAME(sl.principal_id) FROM sys.fulltext_stoplists AS sl ORDER BY sl.name").await?;
        for r in rows {
            self.db.fulltext_stoplists.push(FullTextStoplist { name: s(&r, 0), owner: text(&r, 1), words: Vec::new() });
        }
        if !self.db.fulltext_stoplists.is_empty() {
            let rows = db::query(
                conn,
                "SELECT sl.name, sw.stopword, sw.language_id FROM sys.fulltext_stopwords AS sw
                 JOIN sys.fulltext_stoplists AS sl ON sl.stoplist_id = sw.stoplist_id",
            )
            .await?;
            for r in rows {
                if let Some(list) = self.db.fulltext_stoplists.iter_mut().find(|l| l.name == s(&r, 0)) {
                    list.words.push(Stopword { word: s(&r, 1), language: i(&r, 2) });
                }
            }
            for list in &mut self.db.fulltext_stoplists {
                list.words.sort();
            }
        }
        let property_lists = v.major >= SQL2012 && !azure_db;
        if property_lists {
            let rows = db::query(
                conn,
                "SELECT pl.name, USER_NAME(pl.principal_id), p.property_name, CONVERT(nvarchar(36), p.property_set_guid), p.property_int_id, p.property_description
                 FROM sys.registered_search_property_lists AS pl
                 LEFT JOIN sys.registered_search_properties AS p ON p.property_list_id = pl.property_list_id
                 ORDER BY pl.name, p.property_name",
            )
            .await?;
            for r in rows {
                let name = s(&r, 0);
                if self.db.search_property_lists.last().is_none_or(|l| l.name != name) {
                    self.db.search_property_lists.push(SearchPropertyList { name, owner: text(&r, 1), properties: Vec::new() });
                }
                if let Some(p) = text(&r, 2) {
                    let property = SearchProperty { name: p, set_guid: s(&r, 3).to_uppercase(), int_id: i(&r, 4), description: text(&r, 5) };
                    self.db.search_property_lists.last_mut().unwrap().properties.push(property);
                }
            }
        }
        let sql = format!(
            "SELECT fi.object_id, c.name, ix.name, ds.name, RTRIM(fi.change_tracking_state), fi.stoplist_id, sl.name, {pl}
             FROM sys.fulltext_indexes AS fi
             JOIN sys.fulltext_catalogs AS c ON c.fulltext_catalog_id = fi.fulltext_catalog_id
             JOIN sys.indexes AS ix ON ix.object_id = fi.object_id AND ix.index_id = fi.unique_index_id
             LEFT JOIN sys.data_spaces AS ds ON ds.data_space_id = fi.data_space_id
             LEFT JOIN sys.fulltext_stoplists AS sl ON sl.stoplist_id = fi.stoplist_id
             {pl_join}",
            pl = if property_lists { "pl.name" } else { "CAST(NULL AS sysname)" },
            pl_join = if property_lists { "LEFT JOIN sys.registered_search_property_lists AS pl ON pl.property_list_id = fi.property_list_id" } else { "" },
        );
        let mut any = false;
        for r in db::query(conn, &sql).await? {
            let stoplist = match int(&r, 5) {
                None => "OFF".to_string(),
                Some(0) => "SYSTEM".to_string(),
                Some(_) => s(&r, 6),
            };
            let index = FullTextIndex {
                catalog: s(&r, 1),
                key_index: s(&r, 2),
                filegroup: text(&r, 3),
                change_tracking: match s(&r, 4).as_str() {
                    "M" => "MANUAL",
                    "O" => "OFF",
                    _ => "AUTO",
                }
                .into(),
                stoplist,
                property_list: text(&r, 7),
                columns: Vec::new(),
            };
            match self.owners.get(&i(&r, 0)).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].fulltext_index = Some(index),
                Some(Owner::View(x)) => self.db.views[x].fulltext_index = Some(index),
                _ => continue,
            }
            any = true;
        }
        if !any {
            return Ok(());
        }
        let rows = db::query(
            conn,
            "SELECT fc.object_id, c.name, tc.name, fc.language_id FROM sys.fulltext_index_columns AS fc
             JOIN sys.columns AS c ON c.object_id = fc.object_id AND c.column_id = fc.column_id
             LEFT JOIN sys.columns AS tc ON tc.object_id = fc.object_id AND tc.column_id = fc.type_column_id
             ORDER BY fc.object_id, c.name",
        )
        .await?;
        for r in rows {
            let column = FullTextColumn { name: s(&r, 1), type_column: text(&r, 2), language: i(&r, 3) };
            let index = match self.owners.get(&i(&r, 0)).copied() {
                Some(Owner::Table(t)) => self.db.tables[t].fulltext_index.as_mut(),
                Some(Owner::View(x)) => self.db.views[x].fulltext_index.as_mut(),
                _ => None,
            };
            if let Some(index) = index {
                index.columns.push(column);
            }
        }
        Ok(())
    }

    async fn edge_constraints(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT ec.parent_object_id, ec.name, ec.is_system_named, ec.is_disabled, ec.is_not_trusted,
                    CAST(ec.delete_referential_action AS int), ecc.from_object_id, ecc.to_object_id
             FROM sys.edge_constraints AS ec JOIN sys.edge_constraint_clauses AS ecc ON ecc.object_id = ec.object_id
             ORDER BY ec.parent_object_id, ec.name, ecc.clause_number",
        )
        .await?;
        for r in rows {
            let Some(Owner::Table(t)) = self.owners.get(&i(&r, 0)).copied() else { continue };
            let (Some(from), Some(to)) = (self.objects.get(&i(&r, 6)).map(|o| o.1.clone()), self.objects.get(&i(&r, 7)).map(|o| o.1.clone())) else { continue };
            let name = s(&r, 1);
            let list = &mut self.db.tables[t].edge_constraints;
            if list.last().is_none_or(|e| e.name != name) {
                list.push(EdgeConstraint {
                    name,
                    system_named: flag(&r, 2),
                    disabled: flag(&r, 3),
                    not_trusted: flag(&r, 4),
                    on_delete_cascade: i(&r, 5) == 1,
                    connections: Vec::new(),
                });
            }
            list.last_mut().unwrap().connections.push((from, to));
        }
        Ok(())
    }

    /// Sensitivity classification: the catalog view from SQL Server 2019, and the extended
    /// properties older tools wrote (taken out of the extended property list).
    async fn sensitivity(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let mut found: Vec<(i32, i32, Sensitivity)> = Vec::new();
        if v.major >= SQL2019 {
            let rows = db::query(
                conn,
                "SELECT sc.major_id, sc.minor_id, CONVERT(nvarchar(128), sc.label), CONVERT(nvarchar(128), sc.label_id),
                        CONVERT(nvarchar(128), sc.information_type), CONVERT(nvarchar(128), sc.information_type_id), sc.rank_desc
                 FROM sys.sensitivity_classifications AS sc WHERE sc.class = 1 AND sc.minor_id > 0",
            )
            .await?;
            for r in rows {
                let rank = text(&r, 6).filter(|x| x != "NONE");
                found.push((i(&r, 0), i(&r, 1), Sensitivity { label: text(&r, 2), label_id: text(&r, 3), information_type: text(&r, 4), information_type_id: text(&r, 5), rank }));
            }
        }
        let rows = db::query(
            conn,
            "SELECT ep.major_id, ep.minor_id, ep.name, CONVERT(nvarchar(128), ep.value) FROM sys.extended_properties AS ep
             WHERE ep.class = 1 AND ep.minor_id > 0
               AND ep.name IN (N'sys_sensitivity_label_name', N'sys_sensitivity_label_id', N'sys_information_type_name', N'sys_information_type_id')",
        )
        .await?;
        for r in rows {
            let (major, minor) = (i(&r, 0), i(&r, 1));
            let at = match found.iter().position(|(a, b, _)| *a == major && *b == minor) {
                Some(at) => at,
                None => {
                    found.push((major, minor, Sensitivity::default()));
                    found.len() - 1
                }
            };
            let entry = &mut found[at].2;
            let slot = match s(&r, 2).as_str() {
                "sys_sensitivity_label_name" => &mut entry.label,
                "sys_sensitivity_label_id" => &mut entry.label_id,
                "sys_information_type_name" => &mut entry.information_type,
                _ => &mut entry.information_type_id,
            };
            if slot.is_none() {
                *slot = text(&r, 3);
            }
        }
        self.db.extended_properties.retain(|p| !(p.minor_type.as_deref() == Some("COLUMN") && p.name.starts_with("sys_") && is_classification_property(&p.name)));
        for (major, minor, sensitivity) in found {
            let Some(owner) = self.owners.get(&major).copied() else { continue };
            if matches!(owner, Owner::TableType(_)) {
                continue;
            }
            if let Some(c) = self.columns_of(owner).iter_mut().find(|c| c.ordinal == minor as u32) {
                c.sensitivity = Some(sensitivity);
            }
        }
        Ok(())
    }

    async fn service_broker(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT mt.name, USER_NAME(mt.principal_id), RTRIM(mt.validation), QUOTENAME(SCHEMA_NAME(x.schema_id)) + N'.' + QUOTENAME(x.name)
             FROM sys.service_message_types AS mt
             LEFT JOIN sys.xml_schema_collections AS x ON x.xml_collection_id = mt.xml_collection_id
             WHERE mt.message_type_id > 65535 ORDER BY mt.name",
        )
        .await?;
        for r in rows {
            let collection = text(&r, 3);
            let validation = match s(&r, 2).as_str() {
                "E" => "EMPTY",
                "X" if collection.is_some() => "VALID_XML",
                "X" => "WELL_FORMED_XML",
                _ => "NONE",
            };
            self.db.message_types.push(MessageType { name: s(&r, 0), owner: text(&r, 1), validation: validation.into(), xml_collection: collection });
        }
        let rows = db::query(
            conn,
            "SELECT c.name, USER_NAME(c.principal_id), mt.name, u.is_sent_by_initiator, u.is_sent_by_target
             FROM sys.service_contracts AS c
             LEFT JOIN sys.service_contract_message_usages AS u ON u.service_contract_id = c.service_contract_id
             LEFT JOIN sys.service_message_types AS mt ON mt.message_type_id = u.message_type_id
             WHERE c.name <> N'DEFAULT' AND c.name NOT LIKE N'http://schemas.microsoft.com/SQL/%'
             ORDER BY c.name, mt.name",
        )
        .await?;
        for r in rows {
            let name = s(&r, 0);
            if self.db.contracts.last().is_none_or(|c| c.name != name) {
                self.db.contracts.push(Contract { name, owner: text(&r, 1), messages: Vec::new() });
            }
            if let Some(m) = text(&r, 2) {
                let sent_by = match (flag(&r, 3), flag(&r, 4)) {
                    (true, true) => "ANY",
                    (true, false) => "INITIATOR",
                    _ => "TARGET",
                };
                self.db.contracts.last_mut().unwrap().messages.push(ContractMessage { message_type: m, sent_by: sent_by.into() });
            }
        }
        let sql = "SELECT q.object_id, SCHEMA_NAME(q.schema_id), q.name, USER_NAME(o.principal_id), q.is_enqueue_enabled, q.is_retention_enabled,
                    q.is_poison_message_handling_enabled, q.is_activation_enabled, q.activation_procedure, CAST(q.max_readers AS int),
                    CASE WHEN q.execute_as_principal_id = -2 THEN N'OWNER' ELSE USER_NAME(q.execute_as_principal_id) END, fg.name
             FROM sys.service_queues AS q JOIN sys.objects AS o ON o.object_id = q.object_id
             OUTER APPLY (SELECT TOP (1) ds.name FROM sys.internal_tables AS it
                          JOIN sys.indexes AS ix ON ix.object_id = it.object_id AND ix.index_id < 2
                          JOIN sys.data_spaces AS ds ON ds.data_space_id = ix.data_space_id
                          WHERE it.parent_object_id = q.object_id) AS fg
             WHERE q.is_ms_shipped = 0 ORDER BY 2, 3";
        let mut queues = HashMap::new();
        for r in db::query(conn, sql).await? {
            let activation = text(&r, 8).map(|procedure| QueueActivation {
                enabled: flag(&r, 7),
                procedure,
                max_readers: i(&r, 9),
                execute_as: text(&r, 10),
            });
            queues.insert(i(&r, 0), self.db.queues.len());
            self.db.queues.push(Queue {
                schema: s(&r, 1),
                name: s(&r, 2),
                owner: text(&r, 3),
                enabled: flag(&r, 4),
                retention: flag(&r, 5),
                poison_message_handling: flag(&r, 6),
                activation,
                filegroup: text(&r, 11),
                event_notifications: Vec::new(),
            });
        }
        let rows = db::query(
            conn,
            "SELECT s.name, USER_NAME(s.principal_id), SCHEMA_NAME(o.schema_id), o.name, c.name
             FROM sys.services AS s
             JOIN sys.objects AS o ON o.object_id = s.service_queue_id
             LEFT JOIN sys.service_contract_usages AS u ON u.service_id = s.service_id
             LEFT JOIN sys.service_contracts AS c ON c.service_contract_id = u.service_contract_id
             WHERE s.name NOT LIKE N'http://schemas.microsoft.com/SQL/%'
             ORDER BY s.name, c.name",
        )
        .await?;
        for r in rows {
            let name = s(&r, 0);
            if self.db.services.last().is_none_or(|x| x.name != name) {
                self.db.services.push(Service { name, owner: text(&r, 1), queue: ObjectName::new(&s(&r, 2), &s(&r, 3)), contracts: Vec::new() });
            }
            if let Some(c) = text(&r, 4) {
                self.db.services.last_mut().unwrap().contracts.push(c);
            }
        }
        let rows = db::query(
            conn,
            "SELECT r.name, USER_NAME(r.principal_id), r.remote_service_name, r.broker_instance, CONVERT(nvarchar(30), r.lifetime, 121),
                    r.address, r.mirror_address
             FROM sys.routes AS r WHERE r.name <> N'AutoCreatedLocal' ORDER BY r.name",
        )
        .await?;
        for r in rows {
            self.db.routes.push(Route {
                name: s(&r, 0),
                owner: text(&r, 1),
                remote_service: text(&r, 2),
                broker_instance: text(&r, 3),
                expires: text(&r, 4),
                address: s(&r, 5),
                mirror_address: text(&r, 6),
            });
        }
        let rows = db::query(
            conn,
            "SELECT b.name, USER_NAME(b.principal_id), b.remote_service_name, USER_NAME(b.remote_principal_id), b.is_anonymous_on
             FROM sys.remote_service_bindings AS b ORDER BY b.name",
        )
        .await?;
        for r in rows {
            self.db.service_bindings.push(ServiceBinding { name: s(&r, 0), owner: text(&r, 1), remote_service: s(&r, 2), user: s(&r, 3), anonymous: flag(&r, 4) });
        }
        let rows = db::query(
            conn,
            "SELECT en.object_id, en.name, en.parent_class, en.parent_id, en.service_name, en.broker_instance, e.type_desc
             FROM sys.event_notifications AS en LEFT JOIN sys.events AS e ON e.object_id = en.object_id
             WHERE en.parent_class IN (0, 1) ORDER BY en.object_id, e.type_desc",
        )
        .await?;
        let mut last = None;
        for r in rows {
            let id = i(&r, 0);
            if last != Some(id) {
                last = Some(id);
                let queue = if i(&r, 2) == 1 {
                    match queues.get(&i(&r, 3)) {
                        Some(&q) => Some(ObjectName::new(&self.db.queues[q].schema, &self.db.queues[q].name)),
                        None => continue,
                    }
                } else {
                    None
                };
                let en = EventNotification { name: s(&r, 1), queue: queue.clone(), service: s(&r, 4), broker_instance: s(&r, 5), ..Default::default() };
                match queue.and_then(|_| queues.get(&i(&r, 3)).copied()) {
                    Some(q) => self.db.queues[q].event_notifications.push(en),
                    None => self.db.event_notifications.push(en),
                }
            }
            let Some(event) = text(&r, 6) else { continue };
            let target = if i(&r, 2) == 1 {
                queues.get(&i(&r, 3)).and_then(|&q| self.db.queues[q].event_notifications.last_mut())
            } else {
                self.db.event_notifications.last_mut()
            };
            if let Some(en) = target {
                en.events.push(event);
            }
        }
        self.db.event_notifications.sort_by(|a, b| a.name.cmp(&b.name));
        for q in &mut self.db.queues {
            q.event_notifications.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(())
    }

    async fn keys(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT c.name, USER_NAME(c.principal_id), c.subject, c.issuer_name, CONVERT(nvarchar(100), c.thumbprint, 1),
                    CONVERT(nvarchar(30), c.start_date, 120), CONVERT(nvarchar(30), c.expiry_date, 120), c.pvt_key_encryption_type,
                    c.is_active_for_begin_dialog
             FROM sys.certificates AS c WHERE c.name NOT LIKE N'##%' ORDER BY c.name",
        )
        .await?;
        for r in rows {
            self.db.certificates.push(Certificate {
                name: s(&r, 0),
                owner: text(&r, 1),
                subject: s(&r, 2),
                issuer: s(&r, 3),
                thumbprint: s(&r, 4),
                start_date: s(&r, 5),
                expiry_date: s(&r, 6),
                private_key: s(&r, 7),
                active_for_dialog: flag(&r, 8),
            });
        }
        let rows = db::query(
            conn,
            "SELECT k.name, USER_NAME(k.principal_id), CONVERT(nvarchar(100), k.thumbprint, 1), k.algorithm_desc, k.key_length, k.pvt_key_encryption_type
             FROM sys.asymmetric_keys AS k ORDER BY k.name",
        )
        .await?;
        for r in rows {
            self.db.asymmetric_keys.push(AsymmetricKey {
                name: s(&r, 0),
                owner: text(&r, 1),
                thumbprint: s(&r, 2),
                algorithm: s(&r, 3),
                key_length: i(&r, 4),
                private_key: s(&r, 5),
            });
        }
        let rows = db::query(
            conn,
            "SELECT k.name, USER_NAME(k.principal_id), k.algorithm_desc, k.key_length, CONVERT(nvarchar(36), k.key_guid),
                    CONVERT(nvarchar(30), k.create_date, 120), CONVERT(nvarchar(30), k.modify_date, 120)
             FROM sys.symmetric_keys AS k WHERE k.name NOT LIKE N'##%' ORDER BY k.name",
        )
        .await?;
        for r in rows {
            self.db.symmetric_keys.push(SymmetricKey {
                name: s(&r, 0),
                owner: text(&r, 1),
                algorithm: s(&r, 2),
                key_length: i(&r, 3),
                key_guid: s(&r, 4),
                create_date: s(&r, 5),
                modify_date: s(&r, 6),
            });
        }
        Ok(())
    }

    async fn security_policies(&mut self, conn: &mut Connection) -> Result<(), String> {
        let rows = db::query(
            conn,
            "SELECT SCHEMA_NAME(sp.schema_id), sp.name, sp.is_enabled, sp.is_schema_bound, sp.is_not_for_replication,
                    p.predicate_type_desc, p.predicate_definition, OBJECT_SCHEMA_NAME(p.target_object_id), OBJECT_NAME(p.target_object_id),
                    p.operation_desc
             FROM sys.security_policies AS sp LEFT JOIN sys.security_predicates AS p ON p.object_id = sp.object_id
             WHERE sp.is_ms_shipped = 0
             ORDER BY 1, 2, p.security_predicate_id",
        )
        .await?;
        for r in rows {
            let (schema, name) = (s(&r, 0), s(&r, 1));
            if self.db.security_policies.last().is_none_or(|p| p.schema != schema || p.name != name) {
                self.db.security_policies.push(SecurityPolicy {
                    schema,
                    name,
                    enabled: flag(&r, 2),
                    schema_bound: flag(&r, 3),
                    not_for_replication: flag(&r, 4),
                    predicates: Vec::new(),
                });
            }
            let (Some(definition), Some(table)) = (text(&r, 6), text(&r, 8)) else { continue };
            let (function, arguments) = split_predicate(&definition);
            self.db.security_policies.last_mut().unwrap().predicates.push(SecurityPredicate {
                kind: s(&r, 5),
                function,
                arguments,
                table: ObjectName::new(&s(&r, 7), &table),
                operation: text(&r, 9).map(|o| o.replace('_', " ")),
            });
        }
        Ok(())
    }

    async fn polybase(&mut self, conn: &mut Connection, v: &Version) -> Result<(), String> {
        let rows = db::query(conn, "SELECT c.name FROM sys.database_scoped_credentials AS c ORDER BY c.name").await?;
        self.db.credentials = rows.iter().map(|r| s(r, 0)).collect();
        let sql = format!(
            "SELECT ds.name, ds.location, ds.type_desc, ds.resource_manager_location, c.name, ds.database_name, ds.shard_map_name, {more}
             FROM sys.external_data_sources AS ds LEFT JOIN sys.database_scoped_credentials AS c ON c.credential_id = ds.credential_id
             ORDER BY ds.name",
            more = v.pick(SQL2019, "ds.connection_options, CASE ds.pushdown WHEN N'ON' THEN 1 WHEN N'OFF' THEN 0 END", "CAST(NULL AS nvarchar(4000)), CAST(NULL AS int)"),
        );
        for r in db::query(conn, &sql).await? {
            self.db.external_data_sources.push(ExternalDataSource {
                name: s(&r, 0),
                location: s(&r, 1),
                kind: text(&r, 2).filter(|k| k != "NONE"),
                resource_manager_location: text(&r, 3),
                credential: text(&r, 4),
                database_name: text(&r, 5),
                shard_map_name: text(&r, 6),
                connection_options: text(&r, 7),
                pushdown: int(&r, 8).map(|p| p != 0),
            });
        }
        let sql = format!(
            "SELECT f.name, f.format_type, f.field_terminator, f.string_delimiter, f.date_format, f.use_type_default, f.serde_method,
                    f.encoding, f.data_compression, {first_row}
             FROM sys.external_file_formats AS f ORDER BY f.name",
            first_row = v.pick(SQL2019, "f.first_row", "CAST(NULL AS int)"),
        );
        for r in db::query(conn, &sql).await? {
            self.db.external_file_formats.push(ExternalFileFormat {
                name: s(&r, 0),
                format_type: s(&r, 1).to_ascii_uppercase(),
                field_terminator: text(&r, 2),
                string_delimiter: text(&r, 3),
                date_format: text(&r, 4).filter(|d| !d.is_empty()),
                use_type_default: flag(&r, 5),
                serde_method: text(&r, 6),
                encoding: text(&r, 7),
                data_compression: text(&r, 8),
                first_row: int(&r, 9).map(|n| n as i32).filter(|n| *n > 1),
            });
        }
        Ok(())
    }

    /// References of the new objects for dependency order: what a CLR module, a queue, a service
    /// or a policy needs before it can be created.
    pub(super) fn object_dependencies(&mut self) {
        object_dependencies(&mut self.db);
    }
}

/// Dependencies the server does not record: CLR modules on their assembly, Service Broker objects
/// on each other, typed columns on their XML schema collection...
pub(crate) fn object_dependencies(db: &mut Database) {
    {
        let mut extra: Vec<Dependency> = Vec::new();
        let assembly = |a: &str| ObjectName::new(KEY_ASSEMBLY, a);
        let first_part = |m: &str| split_quoted(m).into_iter().next().unwrap_or_default();
        for r in db.routines.iter() {
            if let Some(m) = &r.clr_method {
                extra.push(Dependency { from: ObjectName::new(&r.schema, &r.name), to: assembly(&first_part(m)), strong: true });
            }
        }
        for t in db.types.iter() {
            if let Some(c) = &t.clr_class {
                extra.push(Dependency { from: ObjectName::new(&t.schema, &t.name), to: assembly(&first_part(c)), strong: true });
            }
        }
        for a in db.assemblies.iter() {
            for r in &a.references {
                extra.push(Dependency { from: assembly(&a.name), to: assembly(r), strong: true });
            }
        }
        for t in db.tables.iter() {
            for tr in t.triggers.iter().filter_map(|tr| tr.clr_method.as_ref()) {
                extra.push(Dependency { from: t.object_name(), to: assembly(&first_part(tr)), strong: false });
            }
            for c in &t.columns {
                if let Some(x) = &c.data_type.xml_collection {
                    extra.push(Dependency { from: t.object_name(), to: two_part(x), strong: true });
                }
            }
            for e in &t.edge_constraints {
                for (a, b) in &e.connections {
                    extra.push(Dependency { from: t.object_name(), to: a.clone(), strong: false });
                    extra.push(Dependency { from: t.object_name(), to: b.clone(), strong: false });
                }
            }
        }
        for q in db.queues.iter() {
            if let Some(a) = &q.activation {
                extra.push(Dependency { from: ObjectName::new(&q.schema, &q.name), to: two_part(&a.procedure), strong: true });
            }
        }
        for c in db.contracts.iter() {
            for m in &c.messages {
                extra.push(Dependency { from: ObjectName::new(KEY_CONTRACT, &c.name), to: ObjectName::new(KEY_MESSAGE_TYPE, &m.message_type), strong: true });
            }
        }
        for m in db.message_types.iter() {
            if let Some(x) = &m.xml_collection {
                extra.push(Dependency { from: ObjectName::new(KEY_MESSAGE_TYPE, &m.name), to: two_part(x), strong: true });
            }
        }
        for s in db.services.iter() {
            extra.push(Dependency { from: ObjectName::new(KEY_SERVICE, &s.name), to: s.queue.clone(), strong: true });
            for c in &s.contracts {
                extra.push(Dependency { from: ObjectName::new(KEY_SERVICE, &s.name), to: ObjectName::new(KEY_CONTRACT, c), strong: true });
            }
        }
        for en in db.event_notifications.iter() {
            extra.push(Dependency { from: ObjectName::new(KEY_EVENT_NOTIFICATION, &en.name), to: ObjectName::new(KEY_SERVICE, &en.service), strong: false });
        }
        for p in db.security_policies.iter() {
            let from = ObjectName::new(&p.schema, &p.name);
            for pr in &p.predicates {
                extra.push(Dependency { from: from.clone(), to: two_part(&pr.function), strong: true });
                extra.push(Dependency { from: from.clone(), to: pr.table.clone(), strong: false });
            }
        }
        for t in db.external_tables.iter() {
            extra.push(Dependency { from: ObjectName::new(&t.schema, &t.name), to: ObjectName::new(KEY_EXTERNAL_DATA_SOURCE, &t.data_source), strong: true });
        }
        for d in extra {
            if d.from != d.to && !db.dependencies.iter().any(|x| x.from == d.from && x.to == d.to) {
                db.dependencies.push(d);
            }
        }
        db.dependencies.sort();
    }
}

fn is_classification_property(name: &str) -> bool {
    matches!(name, "sys_sensitivity_label_name" | "sys_sensitivity_label_id" | "sys_information_type_name" | "sys_information_type_id")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicates() {
        assert_eq!(split_predicate("([dbo].[fn_tenant]([TenantId]))"), ("[dbo].[fn_tenant]".into(), "([TenantId])".into()));
        assert_eq!(split_predicate("([rls].[f]([a],[b]))"), ("[rls].[f]".into(), "([a],[b])".into()));
        assert_eq!(two_part("[dbo].[Activate]"), ObjectName::new("dbo", "Activate"));
    }
}
