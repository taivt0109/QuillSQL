//! Tests of what is compared on each object (specs\gaps\fix-schema-model.md).

use super::*;

fn opts() -> Options {
    Options::default_set()
}

fn run(a: &Database, b: &Database, o: &Options) -> Comparison {
    compare(a, b, o, &Mappings::default())
}

fn find<'c>(c: &'c Comparison, ty: ObjectType, name: &str) -> Option<&'c Difference> {
    c.differences.iter().find(|d| d.object_type == ty && d.name.name == name)
}

fn props(d: &Difference) -> Vec<&str> {
    d.properties.iter().map(|p| p.name.as_str()).collect()
}

fn schema(name: &str) -> Schema {
    Schema { name: name.into(), owner: "dbo".into() }
}

fn table(name: &str) -> Table {
    Table { schema: "dbo".into(), name: name.into(), lock_escalation: "TABLE".into(), ..Default::default() }
}

fn user(name: &str, login: Option<&str>) -> Principal {
    Principal { name: name.into(), kind: "WINDOWS_USER".into(), login: login.map(Into::into), ..Default::default() }
}

fn role(name: &str) -> Principal {
    Principal { name: name.into(), kind: "DATABASE_ROLE".into(), owner: Some("dbo".into()), ..Default::default() }
}

fn grant(class: &str, schema: Option<&str>, object: Option<&str>, grantee: &str, what: &str) -> Permission {
    Permission {
        class: class.into(),
        schema: schema.map(Into::into),
        object: object.map(Into::into),
        grantee: grantee.into(),
        permission: what.into(),
        state: "GRANT".into(),
        ..Default::default()
    }
}

#[test]
fn dbo_schema_is_compared_but_never_one_sided() {
    let base = Database { schemas: vec![schema("dbo")], principals: vec![role("app")], ..Default::default() };
    let mut granted = base.clone();
    granted.permissions.push(grant("SCHEMA", Some("dbo"), None, "app", "EXECUTE"));
    let c = run(&granted, &base, &opts());
    let d = find(&c, ObjectType::Schema, "dbo").expect("dbo reported");
    assert_eq!((d.kind, props(d)), (DiffKind::Different, vec!["Permission"]));
    let mut described = base.clone();
    described.extended_properties.push(ExtendedProperty {
        class: "SCHEMA".into(),
        schema: Some("dbo".into()),
        name: "MS_Description".into(),
        value: Some("x".into()),
        ..Default::default()
    });
    assert_eq!(find(&run(&base, &described, &opts()), ObjectType::Schema, "dbo").unwrap().kind, DiffKind::Different);
    // Identical, or missing on one side (an empty scripts folder): not listed.
    assert!(find(&run(&base, &base, &opts()), ObjectType::Schema, "dbo").is_none());
    assert!(find(&run(&base, &Database::default(), &opts()), ObjectType::Schema, "dbo").is_none());
}

#[test]
fn windows_users_pair_across_domains() {
    let side = |d: &str| Database {
        principals: vec![role("Readers"), Principal { member_of: vec!["Readers".into()], ..user(&format!("{d}\\svc"), Some(&format!("{d}\\svc"))) }],
        permissions: vec![grant("OBJECT", Some("dbo"), Some("T"), &format!("{d}\\svc"), "SELECT")],
        tables: vec![table("T")],
        ..Default::default()
    };
    let c = run(&side("DEV"), &side("PROD"), &opts().without(Opt::IgnoreUserProperties));
    let u = find(&c, ObjectType::User, "DEV\\svc").unwrap();
    assert_eq!(u.kind, DiffKind::Equal, "{:?}", u.properties);
    assert_eq!(u.target_name.as_ref().unwrap().name, "PROD\\svc");
    assert!(c.differences.iter().all(|d| d.kind == DiffKind::Equal), "{:?}", c.differences);
    // A login that differs beyond the domain keeps them apart.
    let mut other = side("PROD");
    other.principals[1].login = Some("PROD\\other".into());
    let c = run(&side("DEV"), &other, &opts());
    assert_eq!(find(&c, ObjectType::User, "DEV\\svc").unwrap().kind, DiffKind::OnlyIn1);
}

