//! Scripts-folder files of the objects of `catalog::objects`, and how deploying to a folder
//! takes them over from the source.

use super::parse::{FileKind, ObjectKey};
use super::script;
use crate::catalog::*;
use crate::schema::ddl_objects as d;
use crate::schema::{ObjectType, Opt, Options};

fn xprop_class(t: ObjectType) -> Option<&'static str> {
    Some(match t {
        ObjectType::Assembly => "ASSEMBLY",
        ObjectType::XmlSchemaCollection => "XML SCHEMA COLLECTION",
        ObjectType::Default => "DEFAULT",
        ObjectType::Rule => "RULE",
        ObjectType::PartitionFunction => "PARTITION FUNCTION",
        ObjectType::PartitionScheme => "PARTITION SCHEME",
        ObjectType::MessageType => "MESSAGE TYPE",
        ObjectType::Contract => "CONTRACT",
        ObjectType::Queue => "QUEUE",
        ObjectType::Service => "SERVICE",
        ObjectType::Route => "ROUTE",
        ObjectType::ServiceBinding => "REMOTE SERVICE BINDING",
        _ => return None,
    })
}

/// GRANT statements on a securable that has a class prefix (`SERVICE::`) or is an object.
fn grants(db: &Database, class: &str, schema: Option<&str>, name: &str, o: &Options) -> Vec<String> {
    if o.has(Opt::IgnorePermissions) {
        return Vec::new();
    }
    db.permissions.iter().filter(|p| p.class == class && p.schema.as_deref() == schema && p.object.as_deref() == Some(name)).map(Permission::grant_sql).collect()
}

/// A DDL trigger's file: its text, then its extended properties.
pub fn ddl_trigger_file(db: &Database, t: &DdlTrigger, o: &Options) -> String {
    let mut text = script::ddl_trigger(t);
    if !o.has(Opt::IgnoreExtendedProperties) {
        let props: Vec<String> = db.extended_properties.iter().filter(|p| p.class == "TRIGGER" && p.schema.is_none() && p.object.as_deref() == Some(t.name.as_str())).map(script::xprop_sql).collect();
        text.push_str(&script::statements(&props));
    }
    text
}

