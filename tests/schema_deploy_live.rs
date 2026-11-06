//! Deployment-script parity against a real SQL Server (see compare_live.rs for the setup).
//! Databases are named QD_schemadeploy_*.

use quillsql::catalog::{self, IndexKind, XmlIndexKind};
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::{self, DiffKind, Mappings, Opt, Options, deploy};

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

    fn create(&self, name: &str, script: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut master = db::connect_async(&self.spec("master")).await.unwrap();
            let q = db::quote(name);
            db::execute(&mut master, &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(name)))
                .await
                .unwrap();
            let mut conn = db::connect_async(&self.spec(name)).await.unwrap();
            for batch in deploy::split_batches(script) {
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

    fn scalar(&self, database: &str, sql: &str) -> i32 {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut c = db::connect_async(&self.spec(database)).await.unwrap();
            let rows = db::query(&mut c, sql).await.unwrap();
            rows[0].get::<i32, _>(0).unwrap()
        })
    }
}

/// Script that makes `to` like `from`.
fn script(server: &Server, from: &str, to: &str, opts: &Options) -> (deploy::DeploymentScript, catalog::Database) {
    let db1 = catalog::read_database(&server.spec(from)).unwrap();
    let db2 = catalog::read_database(&server.spec(to)).unwrap();
    let c = schema::compare(&db1, &db2, opts, &Mappings::default());
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: "SERIALIZABLE".into(),
        source: from.into(),
        target: to.into(),
        target_database: to.into(),
        target_major_version: db2.server.major_version,
    };
    (deploy::generate(&c, &db1, &db2, opts, &Mappings::default(), &settings).unwrap(), db1)
}

/// Deploys `from` onto `to` and checks that nothing is left to do.
fn converge(server: &Server, from: &str, to: &str, opts: &Options) -> deploy::DeploymentScript {
    let (script, db1) = script(server, from, to, opts);
    if let Err(e) = deploy::execute(&server.spec(to), &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    let after = catalog::read_database(&server.spec(to)).unwrap();
    let c = schema::compare(&db1, &after, opts, &Mappings::default());
    let left: Vec<_> = c.differences.iter().filter(|d| d.kind != DiffKind::Equal).collect();
    assert!(left.is_empty(), "still different after deployment: {left:#?}\n\n{}", script.sql);
    script
}

const SHOP: &str = include_str!("fixtures/compare/shop.sql");
const SHOP_CHANGES: &str = include_str!("fixtures/compare/shop_changes.sql");

const INDEXES: &str = "
CREATE TABLE dbo.Doc (Id int NOT NULL CONSTRAINT PK_Doc PRIMARY KEY CLUSTERED, Body xml NULL, Shape geometry NULL, Place geography NULL);
GO
CREATE PRIMARY XML INDEX PX_Doc ON dbo.Doc (Body);
GO
CREATE XML INDEX SX_Doc ON dbo.Doc (Body) USING XML INDEX PX_Doc FOR PATH;
GO
CREATE SPATIAL INDEX SI_Shape ON dbo.Doc (Shape) USING GEOMETRY_GRID
    WITH (BOUNDING_BOX = (0, 0, 500, 200.5), GRIDS = (LOW, LOW, MEDIUM, HIGH), CELLS_PER_OBJECT = 64);
GO
CREATE SPATIAL INDEX SI_Place ON dbo.Doc (Place) USING GEOGRAPHY_AUTO_GRID WITH (CELLS_PER_OBJECT = 12);
GO
CREATE TABLE dbo.Sel (Id int NOT NULL CONSTRAINT PK_Sel PRIMARY KEY CLUSTERED, Body xml NULL);
GO
CREATE SELECTIVE XML INDEX SXI_Sel ON dbo.Sel (Body)
    WITH XMLNAMESPACES ('http://example.com/ns' AS e)
    FOR (pathName = '/e:a/e:b' AS XQUERY 'xs:string' MAXLENGTH(40) SINGLETON, pathId = '/e:a/@id' AS SQL int);
GO
CREATE XML INDEX SXI_Sel_Name ON dbo.Sel (Body) USING XML INDEX SXI_Sel FOR (pathName);
";

#[test]
fn xml_and_spatial_indexes() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Ix1", "QD_schemadeploy_Ix2");
    server.create(a, INDEXES);
    let model = catalog::read_database(&server.spec(a)).unwrap();
    let doc = model.table("dbo", "Doc").unwrap();
    let ix = |n: &str| doc.indexes.iter().find(|i| i.name == n).unwrap();
    assert_eq!(ix("PX_Doc").xml.as_ref().unwrap().kind, XmlIndexKind::Primary);
    let sx = ix("SX_Doc").xml.as_ref().unwrap();
    assert_eq!((sx.kind.clone(), sx.secondary_type.as_deref(), sx.using_index.as_deref()), (XmlIndexKind::Secondary, Some("PATH"), Some("PX_Doc")));
    let shape = ix("SI_Shape");
    assert_eq!(shape.kind, IndexKind::Spatial);
    let sp = shape.spatial.as_ref().unwrap();
    assert_eq!((sp.tessellation.as_str(), sp.bounding_box.join(","), sp.grids.join(","), sp.cells_per_object), ("GEOMETRY_GRID", "0,0,500,200.5".into(), "LOW,LOW,MEDIUM,HIGH".into(), 64));
    assert!(ix("SI_Place").spatial.as_ref().unwrap().geography);
    let sel = model.table("dbo", "Sel").unwrap();
    let sxi = sel.indexes.iter().find(|i| i.name == "SXI_Sel").unwrap().xml.as_ref().unwrap();
    assert_eq!(sxi.kind, XmlIndexKind::Selective);
    assert_eq!((sxi.paths.len(), sxi.namespaces.len()), (2, 1));
    let sec = sel.indexes.iter().find(|i| i.name == "SXI_Sel_Name").unwrap().xml.as_ref().unwrap();
    assert_eq!((sec.kind.clone(), sec.using_path.as_deref()), (XmlIndexKind::SecondarySelective, Some("pathName")));

    // From an empty database, and back.
    server.create(b, "SELECT 1");
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.sql.contains("CREATE PRIMARY XML INDEX [PX_Doc]") && s.sql.contains("sp_db_selective_xml_index"), "{}", s.sql);
    server.create(b, "SELECT 1");
    converge(&server, b, a, &Options::default_set());

    // Changed definitions are dropped and created again.
    server.create(a, INDEXES);
    let changed = INDEXES
        .replace("FOR PATH", "FOR VALUE")
        .replace("GRIDS = (LOW, LOW, MEDIUM, HIGH), CELLS_PER_OBJECT = 64", "GRIDS = (HIGH, HIGH, HIGH, HIGH), CELLS_PER_OBJECT = 32")
        .replace("MAXLENGTH(40)", "MAXLENGTH(80)");
    server.create(b, &changed);
    let s = converge(&server, b, a, &Options::default_set());
    assert!(s.sql.contains("Dropping XML index [SX_Doc] from [dbo].[Doc]") && s.sql.contains("FOR VALUE"), "{}", s.sql);
    server.create(a, INDEXES);
    converge(&server, a, b, &Options::default_set());
    server.drop(a);
    server.drop(b);
}