#[test]
fn mapped_table_with_another_name_is_different() {
    let a = Database { tables: vec![table("Client")], ..Default::default() };
    let b = Database { tables: vec![table("Customer")], ..Default::default() };
    let m = Mappings {
        tables: vec![TableMapping { source: ObjectName::new("dbo", "Client"), target: ObjectName::new("dbo", "Customer"), columns: vec![] }],
        ..Default::default()
    };
    let c = compare(&a, &b, &opts(), &m);
    let d = find(&c, ObjectType::Table, "Client").unwrap();
    assert_eq!((d.kind, props(d)), (DiffKind::Different, vec!["Name"]));
    assert_eq!(d.target_name.as_ref().unwrap().name, "Customer");
    // Target already renamed: the mapping no longer applies and the names pair.
    let c = compare(&a, &a, &opts(), &m);
    assert!(c.identical(), "{:?}", c.differences);
}

#[test]
fn membership_is_reported_on_the_role() {
    let a = Database { principals: vec![role("R"), Principal { member_of: vec!["R".into(), "db_datareader".into()], ..user("u", None) }], ..Default::default() };
    let b = Database { principals: vec![role("R"), user("u", None)], ..Default::default() };
    let c = run(&a, &b, &opts());
    assert_eq!(find(&c, ObjectType::User, "u").unwrap().kind, DiffKind::Equal);
    assert_eq!(props(find(&c, ObjectType::Role, "R").unwrap()), vec!["Member"]);
    assert_eq!(props(find(&c, ObjectType::Role, "db_datareader").unwrap()), vec!["Member"]);
    // IgnoreUsers: user members don't count, the users themselves are still compared.
    let c = run(&a, &b, &opts().with(Opt::IgnoreUsers));
    assert!(c.identical(), "{:?}", c.differences);
    let c = run(&a, &Database::default(), &opts().with(Opt::IgnoreUsers));
    assert_eq!(find(&c, ObjectType::User, "u").unwrap().kind, DiffKind::OnlyIn1);
}

#[test]
fn public_and_guest_database_permissions() {
    let a = Database {
        permissions: vec![grant("DATABASE", None, None, "public", "VIEW DEFINITION"), grant("DATABASE", None, None, "guest", "CONNECT")],
        ..Default::default()
    };
    let c = run(&a, &Database::default(), &opts());
    for (ty, n) in [(ObjectType::Role, "public"), (ObjectType::User, "guest")] {
        let d = find(&c, ty, n).unwrap();
        assert_eq!((d.kind, props(d)), (DiffKind::Different, vec!["Permission"]));
    }
    assert!(run(&a, &a, &opts()).identical());
}

#[test]
fn user_properties() {
    let a = Database { principals: vec![Principal { kind: "SQL_USER".into(), ..user("u", None) }], ..Default::default() };
    let mut b = Database { principals: vec![Principal { kind: "SQL_USER".into(), default_schema: Some("dbo".into()), ..user("u", None) }], ..Default::default() };
    // No default schema is dbo.
    assert!(run(&a, &b, &opts().without(Opt::IgnoreUserProperties)).identical());
    b.principals[0].kind = "WINDOWS_USER".into();
    assert!(run(&a, &b, &opts()).identical(), "user type is a user property");
    let c = run(&a, &b, &opts().without(Opt::IgnoreUserProperties));
    assert_eq!(props(&c.differences[0]), vec!["User type"]);
}

