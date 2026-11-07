//! Tests against a real SQL Server. They run only when QUILL_TEST_SERVER is set, e.g.
//! `QUILL_TEST_SERVER=localhost,14333 QUILL_TEST_USER=sa QUILL_TEST_PASSWORD=... cargo test --test compare_live`
//! (a Docker `mcr.microsoft.com/mssql/server` container is enough). Each test creates its own
//! databases and drops them afterwards.

use quillsql::catalog::{self, IndexKind, RoutineKind};
use quillsql::db::{self, Auth, ConnectionSpec};

struct Server {
    host: String,
    auth: Auth,
}

fn server() -> Option<Server> {
    let host = std::env::var("QUILL_TEST_SERVER").ok()?;
    let auth = match std::env::var("QUILL_TEST_USER") {
        Ok(user) => Auth::Sql { user, password: std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default() },
        Err(_) => Auth::Windows,
    };
    Some(Server { host, auth })
}

/// Test databases get the process id, so runs from other worktrees on the same server don't collide.
fn unique(name: &str) -> String {
    if name == "master" { name.to_string() } else { format!("{name}_{}", std::process::id()) }
}

impl Server {
    fn spec(&self, database: &str) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(&self.host, &unique(database), self.auth.clone());
        spec.trust_server_certificate = true;
        spec
    }

    /// Creates `name` from a script of GO-separated batches.
    fn create(&self, name: &str, script: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut master = db::connect_async(&self.spec("master")).await.unwrap();
            let q = db::quote(&unique(name));
            db::execute(&mut master, &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(&unique(name))))
                .await
                .unwrap();
            let mut conn = db::connect_async(&self.spec(name)).await.unwrap();
            for batch in batches(script) {
                db::execute(&mut conn, &batch).await.unwrap_or_else(|e| panic!("{e}\n{batch}"));
            }
        });
    }

    fn drop(&self, name: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut master = db::connect_async(&self.spec("master")).await.unwrap();
            let q = db::quote(&unique(name));
            let _ = db::execute(&mut master, &format!("ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q};")).await;
        });
    }
}

