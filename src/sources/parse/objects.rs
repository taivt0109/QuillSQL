//! Scripts-folder statements for the objects of `catalog::objects`: CLR assemblies and
//! aggregates, XML schema collections, defaults and rules, partitioning, full-text search,
//! statistics, sensitivity classification, Service Broker, security policies and PolyBase.
//! What QuillSQL writes into a scripts folder reads back to the same model.

use super::*;
use crate::db::quote;

/// The phase of statements this module reads, when it is one of them.
pub(super) fn phase(c: &Cur) -> Option<u8> {
    let w = |k: usize| c.t.get(k).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
    Some(match (w(0).as_str(), w(1).as_str(), w(2).as_str()) {
        ("CREATE", "ASSEMBLY" | "DEFAULT" | "RULE" | "PARTITION" | "MESSAGE" | "CONTRACT" | "ROUTE" | "REMOTE" | "APPLICATION", _) => 0,
        ("CREATE", "XML", "SCHEMA") | ("CREATE", "FULLTEXT", "CATALOG" | "STOPLIST") | ("CREATE", "SEARCH", _) => 0,
        ("CREATE", "EXTERNAL", "DATA" | "FILE") => 0,
        ("ALTER", "ASSEMBLY", _) | ("ALTER", "FULLTEXT", "STOPLIST") | ("ALTER", "SEARCH", _) => 1,
        ("CREATE", "QUEUE" | "AGGREGATE", _) | ("CREATE", "EXTERNAL", "TABLE") => 1,
        ("CREATE", "SERVICE" | "STATISTICS" | "SECURITY", _) | ("CREATE", "FULLTEXT", "INDEX") | ("ALTER", "FULLTEXT", "INDEX") => 2,
        ("ADD", "SENSITIVITY", _) => 2,
        ("CREATE", "EVENT", _) => 3,
        _ => return None,
    })
}