#[test]
fn triggers() {
    let tr = |name: &str| Trigger { name: name.into(), definition: format!("CREATE TRIGGER {name} ON dbo.T AFTER INSERT AS SELECT 1"), events: vec!["INSERT".into()], ..Default::default() };
    let with = |t: Trigger| Database { tables: vec![Table { triggers: vec![t], ..table("T") }], ..Default::default() };
    let empty = Database { tables: vec![table("T")], ..Default::default() };
    // Replication triggers go by name, not by NOT FOR REPLICATION.
    let o = opts().with(Opt::IgnoreReplicationTriggers);
    assert!(run(&with(tr("MSmerge_ins_X")), &empty, &o).identical());
    assert!(!run(&with(Trigger { not_for_replication: true, ..tr("T_Ins") }), &empty, &o).identical());
    // SET options.
    let off = Trigger { settings: ModuleSettings { ansi_nulls: false, quoted_identifier: true }, ..tr("T_Ins") };
    let c = run(&with(off.clone()), &with(tr("T_Ins")), &opts());
    assert_eq!(c.differences[0].properties[0].name, "ANSI_NULLS");
    assert!(run(&with(off), &with(tr("T_Ins")), &opts().with(Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings)).identical());
    // CLR triggers: assembly, class, method, EXECUTE AS.
    let clr = |m: &str, ex: Option<&str>| Trigger { definition: String::new(), clr_method: Some(m.into()), execute_as: ex.map(Into::into), ..tr("C") };
    let c = run(&with(clr("[A].[K].[M]", None)), &with(clr("[A].[K].[N]", Some("OWNER"))), &opts());
    assert_eq!(props(&c.differences[0]), vec!["CLR method", "Execute as"]);
    // The header's table with or without brackets.
    let bracketed = Trigger { definition: "CREATE TRIGGER [dbo].[T_Ins] ON [dbo].[T] AFTER INSERT AS SELECT 1".into(), ..tr("T_Ins") };
    assert!(run(&with(tr("T_Ins")), &with(bracketed), &opts()).identical());
    // WITH APPEND is left out for targets that refuse it.
    let append = Trigger { definition: "CREATE TRIGGER T_Ins ON dbo.T FOR INSERT WITH APPEND AS SELECT 1".into(), ..tr("T_Ins") };
    let plain = Trigger { definition: "CREATE TRIGGER T_Ins ON dbo.T FOR INSERT AS SELECT 1".into(), ..tr("T_Ins") };
    assert!(run(&with(append), &with(plain), &opts()).identical());
}

#[test]
fn ddl_triggers_ignore_ignore_triggers() {
    let a = Database { ddl_triggers: vec![DdlTrigger { name: "D".into(), ..Default::default() }], ..Default::default() };
    let c = run(&a, &Database::default(), &opts().with(Opt::IgnoreTriggers));
    assert_eq!(find(&c, ObjectType::DdlTrigger, "D").unwrap().kind, DiffKind::OnlyIn1);
}

#[test]
fn history_table_compared_in_full() {
    let side = |dup: bool| {
        let mut t = table("T");
        t.temporal = Some(Temporal { history_table: ObjectName::new("dbo", "TH"), ..Default::default() });
        let h = Table { history_table: true, indexes: vec![Index { name: "IX".into(), ignore_dup_key: dup, ..Default::default() }], ..table("TH") };
        Database { tables: vec![t, h], ..Default::default() }
    };
    let c = run(&side(false), &side(true), &opts());
    let d = find(&c, ObjectType::Table, "T").unwrap();
    assert!(!d.properties.is_empty() && !d.properties[0].location.is_empty(), "{:?}", d.properties);
    assert_eq!(d.properties[0].location[0].name, "History table");
    assert_eq!(d.properties[0].name, "IGNORE_DUP_KEY");
}

#[test]
fn table_options_and_authorization() {
    let a = Database { tables: vec![Table { large_value_types_out_of_row: true, owner: Some("app".into()), ..table("T") }], ..Default::default() };
    let b = Database { tables: vec![table("T")], ..Default::default() };
    assert_eq!(props(&run(&a, &b, &opts()).differences[0]), vec!["Authorization", "Large value types out of row"]);
    assert_eq!(props(&run(&a, &b, &opts().with(Opt::IgnoreSchemaObjectAuthorization)).differences[0]), vec!["Large value types out of row"]);
    let ft = |dir: &str| Database {
        tables: vec![Table {
            file_table: Some(FileTable { directory: dir.into(), filename_collation: "Latin1_General_CI_AS".into(), primary_key: None }),
            columns: vec![Column { name: "stream_id".into(), ..Default::default() }],
            ..table("F")
        }],
        ..Default::default()
    };
    assert_eq!(props(&run(&ft("Docs"), &ft("Files"), &opts()).differences[0]), vec!["Directory name"]);
    let plain = Database { tables: vec![table("F")], ..Default::default() };
    assert_eq!(props(&run(&ft("Docs"), &plain, &opts()).differences[0])[0], "FileTable");
}