#[test]
fn existence_checks_make_scripts_rerunnable() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Ex1", "QD_schemadeploy_Ex2");
    let changed = format!("{SHOP}\nGO\n{SHOP_CHANGES}");
    for opts in [
        Options::default_set().with(Opt::ObjectExistenceChecks),
        Options::default_set().with(Opt::DropAndCreateInsteadOfAlter),
    ] {
        for (from, to) in [(&changed, SHOP), (&SHOP.to_string(), changed.as_str())] {
            server.create(a, from);
            server.create(b, to);
            let s = converge(&server, a, b, &opts);
            assert!(s.sql.contains("IF OBJECT_ID(N'"), "{}", s.sql);
            // Running the same script again finds everything in place.
            if let Err(e) = deploy::execute(&server.spec(b), &s, |_, _| {}) {
                panic!("second run: {e}\n\n{}", s.sql);
            }
        }
    }
    server.drop(a);
    server.drop(b);
}

#[test]
fn ddl_triggers_are_disabled_for_the_deployment() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Ddl1", "QD_schemadeploy_Ddl2");
    let block = "CREATE TRIGGER QD_block ON DATABASE FOR CREATE_TABLE AS BEGIN RAISERROR('no new tables', 16, 1); ROLLBACK; END";
    server.create(a, &format!("CREATE TABLE dbo.T (a int)\nGO\n{block}"));
    server.create(b, block);
    let (s, _) = script(&server, a, b, &Options::default_set());
    assert!(deploy::execute(&server.spec(b), &s, |_, _| {}).is_err(), "the trigger stops the deployment");
    let s = converge(&server, a, b, &Options::default_set().with(Opt::DisableAndReenableDdlTriggers));
    assert!(s.sql.contains("DISABLE TRIGGER ALL ON DATABASE") && s.sql.contains("ENABLE TRIGGER [QD_block] ON DATABASE"), "{}", s.sql);
    assert_eq!(server.scalar(b, "SELECT CAST(is_disabled AS int) FROM sys.triggers WHERE name = N'QD_block'"), 0);
    server.drop(a);
    server.drop(b);
}

