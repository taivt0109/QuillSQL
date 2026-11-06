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

impl Server {
    fn spec(&self, database: &str) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(&self.host, database, self.auth.clone());
        spec.trust_server_certificate = true;
        spec
    }

    /// Creates `name` from a script of GO-separated batches.
    fn create(&self, name: &str, script: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut master = db::connect_async(&self.spec("master")).await.unwrap();
            let q = db::quote(name);
            db::execute(&mut master, &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(name)))
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
            let q = db::quote(name);
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
    let opts = Options::default_set().with(Opt::IgnoreExtendedProperties).with(Opt::IgnoreChecks).with(Opt::IgnoreIndexes);
    let c = schema::compare(&db1, &db2, &opts, &Mappings::default());
    let customer = c.differences.iter().find(|d| d.name.name == "Customer").unwrap();
    assert!(customer.properties.iter().all(|p| p.location.first().is_some_and(|f| f.name == "Column") || p.name == "Column"));
}