#[test]
fn user_type_permissions_and_bindings() {
    let ty = |rule: Option<&str>| UserType { schema: "dbo".into(), name: "Phone".into(), bound_rule: rule.map(Into::into), ..Default::default() };
    let a = Database { types: vec![ty(Some("[dbo].[R]"))], permissions: vec![grant("TYPE", Some("dbo"), Some("Phone"), "app", "REFERENCES")], ..Default::default() };
    let b = Database { types: vec![ty(None)], ..Default::default() };
    assert_eq!(props(&run(&a, &b, &opts()).differences[0]), vec!["Bound rule", "Permission"]);
    assert_eq!(props(&run(&a, &b, &opts().with(Opt::IgnoreBindings)).differences[0]), vec!["Permission"]);
}

#[test]
fn extended_properties() {
    let xp = |class: &str, object: Option<&str>, minor: Option<&str>, value: &str, ty: &str| ExtendedProperty {
        class: class.into(),
        schema: (class != "USER").then(|| "dbo".into()),
        object: object.map(Into::into),
        minor_type: minor.map(|_| "COLUMN".into()),
        minor_name: minor.map(Into::into),
        name: "MS_Description".into(),
        value: Some(value.into()),
        value_type: Some(ty.into()),
    };
    let col = |n: &str| Column { name: n.into(), ..Default::default() };
    let base = Database {
        principals: vec![role("R")],
        views: vec![View { schema: "dbo".into(), name: "V".into(), columns: vec![col("c")], ..Default::default() }],
        types: vec![UserType { schema: "dbo".into(), name: "TT".into(), kind: UserTypeKind::Table, columns: vec![col("c")], ..Default::default() }],
        ..Default::default()
    };
    for (class, object, minor, ty) in [("USER", "R", None, ObjectType::Role), ("VIEW", "V", Some("c"), ObjectType::View), ("TYPE", "TT", Some("c"), ObjectType::UserDefinedType)] {
        let mut a = base.clone();
        a.extended_properties.push(xp(class, Some(object), minor, "x", "nvarchar"));
        let c = run(&a, &base, &opts());
        assert_eq!(find(&c, ty, object).unwrap().kind, DiffKind::Different, "{class}");
    }
    // The base type counts: int 1 is not N'1'.
    let mut a = base.clone();
    a.extended_properties.push(xp("VIEW", Some("V"), None, "1", "int"));
    let mut b = base.clone();
    b.extended_properties.push(xp("VIEW", Some("V"), None, "1", "nvarchar"));
    assert_eq!(props(find(&run(&a, &b, &opts()), ObjectType::View, "V").unwrap()), vec!["Type"]);
    // Internal Microsoft properties are compared (the option only affects scripts folders).
    let mut a = base.clone();
    a.extended_properties.push(ExtendedProperty { name: "MS_DiagramPane1".into(), ..xp("VIEW", Some("V"), None, "x", "nvarchar") });
    let c = run(&a, &base, &opts().with(Opt::IgnoreInternallyUsedMicrosoftExtendedProperties));
    assert_eq!(find(&c, ObjectType::View, "V").unwrap().kind, DiffKind::Different);
}

#[test]
fn add_with_encryption() {
    let v = |text: &str| Database { views: vec![View { schema: "dbo".into(), name: "V".into(), definition: text.into(), ..Default::default() }], ..Default::default() };
    let a = v("CREATE VIEW dbo.V AS SELECT 1 AS x");
    assert!(run(&a, &a, &opts()).identical());
    // Only the deployment adds the encryption; the pair stays identical.
    assert!(run(&a, &a, &opts().with(Opt::AddWithEncryption)).identical());
}
