//! Scripts folders against a real SQL Server: makescripts → read back, deploying to a folder,
//! and deploying from a folder. Run only when QUILL_TEST_SERVER is set (see compare_live.rs).
//! Databases are named QD_sources_*_<process id> and dropped afterwards.

use std::path::{Path, PathBuf};

use quillsql::catalog::{self, Database};
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::{self, DiffKind, Mappings, Opt, Options, deploy};
use quillsql::sources::{folder, parse};

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

    fn create(&self, name: &str, script: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut master = db::connect_async(&self.spec("master")).await.unwrap();
            let q = db::quote(&unique(name));
            db::execute(&mut master, &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(&unique(name))))
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
            let q = db::quote(&unique(name));
            let _ = db::execute(&mut master, &format!("ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q};")).await;
        });
    }

    fn read(&self, name: &str) -> Database {
        catalog::read_database(&self.spec(name)).unwrap()
    }
}

const SHOP: &str = include_str!("fixtures/compare/shop.sql");
const SHOP_CHANGES: &str = include_str!("fixtures/compare/shop_changes.sql");
const XML_SPATIAL: &str = include_str!("fixtures/compare/sources_xml_spatial.sql");
const BINDINGS: &str = include_str!("fixtures/compare/sources_bindings.sql");
const SHOP_DATA: &str = include_str!("fixtures/compare/shop_data.sql");

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quill-sources-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn assert_identical(a: &Database, b: &Database, opts: &Options, what: &str) {
    let c = schema::compare(a, b, opts, &Mappings::default());
    let left: Vec<_> = c.differences.iter().filter(|d| d.kind != DiffKind::Equal).collect();
    assert!(left.is_empty(), "{what}: {left:#?}");
}