#[test]
fn bound_defaults_and_rules() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Bind1", "QD_schemadeploy_Bind2");
    let base = "CREATE DEFAULT dbo.Zero AS 0\nGO\nCREATE RULE dbo.Positive AS @v >= 0\nGO\nCREATE TABLE dbo.T (a int NULL, b int NULL)\nGO\n";
    let bound = format!("{base}EXEC sp_bindefault N'dbo.Zero', N'dbo.T.a'\nGO\nEXEC sp_bindrule N'dbo.Positive', N'dbo.T.a'\nGO\nEXEC sp_bindrule N'dbo.Positive', N'dbo.T.b'");
    server.create(a, &bound);
    let model = catalog::read_database(&server.spec(a)).unwrap();
    let col = model.table("dbo", "T").unwrap().column("a").unwrap();
    assert_eq!((col.bound_default.as_deref(), col.bound_rule.as_deref()), (Some("[dbo].[Zero]"), Some("[dbo].[Positive]")));
    server.create(b, base);
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.sql.contains("EXEC sp_bindefault N'[dbo].[Zero]', N'[dbo].[T].[a]'"), "{}", s.sql);
    // And back: the bindings go.
    server.create(a, base);
    server.create(b, &bound);
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.sql.contains("EXEC sp_unbindrule N'[dbo].[T].[b]'"), "{}", s.sql);
    // A bound column that changes type is unbound around the ALTER COLUMN.
    server.create(a, &bound.replace("a int NULL", "a bigint NULL"));
    server.create(b, &bound);
    let s = converge(&server, a, b, &Options::default_set());
    let (u, alter, bind) = (s.sql.find("sp_unbindefault").unwrap(), s.sql.find("ALTER COLUMN [a] [bigint]").unwrap(), s.sql.find("sp_bindefault").unwrap());
    assert!(u < alter && alter < bind, "{}", s.sql);
    // IgnoreBindings: nothing to do.
    server.create(b, base);
    let (s, _) = script(&server, a, b, &Options::default_set().with(Opt::IgnoreBindings));
    assert!(!s.sql.contains("sp_bindefault"), "{}", s.sql);
    server.drop(a);
    server.drop(b);
}

#[test]
fn recovered_tables_and_rebuild_names() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Rec1", "QD_schemadeploy_Rec2");
    let leftover = "CREATE TABLE dbo.RG_Recovery_3_T (a int)\nGO\n";
    server.create(a, &format!("{leftover}CREATE TABLE dbo.T (a int IDENTITY(10, 1) NOT NULL, b int NULL)"));
    server.create(b, &format!("{leftover}CREATE TABLE dbo.T (a int IDENTITY(1, 1) NOT NULL, b int NULL)\nGO\nINSERT INTO dbo.T (b) VALUES (1), (2)"));
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.sql.contains("CREATE TABLE [dbo].[RG_Recovery_4_T]"), "{}", s.sql);
    assert!(s.warnings.iter().any(|w| w.object == "[dbo].[RG_Recovery_3_T]" && w.message.contains("recovered from a failed deployment")), "{:?}", s.warnings);
    assert_eq!(server.scalar(b, "SELECT COUNT(*) FROM dbo.T"), 2);
    server.drop(a);
    server.drop(b);
}

#[test]
fn change_tracking_follows_the_primary_key() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Ct1", "QD_schemadeploy_Ct2");
    let table = |key: &str| {
        format!(
            "ALTER DATABASE CURRENT SET CHANGE_TRACKING = ON\nGO\nCREATE TABLE dbo.T (a int NOT NULL, b int NOT NULL, CONSTRAINT PK_T PRIMARY KEY ({key}))\nGO\nALTER TABLE dbo.T ENABLE CHANGE_TRACKING"
        )
    };
    server.create(a, &table("a, b"));
    server.create(b, &table("a"));
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.sql.contains("DISABLE CHANGE_TRACKING") && s.warnings.iter().any(|w| w.message.contains("needs to be disabled and re-enabled")), "{}", s.sql);
    assert_eq!(server.scalar(b, "SELECT COUNT(*) FROM sys.change_tracking_tables WHERE object_id = OBJECT_ID('dbo.T')"), 1);
    server.drop(a);
    server.drop(b);
}

#[test]
fn modules_get_encrypted() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_schemadeploy_Enc1", "QD_schemadeploy_Enc2");
    server.create(a, "CREATE VIEW dbo.V AS SELECT 1 AS c\nGO\nCREATE PROCEDURE dbo.P @x int AS SELECT @x");
    server.create(b, "SELECT 1");
    let (s, _) = script(&server, a, b, &Options::default_set().with(Opt::AddWithEncryption));
    deploy::execute(&server.spec(b), &s, |_, _| {}).unwrap_or_else(|e| panic!("{e}\n{}", s.sql));
    assert_eq!(server.scalar(b, "SELECT COUNT(*) FROM sys.sql_modules WHERE definition IS NULL AND object_id IN (OBJECT_ID('dbo.V'), OBJECT_ID('dbo.P'))"), 2);
    server.drop(a);
    server.drop(b);
}