/// `[a].[b]` of a multipart name as read.
fn quoted_parts(parts: &[String]) -> String {
    parts.iter().map(|p| quote(p)).collect::<Vec<_>>().join(".")
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl Builder {
    /// Reads one of this module's statements; None when it is not one of them.
    pub(super) fn object_statement(&mut self, c: &mut Cur) -> Option<R<()>> {
        let w = |k: usize| c.t.get(k).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
        let (w0, w1, w2) = (w(0), w(1), w(2));
        Some(match (w0.as_str(), w1.as_str(), w2.as_str()) {
            ("CREATE", "ASSEMBLY", _) => self.create_assembly(c),
            ("ALTER", "ASSEMBLY", _) => self.alter_assembly(c),
            ("CREATE", "AGGREGATE", _) => self.create_aggregate(c),
            ("CREATE", "XML", "SCHEMA") => self.create_xml_schema_collection(c),
            ("CREATE", "DEFAULT" | "RULE", _) => self.create_bindable(c, w1 == "RULE"),
            ("CREATE", "PARTITION", "FUNCTION") => self.create_partition_function(c),
            ("CREATE", "PARTITION", "SCHEME") => self.create_partition_scheme(c),
            ("CREATE", "FULLTEXT", "CATALOG") => self.create_fulltext_catalog(c),
            ("CREATE", "FULLTEXT", "STOPLIST") => self.create_stoplist(c),
            ("ALTER", "FULLTEXT", "STOPLIST") => self.alter_stoplist(c),
            ("CREATE", "SEARCH", _) => self.create_property_list(c),
            ("ALTER", "SEARCH", _) => self.alter_property_list(c),
            ("CREATE", "FULLTEXT", "INDEX") => self.create_fulltext_index(c),
            ("ALTER", "FULLTEXT", "INDEX") => self.alter_fulltext_index(c),
            ("CREATE", "STATISTICS", _) => self.create_statistics(c),
            ("ADD", "SENSITIVITY", _) => self.add_sensitivity(c),
            ("CREATE", "MESSAGE", _) => self.create_message_type(c),
            ("CREATE", "CONTRACT", _) => self.create_contract(c),
            ("CREATE", "QUEUE", _) => self.create_queue(c),
            ("CREATE", "SERVICE", _) => self.create_service(c),
            ("CREATE", "ROUTE", _) => self.create_route(c),
            ("CREATE", "REMOTE", _) => self.create_service_binding(c),
            ("CREATE", "EVENT", _) => self.create_event_notification(c),
            ("CREATE", "SECURITY", _) => self.create_security_policy(c),
            ("CREATE", "EXTERNAL", "DATA") => self.create_external_data_source(c),
            ("CREATE", "EXTERNAL", "FILE") => self.create_external_file_format(c),
            ("CREATE", "EXTERNAL", "TABLE") => self.create_external_table(c),
            ("CREATE", "APPLICATION", _) => self.create_application_role(c),
            ("ALTER", "TABLE", _) if c.t.iter().any(|t| t.kind == Tok::Word && t.text.eq_ignore_ascii_case("CONNECTION")) => self.add_edge_constraint(c),
            _ => return None,
        })
    }

    /// `ALTER TABLE [s].[e] [WITH NOCHECK] ADD CONSTRAINT [ec] CONNECTION ([a] TO [b], …) [ON DELETE CASCADE]`.
    fn add_edge_constraint(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let table = c.object(&self.schema_default())?;
        let Some(ti) = self.table_index(&table) else { return c.fail(MessageKind::ReferenceNotFound) };
        let not_trusted = c.eat_seq(&["WITH", "NOCHECK"]);
        c.eat_seq(&["WITH", "CHECK"]);
        c.expect("ADD")?;
        c.expect("CONSTRAINT")?;
        let name = c.ident()?;
        c.expect("CONNECTION")?;
        let (a, b) = c.group()?;
        let mut connections = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let from = s.object(&self.schema_default())?;
            s.expect("TO")?;
            connections.push((from, s.object(&self.schema_default())?));
        }
        let on_delete_cascade = c.eat_seq(&["ON", "DELETE", "CASCADE"]);
        let list = &mut self.db.tables[ti].edge_constraints;
        list.retain(|e| !same(&e.name, &name));
        list.push(EdgeConstraint { system_named: looks_system_named(&name), name, connections, on_delete_cascade, disabled: false, not_trusted });
        Ok(())
    }

    /// `0x…` up to the next `keyword` (line continuations taken out).
    fn hex_until(&self, c: &mut Cur, stops: &[&str]) -> R<String> {
        let start = c.i;
        while !c.done() && !stops.iter().any(|s| c.at(s)) {
            c.i += 1;
        }
        if c.i == start {
            return c.syntax();
        }
        let raw = c.text(start, c.i - 1);
        let hex: String = raw.trim().trim_start_matches("0x").trim_start_matches("0X").chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
        Ok(hex.to_ascii_uppercase())
    }

    fn string(&self, c: &mut Cur) -> R<String> {
        match c.peek() {
            Some(t) if t.kind == Tok::Str => {
                let s = string_value(&t.text);
                c.i += 1;
                Ok(s)
            }
            _ => c.syntax(),
        }
    }

    fn create_assembly(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let mut a = Assembly { name: name.clone(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            a.owner = Some(c.ident()?);
        }
        c.expect("FROM")?;
        let content = self.hex_until(c, &["WITH"])?;
        a.files.push(AssemblyFile { name: name.to_lowercase(), content });
        if c.eat("WITH") {
            c.expect("PERMISSION_SET")?;
            c.expect_punct("=")?;
            a.permission_set = c.ident()?.to_ascii_uppercase();
        }
        self.db.assemblies.retain(|x| !same(&x.name, &name));
        self.db.assemblies.push(a);
        self.origin(ObjectType::Assembly, "", &name);
        Ok(())
    }

    fn alter_assembly(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let Some(at) = self.db.assemblies.iter().position(|a| same(&a.name, &name)) else { return c.fail(MessageKind::ReferenceNotFound) };
        if c.eat_seq(&["ADD", "FILE", "FROM"]) {
            let content = self.hex_until(c, &["AS"])?;
            c.expect("AS")?;
            let file = self.string(c)?;
            self.db.assemblies[at].files.push(AssemblyFile { name: file, content });
        } else if c.eat_seq(&["WITH", "VISIBILITY"]) {
            c.expect_punct("=")?;
            self.db.assemblies[at].visible = c.on_off()?;
        }
        Ok(())
    }

    /// `(@a int, @b nvarchar(50) = N'x' OUTPUT)` (or without parentheses) up to `stops`.
    fn parameters(&self, c: &mut Cur, stops: &[&str]) -> R<Vec<Parameter>> {
        let (a, b) = if c.is_punct("(") {
            c.group()?
        } else {
            let a = c.i;
            while !c.done() && !stops.iter().any(|s| c.at(s)) {
                c.i += 1;
            }
            (a, c.i)
        };
        let mut out = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut p = c.sub(x, y);
            let name = p.peek().map(|t| t.text.clone()).unwrap_or_default();
            p.i += 1;
            p.eat("AS");
            let data_type = self.data_type(&mut p)?.sql();
            let mut default = None;
            let mut output = false;
            while !p.done() {
                if p.eat_punct("=") {
                    let start = p.i;
                    while !p.done() && !p.at("OUTPUT") && !p.at("OUT") && !p.at("READONLY") {
                        p.i += 1;
                    }
                    default = Some(p.text(start, p.i - 1).trim().to_string());
                } else if p.eat("OUTPUT") || p.eat("OUT") {
                    output = true;
                } else {
                    p.i += 1;
                }
            }
            out.push(Parameter { name, data_type, output, default });
        }
        Ok(out)
    }

    /// Parameters and return type of a CLR procedure or function read from its CREATE text.
    pub(super) fn clr_signature(&self, definition: &str, r: &mut Routine) {
        let mut c = Cur::new(definition);
        // CREATE [OR ALTER] PROCEDURE|FUNCTION name
        while !c.done() && !c.at("PROCEDURE") && !c.at("PROC") && !c.at("FUNCTION") {
            c.i += 1;
        }
        c.i += 1;
        if c.multipart().is_err() {
            return;
        }
        if let Ok(p) = self.parameters(&mut c, &["WITH", "AS", "RETURNS"]) {
            r.parameters = p;
        }
        if c.eat("RETURNS") {
            if c.eat("TABLE") {
                if let Ok((a, b)) = c.group() {
                    for (x, y) in c.split_commas(a, b) {
                        let mut s = c.sub(x, y);
                        let (Ok(name), Ok(ty)) = (s.ident(), { self.data_type(&mut s) }) else { continue };
                        let ordinal = r.result_columns.len() as u32 + 1;
                        r.result_columns.push(Column { name, ordinal, data_type: ty, nullable: true, ..Default::default() });
                    }
                }
            } else if let Ok(t) = self.data_type(&mut c) {
                r.returns = Some(t.sql());
            }
        }
        r.null_on_null_input = definition.to_ascii_uppercase().contains("RETURNS NULL ON NULL INPUT");
    }

    fn create_aggregate(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        let parameters = self.parameters(c, &["RETURNS"])?;
        c.expect("RETURNS")?;
        let returns = self.data_type(c)?.sql();
        c.expect("EXTERNAL")?;
        c.expect("NAME")?;
        let method = quoted_parts(&c.multipart()?);
        self.db.routines.retain(|x| !(same(&x.schema, &name.schema) && same(&x.name, &name.name)));
        self.db.routines.push(Routine {
            schema: name.schema.clone(),
            name: name.name.clone(),
            kind: RoutineKind::ClrAggregate,
            clr_method: Some(method),
            parameters,
            returns: Some(returns),
            ..Default::default()
        });
        self.origin(ObjectType::Function, &name.schema, &name.name);
        Ok(())
    }

    fn create_xml_schema_collection(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.object(&self.schema_default())?;
        c.expect("AS")?;
        let definition = self.string(c)?;
        self.db.xml_schema_collections.retain(|x| !(same(&x.schema, &name.schema) && same(&x.name, &name.name)));
        self.db.xml_schema_collections.push(XmlSchemaCollection { schema: name.schema.clone(), name: name.name.clone(), owner: None, definition });
        self.origin(ObjectType::XmlSchemaCollection, &name.schema, &name.name);
        Ok(())
    }

    fn create_bindable(&mut self, c: &mut Cur, rule: bool) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        self.bindable_module(c.src, &name, rule)
    }

    /// CREATE DEFAULT / CREATE RULE, a batch of its own: the text as stored.
    pub(super) fn bindable_module(&mut self, definition: &str, name: &ObjectName, rule: bool) -> R<()> {
        let object = BindableObject { schema: name.schema.clone(), name: name.name.clone(), definition: definition.to_string() };
        let list = if rule { &mut self.db.rules } else { &mut self.db.defaults };
        list.retain(|x| !(same(&x.schema, &name.schema) && same(&x.name, &name.name)));
        list.push(object);
        self.origin(if rule { ObjectType::Rule } else { ObjectType::Default }, &name.schema, &name.name);
        Ok(())
    }

    fn create_partition_function(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        c.expect_punct("(")?;
        let data_type = self.data_type(c)?;
        c.expect_punct(")")?;
        c.expect("AS")?;
        c.expect("RANGE")?;
        let range_right = c.eat("RIGHT");
        c.eat("LEFT");
        c.expect("FOR")?;
        c.expect("VALUES")?;
        let (a, b) = c.group()?;
        let boundaries = c.split_commas(a, b).into_iter().map(|(x, y)| c.text(x, y - 1).trim().to_string()).collect();
        self.db.partition_functions.retain(|f| !same(&f.name, &name));
        self.db.partition_functions.push(PartitionFunction { name: name.clone(), data_type, range_right, boundaries });
        self.origin(ObjectType::PartitionFunction, "", &name);
        Ok(())
    }

    fn create_partition_scheme(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        c.expect("AS")?;
        c.expect("PARTITION")?;
        let function = c.ident()?;
        let all = c.eat("ALL");
        c.expect("TO")?;
        let (a, b) = c.group()?;
        let mut filegroups: Vec<String> = c.split_commas(a, b).into_iter().filter(|(x, y)| y > x).map(|(x, _)| unquote(&c.t[x])).collect();
        if all && let Some(fg) = filegroups.first().cloned() {
            let n = self.db.partition_functions.iter().find(|f| same(&f.name, &function)).map(|f| f.boundaries.len() + 2).unwrap_or(1);
            filegroups = vec![fg; n];
        }
        self.db.partition_schemes.retain(|f| !same(&f.name, &name));
        self.db.partition_schemes.push(PartitionScheme { name: name.clone(), function, filegroups });
        self.origin(ObjectType::PartitionScheme, "", &name);
        Ok(())
    }

    fn create_fulltext_catalog(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        let mut f = FullTextCatalog { name: name.clone(), ..Default::default() };
        while !c.done() {
            if c.eat_seq(&["WITH", "ACCENT_SENSITIVITY"]) {
                c.expect_punct("=")?;
                f.accent_sensitive = c.on_off()?;
            } else if c.eat_seq(&["AS", "DEFAULT"]) {
                f.is_default = true;
            } else if c.eat("AUTHORIZATION") {
                f.owner = Some(c.ident()?);
            } else {
                c.i += 1;
            }
        }
        self.db.fulltext_catalogs.retain(|x| !same(&x.name, &name));
        self.db.fulltext_catalogs.push(f);
        self.origin(ObjectType::FullTextCatalog, "", &name);
        Ok(())
    }

    fn create_stoplist(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        let owner = if c.eat("AUTHORIZATION") { Some(c.ident()?) } else { None };
        self.db.fulltext_stoplists.retain(|x| !same(&x.name, &name));
        self.db.fulltext_stoplists.push(FullTextStoplist { name: name.clone(), owner, words: Vec::new() });
        self.origin(ObjectType::FullTextStoplist, "", &name);
        Ok(())
    }

    fn alter_stoplist(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        let add = c.eat("ADD");
        if !add {
            c.expect("DROP")?;
        }
        let word = self.string(c)?;
        c.expect("LANGUAGE")?;
        let language = c.number()?.parse().unwrap_or(0);
        let Some(list) = self.db.fulltext_stoplists.iter_mut().find(|l| same(&l.name, &name)) else { return c.fail(MessageKind::ReferenceNotFound) };
        let w = Stopword { word, language };
        list.words.retain(|x| *x != w);
        if add {
            list.words.push(w);
            list.words.sort();
        }
        Ok(())
    }

    fn create_property_list(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.ident()?;
        self.db.search_property_lists.retain(|x| !same(&x.name, &name));
        self.db.search_property_lists.push(SearchPropertyList { name: name.clone(), owner: None, properties: Vec::new() });
        self.origin(ObjectType::SearchPropertyList, "", &name);
        Ok(())
    }

    fn alter_property_list(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.ident()?;
        c.expect("ADD")?;
        let property = self.string(c)?;
        let mut p = SearchProperty { name: property, ..Default::default() };
        c.expect("WITH")?;
        let (a, b) = c.group()?;
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let key = s.ident()?.to_ascii_uppercase();
            s.expect_punct("=")?;
            match key.as_str() {
                "PROPERTY_SET_GUID" => p.set_guid = self.string(&mut s)?.to_uppercase(),
                "PROPERTY_INT_ID" => p.int_id = s.number()?.parse().unwrap_or(0),
                "PROPERTY_DESCRIPTION" => p.description = Some(self.string(&mut s)?),
                _ => {}
            }
        }
        let Some(list) = self.db.search_property_lists.iter_mut().find(|l| same(&l.name, &name)) else { return c.fail(MessageKind::ReferenceNotFound) };
        list.properties.push(p);
        list.properties.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    fn create_fulltext_index(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let table = c.object(&self.schema_default())?;
        let Some(ti) = self.table_index(&table) else { return c.fail(MessageKind::ReferenceNotFound) };
        let mut f = FullTextIndex { change_tracking: "AUTO".into(), stoplist: "SYSTEM".into(), ..Default::default() };
        c.expect("KEY")?;
        c.expect("INDEX")?;
        f.key_index = c.ident()?;
        c.expect("ON")?;
        if c.is_punct("(") {
            let (a, b) = c.group()?;
            let mut s = c.sub(a, b);
            f.catalog = s.ident()?;
            if s.eat_punct(",") && s.eat("FILEGROUP") {
                f.filegroup = Some(s.ident()?);
            }
        } else {
            f.catalog = c.ident()?;
        }
        if c.eat("WITH") {
            while !c.done() {
                if c.eat("CHANGE_TRACKING") {
                    c.eat_punct("=");
                    f.change_tracking = c.ident()?.to_ascii_uppercase();
                } else if c.eat("STOPLIST") {
                    c.eat_punct("=");
                    f.stoplist = c.ident()?;
                    if f.stoplist.eq_ignore_ascii_case("OFF") || f.stoplist.eq_ignore_ascii_case("SYSTEM") {
                        f.stoplist = f.stoplist.to_ascii_uppercase();
                    }
                } else if c.eat_seq(&["SEARCH", "PROPERTY", "LIST"]) {
                    c.eat_punct("=");
                    f.property_list = Some(c.ident()?);
                } else {
                    c.i += 1;
                }
            }
        }
        self.db.tables[ti].fulltext_index = Some(f);
        Ok(())
    }

    fn alter_fulltext_index(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let table = c.object(&self.schema_default())?;
        let Some(ti) = self.table_index(&table) else { return c.fail(MessageKind::ReferenceNotFound) };
        if !c.eat("ADD") {
            return Ok(());
        }
        let (a, b) = c.group()?;
        let mut columns = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let mut col = FullTextColumn { name: s.ident()?, ..Default::default() };
            while !s.done() {
                if s.eat_seq(&["TYPE", "COLUMN"]) {
                    col.type_column = Some(s.ident()?);
                } else if s.eat("LANGUAGE") {
                    col.language = s.number()?.parse().unwrap_or(0);
                } else {
                    s.i += 1;
                }
            }
            columns.push(col);
        }
        let Some(f) = self.db.tables[ti].fulltext_index.as_mut() else { return c.fail(MessageKind::ReferenceNotFound) };
        f.columns.extend(columns);
        f.columns.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    fn create_statistics(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        c.expect("ON")?;
        let table = c.object(&self.schema_default())?;
        let Some(ti) = self.table_index(&table) else { return c.fail(MessageKind::ReferenceNotFound) };
        let mut st = Statistic { name: name.clone(), columns: self.name_list(c)?, ..Default::default() };
        if c.eat("WHERE") {
            let start = c.i;
            while !c.done() && !c.at("WITH") {
                c.i += 1;
            }
            st.filter = Some(paren(c.text(start, c.i - 1).trim()));
        }
        if c.eat("WITH") {
            while !c.done() {
                if c.eat("NORECOMPUTE") {
                    st.no_recompute = true;
                } else if c.eat("INCREMENTAL") {
                    c.expect_punct("=")?;
                    st.incremental = c.on_off()?;
                } else {
                    c.i += 1;
                }
            }
        }
        let list = &mut self.db.tables[ti].statistics;
        list.retain(|s| !same(&s.name, &name));
        list.push(st);
        Ok(())
    }

    fn add_sensitivity(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        c.expect("TO")?;
        let parts = c.multipart()?;
        let (table, column) = match parts.as_slice() {
            [s, t, col] => (ObjectName::new(s, t), col.clone()),
            [t, col] => (ObjectName::new(&self.schema_default(), t), col.clone()),
            _ => return c.syntax(),
        };
        let Some(ti) = self.table_index(&table) else { return c.fail(MessageKind::ReferenceNotFound) };
        let mut x = Sensitivity::default();
        c.expect("WITH")?;
        let (a, b) = c.group()?;
        for (p, q) in c.split_commas(a, b) {
            let mut s = c.sub(p, q);
            let key = s.ident()?.to_ascii_uppercase();
            s.expect_punct("=")?;
            let value = if s.peek().is_some_and(|t| t.kind == Tok::Str) { self.string(&mut s)? } else { s.ident()? };
            match key.as_str() {
                "LABEL" => x.label = Some(value),
                "LABEL_ID" => x.label_id = Some(value),
                "INFORMATION_TYPE" => x.information_type = Some(value),
                "INFORMATION_TYPE_ID" => x.information_type_id = Some(value),
                "RANK" => x.rank = Some(value.to_ascii_uppercase()),
                _ => {}
            }
        }
        if let Some(col) = self.db.tables[ti].columns.iter_mut().find(|k| same(&k.name, &column)) {
            col.sensitivity = Some(x);
        }
        Ok(())
    }

    // ---- Service Broker ----

    fn create_message_type(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        let mut m = MessageType { name: name.clone(), validation: "NONE".into(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            m.owner = Some(c.ident()?);
        }
        if c.eat("VALIDATION") {
            c.expect_punct("=")?;
            m.validation = c.ident()?.to_ascii_uppercase();
            if c.eat_seq(&["WITH", "SCHEMA", "COLLECTION"]) {
                m.xml_collection = Some(c.object(&self.schema_default())?.quoted());
            }
        }
        self.db.message_types.retain(|x| !same(&x.name, &name));
        self.db.message_types.push(m);
        self.origin(ObjectType::MessageType, "", &name);
        Ok(())
    }

    fn create_contract(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let mut k = Contract { name: name.clone(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            k.owner = Some(c.ident()?);
        }
        let (a, b) = c.group()?;
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let message_type = s.ident()?;
            s.expect("SENT")?;
            s.expect("BY")?;
            k.messages.push(ContractMessage { message_type, sent_by: s.ident()?.to_ascii_uppercase() });
        }
        k.messages.sort_by(|a, b| a.message_type.cmp(&b.message_type));
        self.db.contracts.retain(|x| !same(&x.name, &name));
        self.db.contracts.push(k);
        self.origin(ObjectType::Contract, "", &name);
        Ok(())
    }

    fn create_queue(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.object(&self.schema_default())?;
        let mut q = Queue { schema: name.schema.clone(), name: name.name.clone(), ..Default::default() };
        while !c.done() {
            if c.eat("STATUS") {
                c.expect_punct("=")?;
                q.enabled = c.on_off()?;
            } else if c.eat("RETENTION") {
                c.expect_punct("=")?;
                q.retention = c.on_off()?;
            } else if c.eat("POISON_MESSAGE_HANDLING") {
                let (a, b) = c.group()?;
                let mut s = c.sub(a, b);
                s.expect("STATUS")?;
                s.expect_punct("=")?;
                q.poison_message_handling = s.on_off()?;
            } else if c.eat("ACTIVATION") {
                let (a, b) = c.group()?;
                let mut act = QueueActivation { enabled: true, ..Default::default() };
                for (x, y) in c.split_commas(a, b) {
                    let mut s = c.sub(x, y);
                    if s.eat("STATUS") {
                        s.expect_punct("=")?;
                        act.enabled = s.on_off()?;
                    } else if s.eat("PROCEDURE_NAME") {
                        s.expect_punct("=")?;
                        act.procedure = s.object(&self.schema_default())?.quoted();
                    } else if s.eat("MAX_QUEUE_READERS") {
                        s.expect_punct("=")?;
                        act.max_readers = s.number()?.parse().unwrap_or(0);
                    } else if s.eat_seq(&["EXECUTE", "AS"]) {
                        let who = s.ident_or_string()?;
                        act.execute_as = match who.to_ascii_uppercase().as_str() {
                            "CALLER" => None,
                            "OWNER" | "SELF" => Some(who.to_ascii_uppercase()),
                            _ => Some(who),
                        };
                    }
                }
                q.activation = Some(act);
            } else if c.eat("ON") {
                q.filegroup = Some(c.ident()?);
            } else {
                c.i += 1;
            }
        }
        self.db.queues.retain(|x| !(same(&x.schema, &q.schema) && same(&x.name, &q.name)));
        self.db.queues.push(q);
        self.origin(ObjectType::Queue, &name.schema, &name.name);
        Ok(())
    }

    fn create_service(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let mut s = Service { name: name.clone(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            s.owner = Some(c.ident()?);
        }
        c.expect("ON")?;
        c.expect("QUEUE")?;
        s.queue = c.object(&self.schema_default())?;
        if c.is_punct("(") {
            s.contracts = self.name_list(c)?;
            s.contracts.sort();
        }
        self.db.services.retain(|x| !same(&x.name, &name));
        self.db.services.push(s);
        self.origin(ObjectType::Service, "", &name);
        Ok(())
    }

    fn create_route(&mut self, c: &mut Cur) -> R<()> {
        c.i = 2;
        let name = c.ident()?;
        let mut r = Route { name: name.clone(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            r.owner = Some(c.ident()?);
        }
        c.expect("WITH")?;
        while !c.done() {
            let key = c.ident()?.to_ascii_uppercase();
            c.expect_punct("=")?;
            match key.as_str() {
                "ADDRESS" => r.address = self.string(c)?,
                "SERVICE_NAME" => r.remote_service = Some(self.string(c)?),
                "BROKER_INSTANCE" => r.broker_instance = Some(self.string(c)?),
                "MIRROR_ADDRESS" => r.mirror_address = Some(self.string(c)?),
                _ => {
                    c.i += 1;
                }
            }
            c.eat_punct(",");
        }
        self.db.routes.retain(|x| !same(&x.name, &name));
        self.db.routes.push(r);
        self.origin(ObjectType::Route, "", &name);
        Ok(())
    }

    fn create_service_binding(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.ident()?;
        let mut b = ServiceBinding { name: name.clone(), ..Default::default() };
        if c.eat("AUTHORIZATION") {
            b.owner = Some(c.ident()?);
        }
        c.expect("TO")?;
        c.expect("SERVICE")?;
        b.remote_service = self.string(c)?;
        c.expect("WITH")?;
        while !c.done() {
            if c.eat("USER") {
                c.expect_punct("=")?;
                b.user = c.ident()?;
            } else if c.eat("ANONYMOUS") {
                c.expect_punct("=")?;
                b.anonymous = c.on_off()?;
            } else {
                c.i += 1;
            }
        }
        self.db.service_bindings.retain(|x| !same(&x.name, &name));
        self.db.service_bindings.push(b);
        self.origin(ObjectType::ServiceBinding, "", &name);
        Ok(())
    }

    fn create_event_notification(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        c.expect("ON")?;
        let queue = if c.eat("QUEUE") { Some(c.object(&self.schema_default())?) } else { c.expect("DATABASE").map(|_| None)? };
        let mut e = EventNotification { name: name.clone(), queue: queue.clone(), ..Default::default() };
        if c.eat_seq(&["WITH", "FAN_IN"]) {
            e.fan_in = true;
        }
        c.expect("FOR")?;
        while !c.done() && !c.at("TO") {
            if let Some(t) = c.peek()
                && t.kind == Tok::Word
            {
                e.events.push(t.text.to_ascii_uppercase());
            }
            c.i += 1;
        }
        e.events.sort();
        c.expect("TO")?;
        c.expect("SERVICE")?;
        e.service = self.string(c)?;
        if c.eat_punct(",") {
            e.broker_instance = self.string(c)?;
        }
        match queue {
            Some(q) => {
                let Some(qu) = self.db.queues.iter_mut().find(|x| same(&x.schema, &q.schema) && same(&x.name, &q.name)) else { return c.fail(MessageKind::ReferenceNotFound) };
                qu.event_notifications.retain(|x| !same(&x.name, &name));
                qu.event_notifications.push(e);
            }
            None => {
                self.db.event_notifications.retain(|x| !same(&x.name, &name));
                self.db.event_notifications.push(e);
                self.origin(ObjectType::EventNotification, "", &name);
            }
        }
        Ok(())
    }

    fn create_security_policy(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.object(&self.schema_default())?;
        let mut p = SecurityPolicy { schema: name.schema.clone(), name: name.name.clone(), enabled: true, schema_bound: true, ..Default::default() };
        while c.eat("ADD") || c.eat_punct(",") {
            if c.at("FILTER") || c.at("BLOCK") {
                let kind = c.ident()?.to_ascii_uppercase();
                c.expect("PREDICATE")?;
                let function = quoted_parts(&c.multipart()?);
                let open = c.i;
                c.group()?;
                let arguments = c.text(open, c.i - 1).to_string();
                c.expect("ON")?;
                let table = c.object(&self.schema_default())?;
                let operation = ["AFTER", "BEFORE"].iter().find(|w| c.at(w)).map(|_| {
                    let a = c.ident().unwrap_or_default().to_ascii_uppercase();
                    let b = c.ident().unwrap_or_default().to_ascii_uppercase();
                    format!("{a} {b}")
                });
                p.predicates.push(SecurityPredicate { kind, function, arguments, table, operation });
            }
        }
        if c.eat("WITH") {
            let (a, b) = c.group()?;
            for (x, y) in c.split_commas(a, b) {
                let mut s = c.sub(x, y);
                if s.eat("STATE") {
                    s.expect_punct("=")?;
                    p.enabled = s.on_off()?;
                } else if s.eat("SCHEMABINDING") {
                    s.expect_punct("=")?;
                    p.schema_bound = s.on_off()?;
                }
            }
        }
        if c.eat_seq(&["NOT", "FOR", "REPLICATION"]) {
            p.not_for_replication = true;
        }
        self.db.security_policies.retain(|x| !(same(&x.schema, &p.schema) && same(&x.name, &p.name)));
        self.db.security_policies.push(p);
        self.origin(ObjectType::SecurityPolicy, &name.schema, &name.name);
        Ok(())
    }

    // ---- PolyBase ----

    /// `KEY = value` items of a WITH ( … ) list, values unquoted.
    fn with_items(&self, c: &mut Cur) -> R<Vec<(String, String)>> {
        c.expect("WITH")?;
        let (a, b) = c.group()?;
        let mut out = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let key = s.ident()?.to_ascii_uppercase();
            if !s.eat_punct("=") {
                out.push((key, c.text(x + 1, y - 1).to_string()));
                continue;
            }
            let value = match s.peek() {
                Some(t) if t.kind == Tok::Str => self.string(&mut s)?,
                _ => c.text(x + 2, y - 1).trim().trim_matches(['[', ']']).to_string(),
            };
            out.push((key, value));
        }
        Ok(out)
    }

    fn create_external_data_source(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.ident()?;
        let mut d = ExternalDataSource { name: name.clone(), ..Default::default() };
        for (k, v) in self.with_items(c)? {
            match k.as_str() {
                "LOCATION" => d.location = v,
                "TYPE" => d.kind = Some(v.to_ascii_uppercase()),
                "RESOURCE_MANAGER_LOCATION" => d.resource_manager_location = Some(v),
                "CREDENTIAL" => d.credential = Some(v),
                "DATABASE_NAME" => d.database_name = Some(v),
                "SHARD_MAP_NAME" => d.shard_map_name = Some(v),
                "CONNECTION_OPTIONS" => d.connection_options = Some(v),
                "PUSHDOWN" => d.pushdown = Some(v.eq_ignore_ascii_case("ON")),
                _ => {}
            }
        }
        self.db.external_data_sources.retain(|x| !same(&x.name, &name));
        self.db.external_data_sources.push(d);
        self.origin(ObjectType::ExternalDataSource, "", &name);
        Ok(())
    }

    fn create_external_file_format(&mut self, c: &mut Cur) -> R<()> {
        c.i = 4;
        let name = c.ident()?;
        let mut f = ExternalFileFormat { name: name.clone(), ..Default::default() };
        let items = self.with_items(c)?;
        for (k, v) in items {
            match k.as_str() {
                "FORMAT_TYPE" => f.format_type = v.to_ascii_uppercase(),
                "DATA_COMPRESSION" => f.data_compression = Some(v),
                "SERDE_METHOD" => f.serde_method = Some(v),
                "FORMAT_OPTIONS" => {
                    let mut s = Cur::new(&v);
                    let Ok((a, b)) = s.group() else { continue };
                    for (x, y) in s.split_commas(a, b) {
                        let mut o = s.sub(x, y);
                        let Ok(key) = o.ident() else { continue };
                        o.eat_punct("=");
                        let value = match o.peek() {
                            Some(t) if t.kind == Tok::Str => string_value(&t.text),
                            Some(t) => t.text.clone(),
                            None => String::new(),
                        };
                        match key.to_ascii_uppercase().as_str() {
                            "FIELD_TERMINATOR" => f.field_terminator = Some(value),
                            "STRING_DELIMITER" => f.string_delimiter = Some(value),
                            "FIRST_ROW" => f.first_row = value.parse().ok(),
                            "DATE_FORMAT" => f.date_format = Some(value),
                            "USE_TYPE_DEFAULT" => f.use_type_default = value.eq_ignore_ascii_case("TRUE"),
                            "ENCODING" => f.encoding = Some(value),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        self.db.external_file_formats.retain(|x| !same(&x.name, &name));
        self.db.external_file_formats.push(f);
        self.origin(ObjectType::ExternalFileFormat, "", &name);
        Ok(())
    }

    fn create_external_table(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.object(&self.schema_default())?;
        let (a, b) = c.group()?;
        let mut columns = Vec::new();
        for (x, y) in c.split_commas(a, b) {
            let mut s = c.sub(x, y);
            let cname = s.ident()?;
            let data_type = self.data_type(&mut s)?;
            let mut col = Column { name: cname, ordinal: columns.len() as u32 + 1, data_type, nullable: true, ..Default::default() };
            while !s.done() {
                if s.eat("COLLATE") {
                    col.collation = Some(s.ident()?);
                } else if s.eat_seq(&["NOT", "NULL"]) {
                    col.nullable = false;
                } else {
                    s.i += 1;
                }
            }
            columns.push(col);
        }
        let mut t = ExternalTable { schema: name.schema.clone(), name: name.name.clone(), columns, ..Default::default() };
        for (k, v) in self.with_items(c)? {
            match k.as_str() {
                "LOCATION" => t.location = Some(v),
                "DATA_SOURCE" => t.data_source = v,
                "FILE_FORMAT" => t.file_format = Some(v),
                "REJECT_TYPE" => t.reject_type = Some(v.to_ascii_uppercase()),
                "REJECT_VALUE" => t.reject_value = Some(v),
                "REJECT_SAMPLE_VALUE" => t.reject_sample_value = Some(v),
                "SCHEMA_NAME" => t.remote_schema = Some(v),
                "OBJECT_NAME" => t.remote_object = Some(v),
                "DISTRIBUTION" => t.distribution = Some(v),
                _ => {}
            }
        }
        self.db.external_tables.retain(|x| !(same(&x.schema, &t.schema) && same(&x.name, &t.name)));
        self.db.external_tables.push(t);
        self.origin(ObjectType::ExternalTable, &name.schema, &name.name);
        Ok(())
    }

    fn create_application_role(&mut self, c: &mut Cur) -> R<()> {
        c.i = 3;
        let name = c.ident()?;
        let mut default_schema = None;
        while !c.done() {
            if c.eat("DEFAULT_SCHEMA") {
                c.expect_punct("=")?;
                default_schema = Some(c.ident()?);
            } else {
                c.i += 1;
            }
        }
        self.db.principals.retain(|p| !same(&p.name, &name));
        self.db.principals.push(Principal { name: name.clone(), kind: "APPLICATION_ROLE".into(), default_schema, ..Default::default() });
        self.origin(ObjectType::Role, "", &name);
        Ok(())
    }
}