/// Every object of these kinds with its file content.
pub fn object_files(db: &Database, o: &Options) -> Vec<(ObjectKey, String)> {
    let key = |ty, s: &str, n: &str| (FileKind::Object(ty), s.to_string(), n.to_string());
    let mut out: Vec<(ObjectKey, String)> = Vec::new();
    let mut add = |k: ObjectKey, mut v: Vec<String>, g: Vec<String>| {
        v.extend(g);
        // Extended properties of the object.
        let (FileKind::Object(ty), schema, name) = &k else { return };
        if let Some(class) = xprop_class(*ty)
            && !o.has(Opt::IgnoreExtendedProperties)
        {
            let scoped = ty.schema_scoped();
            v.extend(
                db.extended_properties
                    .iter()
                    .filter(|p| p.class == class && p.object.as_deref() == Some(name.as_str()) && (if scoped { p.schema.as_deref() == Some(schema.as_str()) } else { p.schema.is_none() }))
                    .map(script::xprop_sql),
            );
        }
        out.push((k, script::statements(&v)));
    };
    for a in &db.assemblies {
        add(key(ObjectType::Assembly, "", &a.name), d::create_assembly(a), grants(db, "ASSEMBLY", None, &a.name, o));
    }
    for x in &db.xml_schema_collections {
        add(key(ObjectType::XmlSchemaCollection, &x.schema, &x.name), vec![d::create_xml_schema_collection(x)], grants(db, "XML SCHEMA COLLECTION", Some(&x.schema), &x.name, o));
    }
    for x in &db.defaults {
        add(key(ObjectType::Default, &x.schema, &x.name), vec![d::create_bindable(x)], Vec::new());
    }
    for x in &db.rules {
        add(key(ObjectType::Rule, &x.schema, &x.name), vec![d::create_bindable(x)], Vec::new());
    }
    for f in &db.partition_functions {
        add(key(ObjectType::PartitionFunction, "", &f.name), vec![d::create_partition_function(f)], Vec::new());
    }
    for s in &db.partition_schemes {
        add(key(ObjectType::PartitionScheme, "", &s.name), vec![d::create_partition_scheme(s)], Vec::new());
    }
    for c in &db.fulltext_catalogs {
        add(key(ObjectType::FullTextCatalog, "", &c.name), vec![d::create_fulltext_catalog(c)], grants(db, "FULLTEXT CATALOG", None, &c.name, o));
    }
    for l in &db.fulltext_stoplists {
        add(key(ObjectType::FullTextStoplist, "", &l.name), d::create_stoplist(l), grants(db, "FULLTEXT STOPLIST", None, &l.name, o));
    }
    for l in &db.search_property_lists {
        add(key(ObjectType::SearchPropertyList, "", &l.name), d::create_property_list(l), Vec::new());
    }
    for m in &db.message_types {
        add(key(ObjectType::MessageType, "", &m.name), vec![d::create_message_type(m)], grants(db, "MESSAGE TYPE", None, &m.name, o));
    }
    for c in &db.contracts {
        add(key(ObjectType::Contract, "", &c.name), vec![d::create_contract(c)], grants(db, "CONTRACT", None, &c.name, o));
    }
    for q in &db.queues {
        let mut v = vec![d::create_queue(q, true, !o.has(Opt::IgnoreFileGroups))];
        v.extend(q.event_notifications.iter().map(d::create_event_notification));
        add(key(ObjectType::Queue, &q.schema, &q.name), v, grants(db, "OBJECT", Some(&q.schema), &q.name, o));
    }
    for s in &db.services {
        add(key(ObjectType::Service, "", &s.name), vec![d::create_service(s)], grants(db, "SERVICE", None, &s.name, o));
    }
    for r in &db.routes {
        add(key(ObjectType::Route, "", &r.name), vec![d::create_route(r)], grants(db, "ROUTE", None, &r.name, o));
    }
    for b in &db.service_bindings {
        add(key(ObjectType::ServiceBinding, "", &b.name), vec![d::create_service_binding(b)], grants(db, "REMOTE SERVICE BINDING", None, &b.name, o));
    }
    for e in &db.event_notifications {
        add(key(ObjectType::EventNotification, "", &e.name), vec![d::create_event_notification(e)], Vec::new());
    }
    if !o.has(Opt::IgnoreCertificatesAndCryptoKeys) {
        for c in &db.certificates {
            add(key(ObjectType::Certificate, "", &c.name), vec![d::describe_certificate(c)], Vec::new());
        }
        for k in &db.asymmetric_keys {
            add(key(ObjectType::AsymmetricKey, "", &k.name), vec![d::describe_asymmetric_key(k)], Vec::new());
        }
        for k in &db.symmetric_keys {
            add(key(ObjectType::SymmetricKey, "", &k.name), vec![d::describe_symmetric_key(k)], Vec::new());
        }
    }
    for p in &db.security_policies {
        add(key(ObjectType::SecurityPolicy, &p.schema, &p.name), vec![d::create_security_policy(p)], grants(db, "OBJECT", Some(&p.schema), &p.name, o));
    }
    for x in &db.external_data_sources {
        add(key(ObjectType::ExternalDataSource, "", &x.name), vec![d::create_external_data_source(x)], Vec::new());
    }
    for x in &db.external_file_formats {
        add(key(ObjectType::ExternalFileFormat, "", &x.name), vec![d::create_external_file_format(x)], Vec::new());
    }
    for x in &db.external_tables {
        add(key(ObjectType::ExternalTable, &x.schema, &x.name), vec![d::create_external_table(x)], grants(db, "OBJECT", Some(&x.schema), &x.name, o));
    }
    out
}

