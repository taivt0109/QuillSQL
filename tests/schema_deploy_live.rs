//! Deployment-script parity against a real SQL Server (see compare_live.rs for the setup).
//! Databases are named QD_schemadeploy_<name>_<process id>: the same tests may run from several
//! worktrees against one server at once, and a shared name lets one run drop the other's database.

use quillsql::catalog::{self, IndexKind, XmlIndexKind};
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::{self, DiffKind, Mappings, Opt, Options, deploy};

/// A database name of this test run.
fn unique(name: &str) -> String {
    format!("QD_schemadeploy_{name}_{}", std::process::id())
}

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
    let (a, b) = (&unique("Ix1"), &unique("Ix2"));
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
    let (a, b) = (&unique("Ex1"), &unique("Ex2"));
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
    let (a, b) = (&unique("Ddl1"), &unique("Ddl2"));
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
    let (a, b) = (&unique("Bind1"), &unique("Bind2"));
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
    // A new NOT NULL column with a bound default: the table is rebuilt and the rows get the default.
    server.create(a, &format!("{}\nGO\nEXEC sp_bindefault N'dbo.Zero', N'dbo.T.c'", base.replace("b int NULL)", "b int NULL, c int NOT NULL)")));
    server.create(b, &format!("{base}INSERT INTO dbo.T (a, b) VALUES (1, 1), (2, 2)"));
    let s = converge(&server, a, b, &Options::default_set());
    assert!(s.warnings.iter().any(|w| w.message.contains("The default [dbo].[Zero] must be bound to it")), "{:?}", s.warnings);
    assert_eq!(server.scalar(b, "SELECT COUNT(*) FROM dbo.T WHERE c = 0"), 2);
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
    let (a, b) = (&unique("Rec1"), &unique("Rec2"));
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
    let (a, b) = (&unique("Ct1"), &unique("Ct2"));
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
    let (a, b) = (&unique("Enc1"), &unique("Enc2"));
    server.create(a, "CREATE VIEW dbo.V AS SELECT 1 AS c\nGO\nCREATE PROCEDURE dbo.P @x int AS SELECT @x");
    server.create(b, "SELECT 1");
    let (s, _) = script(&server, a, b, &Options::default_set().with(Opt::AddWithEncryption));
    deploy::execute(&server.spec(b), &s, |_, _| {}).unwrap_or_else(|e| panic!("{e}\n{}", s.sql));
    assert_eq!(server.scalar(b, "SELECT COUNT(*) FROM sys.sql_modules WHERE definition IS NULL AND object_id IN (OBJECT_ID('dbo.V'), OBJECT_ID('dbo.P'))"), 2);
    server.drop(a);
    server.drop(b);
}

/// A database script that gives the database a memory-optimized filegroup first.
fn with_memory_optimized_filegroup(db: &str, rest: &str) -> String {
    format!(
        "ALTER DATABASE [{db}] ADD FILEGROUP mo CONTAINS MEMORY_OPTIMIZED_DATA\nGO\nALTER DATABASE [{db}] ADD FILE (NAME = N'{db}_mo', FILENAME = N'/var/opt/mssql/data/{db}_mo') TO FILEGROUP mo\nGO\n{rest}"
    )
}