fn batches(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in script.lines() {
        if line.trim().eq_ignore_ascii_case("GO") {
            if !cur.trim().is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

const SHOP: &str = include_str!("fixtures/compare/shop.sql");

#[test]
fn reads_the_shop_fixture() {
    let Some(server) = server() else { return };
    server.create("QuillTest_Read", SHOP);
    let model = catalog::read_database(&server.spec("QuillTest_Read")).unwrap();
    server.drop("QuillTest_Read");

    assert!(model.server.major_version >= 11);
    let customer = model.table("dbo", "Customer").unwrap();
    let names: Vec<_> = customer.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["CustomerId", "Name", "Phone", "Email", "Balance", "CreatedAt", "Notes", "NameLength"]);
    assert_eq!(customer.column("Name").unwrap().data_type.sql(), "nvarchar(100)");
    assert_eq!(customer.column("Name").unwrap().collation.as_deref(), Some("Latin1_General_CI_AS"));
    assert_eq!(customer.column("Phone").unwrap().data_type.sql(), "[dbo].[Phone]");
    assert_eq!(customer.column("Notes").unwrap().data_type.sql(), "nvarchar(max)");
    assert!(customer.column("CustomerId").unwrap().identity.is_some());
    assert!(customer.column("NameLength").unwrap().computed.as_ref().unwrap().persisted);
    assert_eq!(customer.column("Balance").unwrap().default.as_ref().unwrap().name, "DF_Customer_Balance");
    assert!(customer.column("CreatedAt").unwrap().default.as_ref().unwrap().system_named);

    let pk = customer.primary_key().unwrap();
    assert_eq!((pk.name.as_str(), &pk.kind), ("PK_Customer", &IndexKind::Clustered));
    let ix = customer.indexes.iter().find(|i| i.name == "IX_Customer_Name").unwrap();
    assert!(ix.columns[0].descending);
    assert_eq!(ix.included, ["Email"]);
    assert_eq!(ix.fill_factor, 90);
    assert!(ix.filter.is_some());
    assert!(customer.indexes.iter().any(|i| i.unique_constraint && i.name == "UQ_Customer_Email"));

    let order = model.table("Sales", "Order").unwrap();
    assert_eq!(order.foreign_keys[0].on_delete, "CASCADE");
    assert_eq!(order.storage.compression[0].compression, "PAGE");
    assert_eq!(order.triggers[0].events, ["INSERT", "UPDATE"]);
    let line = model.table("Sales", "OrderLine").unwrap();
    assert!(line.foreign_keys[0].system_named && line.foreign_keys[0].disabled);

    let view = &model.views[0];
    assert!(view.schema_bound && view.indexes.len() == 1 && view.columns.len() == 3);
    let proc = model.routines.iter().find(|r| r.name == "GetOrders").unwrap();
    assert_eq!((proc.kind.clone(), proc.execute_as.as_deref()), (RoutineKind::Procedure, Some("OWNER")));
    assert_eq!(model.sequences[0].cache_size, Some(50));
    assert_eq!(model.synonyms[0].base_object, "[Sales].[Order]");
    assert_eq!(model.extended_properties.len(), 3);
    assert!(model.types.iter().any(|t| t.name == "IdList" && t.columns.len() == 2 && t.indexes.len() == 1));
    // A default that uses a sequence makes the table depend on it.
    assert!(model.dependencies.iter().any(|d| d.from.name == "Order" && d.to.name == "OrderNumbers"));
}

const SHOP_CHANGES: &str = include_str!("fixtures/compare/shop_changes.sql");

#[test]
fn compares_two_versions_of_the_shop() {
    use quillsql::schema::{self, DiffKind, Mappings, ObjectType, Opt, Options};
    let Some(server) = server() else { return };
    server.create("QuillTest_Cmp1", SHOP);
    server.create("QuillTest_Cmp2", &format!("{SHOP}\nGO\n{SHOP_CHANGES}"));
    let db1 = catalog::read_database(&server.spec("QuillTest_Cmp1")).unwrap();
    let db2 = catalog::read_database(&server.spec("QuillTest_Cmp2")).unwrap();
    server.drop("QuillTest_Cmp1");
    server.drop("QuillTest_Cmp2");

    // A database compared with itself is identical.
    assert!(schema::compare(&db1, &db1, &Options::default_set(), &Mappings::default()).identical());

    let c = schema::compare(&db1, &db2, &Options::default_set(), &Mappings::default());
    let find = |ty: ObjectType, name: &str| c.differences.iter().find(|d| d.object_type == ty && d.name.name == name).unwrap();
    let customer = find(ObjectType::Table, "Customer");
    assert_eq!(customer.kind, DiffKind::Different);
    let props: Vec<String> = customer
        .properties
        .iter()
        .map(|p| format!("{}/{}", p.location.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(">"), p.name))
        .collect();
    assert!(props.contains(&"Column/Data type".to_string()), "{props:?}");
    assert!(props.contains(&"/Column".to_string()), "{props:?}"); // Country only in database 2
    assert!(props.contains(&"Index/Included columns".to_string()), "{props:?}");
    assert!(!props.iter().any(|p| p.contains("FILLFACTOR")), "IgnoreFillFactor is a default: {props:?}");
    assert!(props.contains(&"Check constraint/Expression".to_string()), "{props:?}");
    assert!(props.contains(&"Extended property/Value".to_string()), "{props:?}");

    assert_eq!(find(ObjectType::View, "CustomerTotals").kind, DiffKind::Equal);
    assert_eq!(find(ObjectType::StoredProcedure, "GetOrders").kind, DiffKind::Different);
    assert_eq!(find(ObjectType::Table, "Product").kind, DiffKind::OnlyIn2);
    assert_eq!(find(ObjectType::Synonym, "Orders").kind, DiffKind::OnlyIn1);
    assert_eq!(find(ObjectType::Function, "FullName").kind, DiffKind::Equal);

    // Qty → Quantity has the same type: auto-mapped, so only the name differs.
    assert!(c.auto_mapped_columns.iter().any(|(_, a, b)| a == "Qty" && b == "Quantity"));
    let line = find(ObjectType::Table, "OrderLine");
    assert!(line.properties.iter().all(|p| p.name == "Name"), "{:?}", line.properties);
    let no_auto = schema::compare(&db1, &db2, &Options::default_set().with(Opt::NoAutoColumnMapping), &Mappings::default());
    let line = no_auto.differences.iter().find(|d| d.name.name == "OrderLine").unwrap();
    assert_eq!(line.properties.iter().filter(|p| p.name == "Column").count(), 2);

    // Ignoring the differences in Customer except the column ones.
    // Users, roles and permissions.
    let app = c.differences.iter().find(|d| d.object_type == ObjectType::User && d.name.name == "app").unwrap();
    let names: Vec<&str> = app.properties.iter().map(|p| p.name.as_str()).collect();
    // IgnoreUserProperties is a default, as in SQL Compare: the default schema is not compared.
    // Membership is reported on the role (here the fixed role db_datareader), as SQL Compare does.
    assert!(names.is_empty(), "{names:?}");
    let reader = find(ObjectType::Role, "db_datareader");
    assert!(reader.properties.iter().any(|p| p.name == "Member" && p.source.as_deref() == Some("app")), "{:?}", reader.properties);
    let all = schema::compare(&db1, &db2, &Options::default_set().without(Opt::IgnoreUserProperties), &Mappings::default());
    let app2 = all.differences.iter().find(|d| d.object_type == ObjectType::User && d.name.name == "app").unwrap();
    assert!(app2.properties.iter().any(|p| p.name == "Default schema"));
    assert_eq!(find(ObjectType::Role, "Auditors").kind, DiffKind::OnlyIn2);
    assert!(customer.properties.iter().any(|p| p.name == "Permission" && p.target.as_deref() == Some("GRANT SELECT TO app")));
    let ignored = schema::compare(&db1, &db2, &Options::default_set().with(Opt::IgnoreUsers).with(Opt::IgnorePermissions), &Mappings::default());
    // IgnoreUsers keeps the users but not their memberships and permissions.
    assert!(!ignored.differences.iter().any(|d| d.object_type == ObjectType::User && d.kind == DiffKind::Different));

    let opts = Options::default_set()
        .with(Opt::IgnoreExtendedProperties)
        .with(Opt::IgnoreChecks)
        .with(Opt::IgnoreIndexes)
        .with(Opt::IgnorePermissions);
    let c = schema::compare(&db1, &db2, &opts, &Mappings::default());
    let customer = c.differences.iter().find(|d| d.name.name == "Customer").unwrap();
    assert!(customer.properties.iter().all(|p| p.location.first().is_some_and(|f| f.name == "Column") || p.name == "Column"));
}

/// Deploys `from` onto `to` and checks that a new comparison finds nothing left to do.
fn converge(server: &Server, from_db: &str, to_db: &str, opts: &quillsql::schema::Options) -> quillsql::schema::deploy::DeploymentScript {
    use quillsql::schema::{self, Mappings, deploy};
    let db1 = catalog::read_database(&server.spec(from_db)).unwrap();
    let db2 = catalog::read_database(&server.spec(to_db)).unwrap();
    let c = schema::compare(&db1, &db2, opts, &Mappings::default());
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: "SERIALIZABLE".into(),
        source: from_db.into(),
        target: to_db.into(),
        target_database: to_db.into(),
        target_major_version: db2.server.major_version,
    };
    let script = deploy::generate(&c, &db1, &db2, opts, &Mappings::default(), &settings).unwrap();
    if let Err(e) = deploy::execute(&server.spec(to_db), &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    let after = catalog::read_database(&server.spec(to_db)).unwrap();
    let c = schema::compare(&db1, &after, opts, &Mappings::default());
    let left: Vec<_> = c.differences.iter().filter(|d| d.kind != schema::DiffKind::Equal).collect();
    assert!(left.is_empty(), "still different after deployment: {left:#?}\n\n{}", script.sql);
    script
}

#[test]
fn deployment_converges_both_ways() {
    use quillsql::schema::Options;
    let Some(server) = server() else { return };
    let changed = format!("{SHOP}\nGO\n{SHOP_CHANGES}");
    // Old → new.
    server.create("QuillTest_Dep1", SHOP);
    server.create("QuillTest_Dep2", &changed);
    let script = converge(&server, "QuillTest_Dep2", "QuillTest_Dep1", &Options::default_set());
    assert!(script.sql.contains("EXEC sp_rename"), "Qty is renamed, not dropped:\n{}", script.sql);
    // New → old.
    server.create("QuillTest_Dep1", SHOP);
    server.create("QuillTest_Dep2", &changed);
    converge(&server, "QuillTest_Dep1", "QuillTest_Dep2", &Options::default_set());
    // From an empty database: everything is created in dependency order.
    server.create("QuillTest_Dep1", "SELECT 1");
    converge(&server, "QuillTest_Dep2", "QuillTest_Dep1", &Options::default_set());
    // And everything dropped again.
    server.create("QuillTest_Dep1", "SELECT 1");
    converge(&server, "QuillTest_Dep1", "QuillTest_Dep2", &Options::default_set());
    server.drop("QuillTest_Dep1");
    server.drop("QuillTest_Dep2");
}

const SHOP_DATA: &str = include_str!("fixtures/compare/shop_data.sql");

#[test]
fn rebuild_keeps_data() {
    use quillsql::schema::Options;
    let Some(server) = server() else { return };
    // Source: identity seed changes (rebuild), a NOT NULL column with a default is added, and
    // a column under a schema-bound view changes type (the view is dropped and re-created).
    let source = SHOP
        .replace("CustomerId int IDENTITY(1, 1)", "CustomerId int IDENTITY(100, 1)")
        .replace("    CONSTRAINT UQ_Customer_Email", "    Country nchar(2) NOT NULL CONSTRAINT DF_Customer_Country DEFAULT (N'VN'),\n    CONSTRAINT UQ_Customer_Email")
        .replace("Total money NOT NULL", "Total decimal(19, 4) NOT NULL");
    server.create("QuillTest_Reb1", &source);
    server.create("QuillTest_Reb2", &format!("{SHOP}\nGO\n{SHOP_DATA}"));
    let script = converge(&server, "QuillTest_Reb1", "QuillTest_Reb2", &Options::default_set());
    assert!(script.sql.contains("INSERT INTO [dbo].[RG_Recovery_1_Customer]("), "{}", script.sql);
    assert!(script.sql.contains("DROP VIEW [Sales].[CustomerTotals]"), "{}", script.sql);

    let rt = quillsql::db::runtime().unwrap();
    let (ids, countries, totals, ident) = rt.block_on(async {
        let mut c = quillsql::db::connect_async(&server.spec("QuillTest_Reb2")).await.unwrap();
        let ids = quillsql::db::query(&mut c, "SELECT STRING_AGG(CAST(CustomerId AS varchar(10)), ',') WITHIN GROUP (ORDER BY CustomerId) FROM dbo.Customer").await.unwrap();
        let countries = quillsql::db::query(&mut c, "SELECT COUNT(*) FROM dbo.Customer WHERE Country = N'VN'").await.unwrap();
        let totals = quillsql::db::query(&mut c, "SELECT CAST(SUM(Total) AS varchar(20)) FROM Sales.[Order]").await.unwrap();
        let ident = quillsql::db::query(&mut c, "SELECT CAST(IDENT_CURRENT('dbo.Customer') AS int)").await.unwrap();
        (
            ids[0].get::<&str, _>(0).unwrap().to_string(),
            ids_count(&countries),
            totals[0].get::<&str, _>(0).unwrap().to_string(),
            ident[0].get::<i32, _>(0).unwrap(),
        )
    });
    server.drop("QuillTest_Reb1");
    server.drop("QuillTest_Reb2");
    assert_eq!(ids, "1,2,7");
    assert_eq!(countries, 3);
    assert_eq!(totals, "115.4900");
    assert_eq!(ident, 7, "the identity continues where the old table left off");
}

fn ids_count(rows: &[tiberius::Row]) -> i32 {
    rows[0].get::<i32, _>(0).unwrap()
}

const KEYS: &str = include_str!("fixtures/compare/keys.sql");

#[test]
fn data_compare_orders_awkward_keys() {
    use quillsql::data::compare::compare;
    use quillsql::data::mapping::DataMappings;
    use quillsql::data::store::RowKind;
    use quillsql::data::{DataOpt, DataOptions};
    let Some(server) = server() else { return };
    server.create("QuillTest_Data1", KEYS);
    // Copy every row, then change a few in the second database.
    let copy = format!("INSERT INTO dbo.KeyGuid (Id, V) SELECT Id, V FROM {}.dbo.KeyGuid;", unique("QuillTest_Data1"));
    let script = KEYS.replace("INSERT INTO dbo.KeyGuid (Id, V) SELECT TOP (500) NEWID(), ROW_NUMBER() OVER (ORDER BY (SELECT 1)) FROM sys.all_objects;", &copy);
    let changes = "UPDATE dbo.KeyText SET V = 100 WHERE Code = N'ß';\nDELETE FROM dbo.KeyText WHERE Code = N'中';\nINSERT INTO dbo.KeyText (Code, V) VALUES (N'new', 0);\nUPDATE TOP (3) dbo.KeyGuid SET V = -V;\nDELETE TOP (2) FROM dbo.KeyGuid WHERE V > 0;\nUPDATE dbo.KeyMixed SET V = 2.000 WHERE V = 2;\nUPDATE dbo.KeyVarchar SET Code = 'b' WHERE Code = 'B';";
    server.create("QuillTest_Data2", &format!("{script}\nGO\n{changes}"));
    let db1 = catalog::read_database(&server.spec("QuillTest_Data1")).unwrap();
    let db2 = catalog::read_database(&server.spec("QuillTest_Data2")).unwrap();
    let run = |opts: &DataOptions| {
        compare(&server.spec("QuillTest_Data1"), &server.spec("QuillTest_Data2"), &db1, &db2, opts, &DataMappings::default(), |_| true, 4, |_, _| {}).unwrap()
    };
    let result = run(&DataOptions::default_set());
    let get = |name: &str| result.tables.iter().find(|t| t.pair.source.name == name).unwrap();
    for t in &result.tables {
        assert!(t.error.is_none(), "{}: {:?}", t.pair.source, t.error);
        assert!(!t.repeated_keys, "{}", t.pair.source);
    }
    let text = get("KeyText");
    assert_eq!((text.count(RowKind::Different), text.count(RowKind::OnlyIn1), text.count(RowKind::OnlyIn2)), (1, 1, 1));
    assert_eq!(text.count(RowKind::Identical), 13);
    let guid = get("KeyGuid");
    assert_eq!((guid.count(RowKind::Different), guid.count(RowKind::OnlyIn1), guid.count(RowKind::OnlyIn2)), (3, 2, 0));
    let mixed = get("KeyMixed");
    assert!(mixed.identical(), "2 and 2.000 are the same decimal: {:?}", mixed.counts);
    // 'B' → 'b' is the same key in a case-insensitive column; the value differs only by case.
    let varchar = get("KeyVarchar");
    assert!(varchar.identical(), "{:?}", varchar.counts);
    // With binary collation, 'B' and 'b' are different keys.
    let binary = run(&DataOptions::default_set().with(DataOpt::ForceBinaryCollation));
    let varchar = binary.tables.iter().find(|t| t.pair.source.name == "KeyVarchar").unwrap();
    assert_eq!((varchar.count(RowKind::OnlyIn1), varchar.count(RowKind::OnlyIn2)), (1, 1), "{:?}", varchar.error);
    // The stored rows can be read back.
    let rows = quillsql::data::store::read_all(&text.path, &text.offsets[RowKind::Different as usize]).unwrap();
    assert_eq!(rows[0].source[0], quillsql::data::value::Value::Str("ß".into()));
    // Checksums agree for an identical table and disagree for a changed one.
    let ck = run(&DataOptions::default_set().with(DataOpt::UseChecksumComparison));
    assert!(ck.tables.iter().find(|t| t.pair.source.name == "KeyMixed").unwrap().by_checksum);
    assert!(!ck.tables.iter().find(|t| t.pair.source.name == "KeyGuid").unwrap().by_checksum);
    server.drop("QuillTest_Data1");
    server.drop("QuillTest_Data2");
}

#[test]
fn data_deployment_converges() {
    use quillsql::data::compare::compare;
    use quillsql::data::mapping::DataMappings;
    use quillsql::data::store::RowKind;
    use quillsql::data::{DataOptions, sync};
    use quillsql::schema::deploy;
    let Some(server) = server() else { return };
    let base = format!("{SHOP}\nGO\n{KEYS}");
    server.create("QuillTest_Sync1", &format!("{base}\nGO\n{SHOP_DATA}"));
    // The target: some rows changed, some missing, some extra (including a new parent row).
    let target_rows = "SET IDENTITY_INSERT dbo.Customer ON;\nINSERT INTO dbo.Customer (CustomerId, Name, Email, Balance, Notes) VALUES (1, N'An Nguyen', 'an@example.com', 10, N'x'), (7, N'Chi', 'chi@example.com', 6, NULL), (9, N'Dung', NULL, 1, N'it''s here');\nSET IDENTITY_INSERT dbo.Customer OFF;\nINSERT INTO Sales.[Order] (OrderId, CustomerId, Total, Status) VALUES (1, 1, 12.5, 'N'), (3, 7, 99.99, 'S'), (4, 9, 1, 'N');\nINSERT INTO Sales.OrderLine (OrderId, LineNumber, Qty) VALUES (4, 1, 1);\nUPDATE dbo.KeyText SET V = NULL WHERE Code = N'a';\nDELETE FROM dbo.KeyMixed WHERE A = 'y';";
    let keys_copy = format!("DELETE FROM dbo.KeyGuid; INSERT INTO dbo.KeyGuid (Id, V) SELECT Id, V + 1 FROM {}.dbo.KeyGuid;", unique("QuillTest_Sync1"));
    server.create("QuillTest_Sync2", &format!("{base}\nGO\n{keys_copy}\nGO\n{target_rows}"));
    let (s1, s2) = (server.spec("QuillTest_Sync1"), server.spec("QuillTest_Sync2"));
    let db1 = catalog::read_database(&s1).unwrap();
    let db2 = catalog::read_database(&s2).unwrap();
    let opts = DataOptions::default_set();
    let result = compare(&s1, &s2, &db1, &db2, &opts, &DataMappings::default(), |_| true, 4, |_, _| {}).unwrap();
    assert!(!result.identical());
    let settings = sync::SyncSettings::new("s", "t", &unique("QuillTest_Sync2"));
    let script = sync::generate(&result, &db2, &opts, &settings).unwrap();
    if let Err(e) = deploy::execute(&s2, &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    let after = compare(&s1, &s2, &db1, &db2, &opts, &DataMappings::default(), |_| true, 4, |_, _| {}).unwrap();
    for t in &after.tables {
        assert!(t.identical(), "{} still differs: {:?} {:?}\n\n{}", t.pair.source, t.counts, t.error, script.sql);
    }
    assert!(after.tables.iter().any(|t| t.count(RowKind::Identical) >= 500));
    server.drop("QuillTest_Sync1");
    server.drop("QuillTest_Sync2");
}