#[test]
fn makescripts_reads_back_identical() {
    let Some(server) = server() else { return };
    let changed = format!("{SHOP}\nGO\n{SHOP_CHANGES}");
    for (name, script) in [("QD_sources_RT1", SHOP), ("QD_sources_RT2", changed.as_str()), ("QD_sources_RT3", XML_SPATIAL), ("QD_sources_RT4", BINDINGS)] {
        server.create(name, script);
        let live = server.read(name);
        let dir = temp(name);
        let opts = Options::default_set();
        folder::write_all(&live, &dir, &opts).unwrap();
        let f = folder::read(&dir).unwrap();
        assert!(f.messages.is_empty(), "{:?}", f.messages);
        assert_identical(&f.database, &live, &opts, name);
        assert_identical(&live, &f.database, &opts.clone().without(Opt::IgnoreWhiteSpace), name);
        // A SQL Compare snapshot of the database reads back the same.
        let snp = quillsql::sources::snp::read(&quillsql::sources::snp_write::write(&live)).unwrap();
        assert_identical(&snp, &live, &opts, &format!("{name} snapshot"));
        // DDL triggers are not compared; they still come back.
        let ddl = |d: &Database| d.ddl_triggers.iter().map(|t| (t.name.clone(), t.disabled)).collect::<Vec<_>>();
        assert_eq!(ddl(&f.database), ddl(&live), "{name}");
        // Writing what was read gives the same files.
        let again = temp(&format!("{name}-again"));
        folder::write_all(&f.database, &again, &opts).unwrap();
        for file in folder::script_files(&dir, &f.info) {
            let rel = file.strip_prefix(&dir).unwrap();
            assert_eq!(std::fs::read(&file).unwrap(), std::fs::read(again.join(rel)).unwrap(), "{}", rel.display());
        }
        // Keys, checks and foreign keys inside CREATE TABLE read back the same.
        let inline = temp(&format!("{name}-inline"));
        folder::write_all(&live, &inline, &opts.clone().with(Opt::InlineTableObjects)).unwrap();
        let f = folder::read(&inline).unwrap();
        assert!(f.messages.is_empty(), "{:?}", f.messages);
        assert_identical(&f.database, &live, &opts, &format!("{name} inline"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&again);
        let _ = std::fs::remove_dir_all(&inline);
        server.drop(name);
    }
}

/// The data tests run one at a time: both drive the same server hard.
static DATA: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `quill <command> ...` with the test server's login for the live `sides`.
fn quill(server: &Server, command: &str, sides: &[u8], args: &[String]) -> (i32, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_quill"));
    cmd.arg(command).args(args);
    for &n in sides {
        cmd.arg(format!("/server{n}:{}", server.host)).arg(format!("/trustServerCertificate{n}"));
        if let Auth::Sql { user, password } = &server.auth {
            cmd.arg(format!("/username{n}:{user}")).arg(format!("/password{n}:{password}"));
        }
    }
    let out = cmd.output().unwrap();
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn data_compare_with_a_scripts_folder() {
    let Some(server) = server() else { return };
    let _serial = DATA.lock().unwrap_or_else(|e| e.into_inner());
    let data = format!("{SHOP}\nGO\n{SHOP_DATA}");
    server.create("QD_sources_Data", &data);
    let staged = || {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut c = db::connect_async(&server.spec("master")).await.unwrap();
            db::query(&mut c, "SELECT name FROM sys.databases WHERE name LIKE N'QuillScripts[_]%'").await.unwrap().len()
        })
    };
    let before = staged();
    let dir = temp("data");
    let d = |s: &str| s.to_string();
    let path = dir.to_string_lossy().to_string();
    // A scripts folder with schema and data.
    folder::write_all(&server.read("QD_sources_Data"), &dir, &Options::default_set()).unwrap();
    let (code, out) = quill(&server, "data", &[1], &[format!("/database1:{}", unique("QD_sources_Data")), format!("/makescripts:{path}")]);
    assert_eq!(code, 0, "{out}");
    assert!(dir.join("Data").join("dbo.Customer_Data.sql").is_file(), "{out}");
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Data"))]);
    assert_eq!(code, 63, "{out}");
    // Change the database, compare, and deploy the database's data to the folder.
    let rt = db::runtime().unwrap();
    rt.block_on(async {
        let mut c = db::connect_async(&server.spec("QD_sources_Data")).await.unwrap();
        db::execute(&mut c, "UPDATE dbo.Customer SET Name = N'Anh' WHERE CustomerId = 1; DELETE FROM Sales.OrderLine WHERE LineNumber = 2;").await.unwrap();
    });
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Data"))]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[dbo].[Customer]") && out.contains("[Sales].[OrderLine]"), "{out}");
    let (code, out) = quill(&server, "data", &[1], &[format!("/database1:{}", unique("QD_sources_Data")), format!("/scripts2:{path}"), d("/synchronize")]);
    assert_eq!(code, 0, "{out}");
    assert!(std::fs::read_to_string(dir.join("Data").join("dbo.Customer_Data.sql")).unwrap().contains("N'Anh'"), "{out}");
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Data"))]);
    assert_eq!(code, 63, "{out}");
    // No temporary databases are left behind.
    assert!(staged() <= before);
    let _ = std::fs::remove_dir_all(&dir);
    server.drop("QD_sources_Data");
}

/// Data of many types, read from a scripts folder without a server.
const TYPES: &str = "CREATE TABLE dbo.Types (Id int NOT NULL PRIMARY KEY, Code char(4) NULL, Name nvarchar(50) COLLATE Latin1_General_CI_AI NULL, \
    Amount decimal(10, 3) NULL, Price money NULL, Ratio float NULL, Small real NULL, Flag bit NULL, Born date NULL, Seen datetime NULL, \
    At datetime2(3) NULL, Zone datetimeoffset(0) NULL, Clock time(2) NULL, Guid uniqueidentifier NULL, Blob varbinary(20) NULL, Fixed binary(4) NULL, Doc xml NULL);