#[test]
fn memory_optimized_tables_are_rebuilt_through_a_copy() {
    let Some(server) = server() else { return };
    let (a, b) = (&unique("Mo1"), &unique("Mo2"));
    let disk = "CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, V nvarchar(20) NOT NULL CONSTRAINT DF_T_V DEFAULT (N'x'))\nGO\nINSERT INTO dbo.T (Id, V) VALUES (1, N'a'), (2, N'b'), (3, N'c')";
    let mo = |width: u32| {
        format!(
            "CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY NONCLUSTERED, V nvarchar({width}) NOT NULL CONSTRAINT DF_T_V DEFAULT (N'x'), W int NOT NULL CONSTRAINT DF_T_W DEFAULT (0)) WITH (MEMORY_OPTIMIZED = ON)"
        )
    };
    let opts = Options::default_set().with(Opt::NoTransactions);
    let rows = |server: &Server| server.scalar(b, "SELECT COUNT(*) FROM dbo.T WHERE W = 0 OR W IS NULL");
    // Disk-based to memory-optimized.
    server.create(a, &with_memory_optimized_filegroup(a, &mo(20)));
    server.create(b, &with_memory_optimized_filegroup(b, disk));
    let s = converge(&server, a, b, &opts);
    assert!(s.sql.contains("CREATE TABLE [dbo].[RG_Recovery_1_T]") && s.sql.contains("UPDATE STATISTICS [dbo].[T] WITH FULLSCAN, NORECOMPUTE"), "{}", s.sql);
    assert!(!s.warnings.iter().any(|w| w.message.contains("in a transaction, which is not supported")), "{:?}", s.warnings);
    assert_eq!(rows(&server), 3);
    // Memory-optimized to memory-optimized.
    server.create(a, &with_memory_optimized_filegroup(a, &mo(50)));
    converge(&server, a, b, &opts);
    assert_eq!(rows(&server), 3);
    // And back to disk, with the default constraint names kept.
    server.create(a, &with_memory_optimized_filegroup(a, &disk.replace("V nvarchar(20) NOT NULL CONSTRAINT DF_T_V DEFAULT (N'x')", "V nvarchar(50) NOT NULL CONSTRAINT DF_T_V DEFAULT (N'x'), W int NOT NULL CONSTRAINT DF_T_W DEFAULT (0)")));
    converge(&server, a, b, &opts);
    assert_eq!(rows(&server), 3);
    // With a transaction the deployment is warned about.
    server.create(a, &with_memory_optimized_filegroup(a, &mo(20)));
    let (s, _) = script(&server, a, b, &Options::default_set());
    assert!(s.warnings.iter().any(|w| w.message.contains("in a transaction, which is not supported")), "{:?}", s.warnings);
    server.drop(a);
    server.drop(b);
}

const SECRET_PROC: &str = "CREATE PROCEDURE dbo.Secret @n int\nWITH ENCRYPTION\nAS\n-- A comment long enough to take the text past one RC4 key block: 0123456789 abcdefghijklmnopqrstuvwxyz ÀÉÎÕÜ ăđơư 漢字.\nSELECT @n * 2 AS twice, N'it''s' AS quote, REPLICATE(N'x', 10) AS pad;";

#[test]
fn encrypted_modules_without_the_admin_connection() {
    let Some(server) = server() else { return };
    let db = &unique("Enc3");
    server.create(db, SECRET_PROC);
    // The dedicated administrator connection isn't reachable here: the reader carries on and the
    // procedure stays encrypted.
    let started = std::time::Instant::now();
    let model = catalog::read_database(&server.spec(db)).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(model.routines.iter().any(|r| r.name == "Secret" && r.encrypted && r.definition.is_empty()));
    assert!(!catalog::read_database_with(&server.spec(db), false).unwrap().routines[0].definition.contains("SELECT"));
    server.drop(db);
}

/// Reads the encrypted image over the DAC inside the Docker container (where it is local) and
/// decrypts it. Runs when QUILL_TEST_DOCKER_CONTAINER names the container.
#[test]
fn decrypts_images_read_over_the_admin_connection() {
    let Some(server) = server() else { return };
    let Ok(container) = std::env::var("QUILL_TEST_DOCKER_CONTAINER") else { return };
    let db = &unique("Enc4");
    server.create(db, SECRET_PROC);
    let password = std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default();
    let sql = "SET NOCOUNT ON; SELECT CONVERT(varchar(36), r.family_guid) + ' ' + CAST(v.objid AS varchar(12)) + ' ' + CAST(v.subobjid AS varchar(6)) + ' ' + CONVERT(varchar(max), v.imageval, 2) \
               FROM sys.sysobjvalues AS v JOIN sys.database_recovery_status AS r ON r.database_id = DB_ID() WHERE v.valclass = 1 AND v.objid = OBJECT_ID('dbo.Secret')";
    let out = std::process::Command::new("docker")
        .args(["exec", &container, "/opt/mssql-tools18/bin/sqlcmd", "-S", "admin:localhost", "-U", "sa", "-P", &password, "-C", "-d", db, "-y", "0", "-Q", sql])
        .output()
        .unwrap();
    server.drop(db);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let line = stdout.lines().map(str::trim).find(|l| l.split(' ').count() == 4 && l.len() > 60).unwrap_or_default().to_string();
    let parts: Vec<&str> = line.split(' ').collect();
    assert_eq!(parts.len(), 4, "{line}\n{}", String::from_utf8_lossy(&out.stderr));
    let cipher: Vec<u8> = (0..parts[3].len()).step_by(2).map(|i| u8::from_str_radix(&parts[3][i..i + 2], 16).unwrap()).collect();
    let text = catalog::decrypt::decrypt(parts[0], parts[1].parse().unwrap(), parts[2].parse().unwrap(), &cipher).unwrap();
    // The batch was sent with the line break the script splitter keeps.
    assert_eq!(text.trim_end(), SECRET_PROC);
}