/// Takes one object of these kinds over from `source` (or removes it): `n1` is its source
/// name, `n2` its name in the folder.
pub fn apply(db: &mut Database, source: &Database, ty: ObjectType, n1: &ObjectName, n2: &ObjectName, from_source: bool) {
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    macro_rules! scoped {
        ($field:ident) => {{
            db.$field.retain(|x| !(eq(&x.schema, &n2.schema) && eq(&x.name, &n2.name)));
            if from_source && let Some(x) = source.$field.iter().find(|x| eq(&x.schema, &n1.schema) && eq(&x.name, &n1.name)) {
                let mut x = x.clone();
                x.schema = n2.schema.clone();
                x.name = n2.name.clone();
                db.$field.push(x);
            }
        }};
    }
    macro_rules! named {
        ($field:ident) => {{
            db.$field.retain(|x| !eq(&x.name, &n2.name));
            if from_source && let Some(x) = source.$field.iter().find(|x| eq(&x.name, &n1.name)) {
                let mut x = x.clone();
                x.name = n2.name.clone();
                db.$field.push(x);
            }
        }};
    }
    match ty {
        ObjectType::Assembly => named!(assemblies),
        ObjectType::XmlSchemaCollection => scoped!(xml_schema_collections),
        ObjectType::Default => scoped!(defaults),
        ObjectType::Rule => scoped!(rules),
        ObjectType::DdlTrigger => named!(ddl_triggers),
        ObjectType::PartitionFunction => named!(partition_functions),
        ObjectType::PartitionScheme => named!(partition_schemes),
        ObjectType::FullTextCatalog => named!(fulltext_catalogs),
        ObjectType::FullTextStoplist => named!(fulltext_stoplists),
        ObjectType::SearchPropertyList => named!(search_property_lists),
        ObjectType::MessageType => named!(message_types),
        ObjectType::Contract => named!(contracts),
        ObjectType::Queue => scoped!(queues),
        ObjectType::Service => named!(services),
        ObjectType::Route => named!(routes),
        ObjectType::ServiceBinding => named!(service_bindings),
        ObjectType::EventNotification => named!(event_notifications),
        ObjectType::Certificate => named!(certificates),
        ObjectType::AsymmetricKey => named!(asymmetric_keys),
        ObjectType::SymmetricKey => named!(symmetric_keys),
        ObjectType::SecurityPolicy => scoped!(security_policies),
        ObjectType::ExternalDataSource => named!(external_data_sources),
        ObjectType::ExternalFileFormat => named!(external_file_formats),
        ObjectType::ExternalTable => scoped!(external_tables),
        _ => return,
    }
    // Extended properties and grants on the object follow it.
    if let Some(class) = xprop_class(ty).or((ty == ObjectType::DdlTrigger).then_some("TRIGGER")) {
        let scoped = ty.schema_scoped();
        let on = |p: &ExtendedProperty, n: &ObjectName| {
            p.class == class && p.object.as_deref().is_some_and(|o| eq(o, &n.name)) && (if scoped { p.schema.as_deref().is_some_and(|s| eq(s, &n.schema)) } else { p.schema.is_none() })
        };
        db.extended_properties.retain(|p| !on(p, n2));
        if from_source {
            for p in source.extended_properties.iter().filter(|p| on(p, n1)) {
                let mut p = p.clone();
                p.object = Some(n2.name.clone());
                if scoped {
                    p.schema = Some(n2.schema.clone());
                }
                db.extended_properties.push(p);
            }
        }
    }
    let class = match ty {
        ObjectType::Queue | ObjectType::SecurityPolicy | ObjectType::ExternalTable => "OBJECT",
        ObjectType::XmlSchemaCollection => "XML SCHEMA COLLECTION",
        ObjectType::Assembly => "ASSEMBLY",
        ObjectType::MessageType => "MESSAGE TYPE",
        ObjectType::Contract => "CONTRACT",
        ObjectType::Service => "SERVICE",
        ObjectType::Route => "ROUTE",
        ObjectType::ServiceBinding => "REMOTE SERVICE BINDING",
        ObjectType::FullTextCatalog => "FULLTEXT CATALOG",
        ObjectType::FullTextStoplist => "FULLTEXT STOPLIST",
        ObjectType::Certificate => "CERTIFICATE",
        ObjectType::AsymmetricKey => "ASYMMETRIC KEY",
        ObjectType::SymmetricKey => "SYMMETRIC KEY",
        _ => return,
    };
    let scoped = ty.schema_scoped();
    let on = |p: &Permission, n: &ObjectName| {
        p.class == class && p.object.as_deref().is_some_and(|o| eq(o, &n.name)) && (!scoped || p.schema.as_deref().is_some_and(|s| eq(s, &n.schema)))
    };
    db.permissions.retain(|p| !on(p, n2));
    if from_source {
        for p in source.permissions.iter().filter(|p| on(p, n1)) {
            let mut p = p.clone();
            p.object = Some(n2.name.clone());
            if scoped {
                p.schema = Some(n2.schema.clone());
            }
            db.permissions.push(p);
        }
    }
}