GO
INSERT dbo.Types VALUES (1, 'ab', N'Ánh', 1.25, 3.5, 1.5e10, 0.1, 1, '2001-02-03', '2020-01-02 03:04:05.833', '2020-01-02 03:04:05.123', '2020-01-02 03:04:05 +07:00', '10:11:12.34', '6F9619FF-8B86-D011-B42D-00C04FC964FF', 0x0102, 0x0A, N'<a b=\"1\"/>');
INSERT dbo.Types VALUES (2, NULL, N'it''s', -0.001, -1, -2.25, 3, 0, NULL, '1999-12-31 23:59:59.997', NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT dbo.Types (Id) VALUES (3);
GO
CREATE TABLE dbo.Child (Id int NOT NULL PRIMARY KEY, TypesId int NULL REFERENCES dbo.Types (Id), Note varchar(10) NULL);
GO
INSERT dbo.Child VALUES (1, 1, 'x'), (2, NULL, 'y ');
";

#[test]
fn data_compare_with_a_scripts_folder_without_a_server() {
    let Some(server) = server() else { return };
    let _serial = DATA.lock().unwrap_or_else(|e| e.into_inner());
    server.create("QD_sources_Types", TYPES);
    let dir = temp("types");
    let d = |s: &str| s.to_string();
    let path = dir.to_string_lossy().to_string();
    folder::write_all(&server.read("QD_sources_Types"), &dir, &Options::default_set()).unwrap();
    let (code, out) = quill(&server, "data", &[1], &[format!("/database1:{}", unique("QD_sources_Types")), format!("/makescripts:{path}")]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Types")), d("/verbose")]);
    assert_eq!(code, 63, "{out}");
    assert!(!out.contains("temporary database"), "read without a server: {out}");
    let rt = db::runtime().unwrap();
    rt.block_on(async {
        let mut c = db::connect_async(&server.spec("QD_sources_Types")).await.unwrap();
        db::execute(&mut c, "UPDATE dbo.Types SET Name = N'Anh', Amount = 2 WHERE Id = 1; DELETE dbo.Child WHERE Id = 2; INSERT dbo.Child VALUES (3, 2, 'z');").await.unwrap();
    });
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Types"))]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[dbo].[Types]") && out.contains("[dbo].[Child]"), "{out}");
    // Deploy the database's data to the folder, then they are the same.
    let (code, out) = quill(&server, "data", &[1], &[format!("/database1:{}", unique("QD_sources_Types")), format!("/scripts2:{path}"), d("/synchronize")]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = quill(&server, "data", &[2], &[format!("/scripts1:{path}"), format!("/database2:{}", unique("QD_sources_Types"))]);
    assert_eq!(code, 63, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
    server.drop("QD_sources_Types");
}

#[test]
fn hand_written_script_reads_like_the_database() {
    let Some(server) = server() else { return };
    for (file, script) in [("shop.sql", SHOP), ("xml_spatial.sql", XML_SPATIAL)] {
        server.create("QD_sources_Hand", script);
        let live = server.read("QD_sources_Hand");
        let defaults = parse::Defaults { collation: live.collation.clone(), ..Default::default() };
        let mut r = parse::Reader::new(defaults);
        r.add_file(Path::new(file), script);
        let read = r.finish("hand");
        assert!(read.messages.is_empty(), "{:?}", read.messages);
        // Unnamed constraints get other generated names than the server's.
        let opts = Options::default_set().with(Opt::IgnoreSystemNamedConstraintNames);
        assert_identical(&read.database, &live, &opts, file);
        assert_identical(&live, &read.database, &opts, file);
        server.drop("QD_sources_Hand");
    }
}

#[test]
fn deploys_to_and_from_a_scripts_folder() {
    let Some(server) = server() else { return };
    let changed = format!("{SHOP}\nGO\n{SHOP_CHANGES}");
    server.create("QD_sources_Old", SHOP);
    server.create("QD_sources_New", &changed);
    let (old, new) = (server.read("QD_sources_Old"), server.read("QD_sources_New"));
    let opts = Options::default_set().with(Opt::NoAutoColumnMapping);

    // Deploy the new schema to a folder made from the old one.
    let dir = temp("sync");
    folder::write_all(&old, &dir, &opts).unwrap();
    let target = folder::read(&dir).unwrap();
    let c = schema::compare(&new, &target.database, &opts, &Mappings::default());
    let changes = folder::sync(&new, &target, &c.differences, &opts).unwrap();
    assert!(changes.deleted.iter().any(|p| p.ends_with("dbo.Orders.sql")), "{changes:?}");
    assert!(changes.written.iter().any(|p| p.ends_with("dbo.Product.sql")), "{changes:?}");
    assert!(!changes.written.iter().any(|p| p.ends_with("dbo.FullName.sql")), "unchanged objects keep their files: {changes:?}");
    let after = folder::read(&dir).unwrap();
    assert!(after.messages.is_empty(), "{:?}", after.messages);
    assert_identical(&new, &after.database, &opts, "folder after sync");

    // Deploy the folder to an empty database: dependencies come out of the scripts.
    server.create("QD_sources_Empty", "SELECT 1");
    let empty = server.read("QD_sources_Empty");
    let c = schema::compare(&after.database, &empty, &opts, &Mappings::default());
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: "SERIALIZABLE".into(),
        source: "folder".into(),
        target: unique("QD_sources_Empty"),
        target_database: unique("QD_sources_Empty"),
        target_major_version: empty.server.major_version,
    };
    let script = deploy::generate(&c, &after.database, &empty, &opts, &Mappings::default(), &settings).unwrap();
    if let Err(e) = deploy::execute(&server.spec("QD_sources_Empty"), &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    assert_identical(&after.database, &server.read("QD_sources_Empty"), &opts, "database deployed from the folder");

    let _ = std::fs::remove_dir_all(&dir);
    for name in ["QD_sources_Old", "QD_sources_New", "QD_sources_Empty"] {
        server.drop(name);
    }
}

#[test]
fn the_other_objects_round_trip_through_snapshots() {
    let Some(server) = server() else { return };
    let clr = |hex: &str| format!("0x{}", hex.trim());
    let objects = include_str!("fixtures/objects/objects.sql").replace("{CLR}", &clr(include_str!("fixtures/objects/clr.hex")));
    server.create("QD_sources_Objects", &objects);
    let mut live = catalog::read_database(&server.spec("QD_sources_Objects")).unwrap();
    server.drop("QD_sources_Objects");
    // Snapshots have no place for a classification's rank.
    for c in live.tables.iter_mut().flat_map(|t| &mut t.columns) {
        if let Some(s) = &mut c.sensitivity {
            s.rank = None;
        }
    }
    let opts = Options::default_set().without(Opt::IgnoreFileGroups).with(Opt::IgnoreCertificatesAndCryptoKeys);
    let snp = quillsql::sources::snp::read(&quillsql::sources::snp_write::write(&live)).unwrap();
    assert_identical(&snp, &live, &opts, "objects snapshot");
    assert_identical(&live, &snp, &opts, "objects snapshot, the other way");
}

#[test]
fn certificates_and_keys_round_trip_through_snapshots() {
    let Some(server) = server() else { return };
    server.create(
        "QD_sources_Keys",
        "CREATE USER KeyUser WITHOUT LOGIN;
        CREATE CERTIFICATE QuillCert ENCRYPTION BY PASSWORD = 'Str0ng!Pass1' WITH SUBJECT = 'Quill', START_DATE = '20250101', EXPIRY_DATE = '20300101';
        CREATE SYMMETRIC KEY QuillKey WITH ALGORITHM = AES_256 ENCRYPTION BY PASSWORD = 'Str0ng!Pass1';
        CREATE ASYMMETRIC KEY QuillAsym WITH ALGORITHM = RSA_2048 ENCRYPTION BY PASSWORD = 'Str0ng!Pass1';
        GRANT CONTROL ON CERTIFICATE::QuillCert TO KeyUser;
        GRANT VIEW DEFINITION ON SYMMETRIC KEY::QuillKey TO KeyUser;
        GRANT CONTROL ON ASYMMETRIC KEY::QuillAsym TO KeyUser;",
    );
    let live = catalog::read_database(&server.spec("QD_sources_Keys")).unwrap();
    server.drop("QD_sources_Keys");
    assert_eq!((live.certificates.len(), live.symmetric_keys.len(), live.asymmetric_keys.len()), (1, 1, 1));
    let snp = quillsql::sources::snp::read(&quillsql::sources::snp_write::write(&live)).unwrap();
    assert_identical(&snp, &live, &Options::default_set(), "keys snapshot");
    assert_eq!(snp.certificates, live.certificates);
    assert_eq!(snp.symmetric_keys, live.symmetric_keys);
}

#[test]
fn source_control_and_clone_markers_are_not_schema() {
    let Some(server) = server() else { return };
    server.create(
        "QD_sources_Markers",
        "EXEC sp_addextendedproperty N'SQLSourceControl Scripts Location', N'<x/>';
        EXEC sp_addextendedproperty N'sqlsourcecontrol Database Revision', N'7';
        EXEC sp_addextendedproperty N'IsSQLCloneDatabase', N'1';
        EXEC sp_addextendedproperty N'Owner', N'Team';",
    );
    let live = server.read("QD_sources_Markers");
    server.drop("QD_sources_Markers");
    let names: Vec<&str> = live.extended_properties.iter().filter(|p| p.class == "DATABASE").map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Owner"]);
    // A scripts folder or a snapshot with the clone marker reads without it too.
    let mut marked = live.clone();
    marked.extended_properties.push(catalog::ExtendedProperty { class: "DATABASE".into(), name: "IsSQLCloneDatabase".into(), value: Some("1".into()), ..Default::default() });
    let dir = temp("markers");
    folder::write_all(&marked, &dir, &Options::default_set()).unwrap();
    assert_identical(&folder::read(&dir).unwrap().database, &live, &Options::default_set(), "folder");
    let _ = std::fs::remove_dir_all(&dir);
    let snp = quillsql::sources::snp::read(&quillsql::sources::snp_write::write(&marked)).unwrap();
    assert!(!snp.extended_properties.iter().any(|p| p.name == "IsSQLCloneDatabase"));
}

/// SQL Compare itself (QUILL_TEST_SQLCOMPARE: the path to SQLCompare.exe) reads our snapshots of
/// the fixtures as identical to their databases.
#[test]
fn sql_compare_reads_our_snapshots_as_identical() {
    let Some(server) = server() else { return };
    let Ok(exe) = std::env::var("QUILL_TEST_SQLCOMPARE") else { return };
    let clr = |hex: &str| format!("0x{}", hex.trim());
    let objects = include_str!("fixtures/objects/objects.sql").replace("{CLR}", &clr(include_str!("fixtures/objects/clr.hex")));
    let keys = "CREATE USER KeyUser WITHOUT LOGIN;
        CREATE CERTIFICATE QuillCert ENCRYPTION BY PASSWORD = 'Str0ng!Pass1' WITH SUBJECT = 'Quill', START_DATE = '20250101', EXPIRY_DATE = '20300101';
        CREATE SYMMETRIC KEY QuillKey WITH ALGORITHM = AES_256 ENCRYPTION BY PASSWORD = 'Str0ng!Pass1';
        CREATE ASYMMETRIC KEY QuillAsym WITH ALGORITHM = RSA_2048 ENCRYPTION BY PASSWORD = 'Str0ng!Pass1';
        GRANT CONTROL ON CERTIFICATE::QuillCert TO KeyUser;";
    let changed = format!("{SHOP}\nGO\n{SHOP_CHANGES}");
    for (name, script) in [("QD_sources_RgShop", SHOP), ("QD_sources_RgChanged", changed.as_str()), ("QD_sources_RgBindings", BINDINGS), ("QD_sources_RgObjects", objects.as_str()), ("QD_sources_RgKeys", keys)] {
        server.create(name, script);
        let file = temp(name).with_extension("snp");
        std::fs::write(&file, quillsql::sources::snp_write::write(&server.read(name))).unwrap();
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg(format!("/snapshot1:{}", file.display())).arg(format!("/server2:{}", server.host)).arg(format!("/database2:{}", unique(name)));
        if let Auth::Sql { user, password } = &server.auth {
            cmd.arg(format!("/username2:{user}")).arg(format!("/password2:{password}"));
        }
        let out = cmd.output().unwrap();
        server.drop(name);
        let _ = std::fs::remove_file(&file);
        // 63: the databases are identical.
        assert_eq!(out.status.code(), Some(63), "{name}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
}
