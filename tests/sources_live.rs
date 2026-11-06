//! Scripts folders against a real SQL Server: makescripts → read back, deploying to a folder,
//! and deploying from a folder. Run only when QUILL_TEST_SERVER is set (see compare_live.rs).
//! Databases are named QD_sources_* and dropped afterwards.

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

    fn read(&self, name: &str) -> Database {
        catalog::read_database(&self.spec(name)).unwrap()
    }
}

const SHOP: &str = include_str!("fixtures/compare/shop.sql");
const SHOP_CHANGES: &str = include_str!("fixtures/compare/shop_changes.sql");
const XML_SPATIAL: &str = include_str!("fixtures/compare/sources_xml_spatial.sql");

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
    for (name, script) in [("QD_sources_RT1", SHOP), ("QD_sources_RT2", changed.as_str()), ("QD_sources_RT3", XML_SPATIAL)] {
        server.create(name, script);
        let live = server.read(name);
        let dir = temp(name);
        let opts = Options::default_set();
        folder::write_all(&live, &dir, &opts).unwrap();
        let f = folder::read(&dir).unwrap();
        assert!(f.messages.is_empty(), "{:?}", f.messages);
        assert_identical(&f.database, &live, &opts, name);
        assert_identical(&live, &f.database, &opts.clone().without(Opt::IgnoreWhiteSpace), name);
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
        target: "QD_sources_Empty".into(),
        target_database: "QD_sources_Empty".into(),
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
