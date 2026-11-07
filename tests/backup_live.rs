//! Backups as a comparison source against a real SQL Server (see compare_live.rs for the
//! setup). The server writes the backups; they are copied out of the Docker container
//! `QUILL_TEST_CONTAINER` (default `quill-mssql-2022`) with `docker cp`, and the backup is then
//! compared with the live database it was taken from. Databases are named QD_backup_*.

use std::path::PathBuf;

use quillsql::catalog;
use quillsql::data::compare::{Side, compare_sides};
use quillsql::data::mapping::DataMappings;
use quillsql::data::store::RowKind;
use quillsql::data::DataOptions;
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::{self, Mappings, Options};
use quillsql::sources::backup;

struct Server {
    host: String,
    auth: Auth,
    container: String,
    dir: PathBuf,
}

fn server(tag: &str) -> Option<Server> {
    let host = std::env::var("QUILL_TEST_SERVER").ok()?;
    let auth = match std::env::var("QUILL_TEST_USER") {
        Ok(user) => Auth::Sql { user, password: std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default() },
        Err(_) => Auth::Windows,
    };
    let container = std::env::var("QUILL_TEST_CONTAINER").unwrap_or_else(|_| "quill-mssql-2022".into());
    let dir = std::env::temp_dir().join(format!("quill-backup-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Some(Server { host, auth, container, dir })
}

fn name(n: &str) -> String {
    format!("QD_backup_{}_{n}", std::process::id())
}

impl Server {
    fn spec(&self, database: &str) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(&self.host, database, self.auth.clone());
        spec.trust_server_certificate = true;
        spec
    }

    fn run(&self, database: &str, sql: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut c = db::connect_async(&self.spec(database)).await.unwrap();
            for batch in schema::deploy::split_batches(sql) {
                db::execute(&mut c, &batch).await.unwrap_or_else(|e| panic!("{e}\n{batch}"));
            }
        });
    }

    fn create(&self, db: &str, script: &str) {
        let q = db::quote(db);
        self.run("master", &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(db)));
        self.run(db, script);
    }

    fn drop(&self, db: &str) {
        let q = db::quote(db);
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut c = db::connect_async(&self.spec("master")).await.unwrap();
            let _ = db::execute(&mut c, &format!("ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q};")).await;
        });
    }

    /// Backs `db` up to the files (`with` adds options) and copies them out of the container.
    fn backup(&self, db: &str, files: &[&str], with: &str) -> Vec<PathBuf> {
        let disks: Vec<String> = files.iter().map(|f| format!("DISK = N'/var/opt/mssql/data/{f}'")).collect();
        self.run("master", &format!("BACKUP DATABASE {} TO {} WITH {with}", db::quote(db), disks.join(", ")));
        files
            .iter()
            .map(|f| {
                let local = self.dir.join(f);
                let ok = std::process::Command::new("docker")
                    .args(["cp", &format!("{}:/var/opt/mssql/data/{f}", self.container)])
                    .arg(&local)
                    .status()
                    .is_ok_and(|s| s.success());
                assert!(ok, "docker cp of {f} failed");
                local
            })
            .collect()
    }

    fn remove(&self, files: &[&str]) {
        for f in files {
            let _ = std::process::Command::new("docker").args(["exec", &self.container, "rm", "-f", &format!("/var/opt/mssql/data/{f}")]).status();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Drops the database and removes its backup files from the container, also when a test fails.
struct Cleanup<'a> {
    server: &'a Server,
    db: String,
    files: Vec<String>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let files: Vec<&str> = self.files.iter().map(String::as_str).collect();
        self.server.remove(&files);
        self.server.drop(&self.db);
    }
}

const SHOP: &str = include_str!("fixtures/compare/shop.sql");
const SHOP_DATA: &str = include_str!("fixtures/compare/shop_data.sql");
const KEYS: &str = include_str!("fixtures/compare/keys.sql");
const BINDINGS: &str = include_str!("fixtures/compare/sources_bindings.sql");

/// The schema read from the backup is the live schema, and so is the data.
fn assert_identical(server: &Server, live: &str, files: &[PathBuf], set: Option<&str>) {
    let b = backup::open(files, &[], set).unwrap_or_else(|e| panic!("{files:?}: {e}"));
    let from_backup = b.read_database().unwrap();
    let spec = server.spec(live);
    let from_live = catalog::read_database(&spec).unwrap();
    let c = schema::compare(&from_backup, &from_live, &Options::default_set(), &Mappings::default());
    let differences: Vec<String> = c.differences.iter().filter(|d| d.kind != schema::DiffKind::Equal).map(|d| format!("{:?} {} {:?}", d.object_type, d.name, d.properties)).collect();
    assert!(differences.is_empty(), "{files:?}: {differences:#?}");
    let data = compare_sides(Side::Backup(&b), Side::Live(&spec), &from_backup, &from_live, &DataOptions::default_set(), &DataMappings::default(), |_| true, 4, |_, _| {}).unwrap();
    for t in &data.tables {
        if t.pair.problem.is_some() {
            continue;
        }
        assert!(t.error.is_none(), "{}: {:?}", t.pair.source, t.error);
        assert!(t.identical(), "{} differs: {:?}", t.pair.source, t.counts);
    }
}

#[test]
fn backups_read_like_the_database() {
    let Some(server) = server("shop") else { return };
    let db = name("Shop");
    server.create(&db, &format!("{SHOP}\nGO\n{SHOP_DATA}\nGO\n{KEYS}\nGO\n{BINDINGS}"));
    let p = |f: &str| format!("{db}_{f}.bak");
    let (plain, compressed, s1, s2, s3) = (p("plain"), p("compressed"), p("s1"), p("s2"), p("s3"));
    let _cleanup = Cleanup { server: &server, db: db.clone(), files: vec![plain.clone(), compressed.clone(), s1.clone(), s2.clone(), s3.clone()] };

    let files = server.backup(&db, &[&plain], "INIT, FORMAT, NAME = N'first'");
    assert_identical(&server, &db, &files, None);
    // Copy-only, so that the first backup stays the base of the differential below.
    let files = server.backup(&db, &[&compressed], "INIT, FORMAT, COMPRESSION, COPY_ONLY");
    assert_identical(&server, &db, &files, None);
    let striped = server.backup(&db, &[&s1, &s2, &s3], "INIT, FORMAT, COPY_ONLY");
    assert_identical(&server, &db, &striped, None);

    // A stripe missing is reported, not read.
    let err = backup::open(&striped[..2], &[], None).err().unwrap();
    assert_eq!(err.to_string(), "One or more backups are missing from the specified set of striped backups");

    // A differential over its full backup, in the same file: changes after the full backup are
    // seen with the differential and not with the full backup alone.
    server.run(&db, "UPDATE dbo.Customer SET Balance = Balance + 1; INSERT INTO dbo.Customer (Name, Email, Balance) VALUES (N'Later', 'later@example.com', 1); CREATE TABLE dbo.Later (Id int NOT NULL PRIMARY KEY);");
    let files = server.backup(&db, &[&plain], "NOINIT, DIFFERENTIAL, NAME = N'second'");
    let sets = backup::list_sets(&files, &[]).unwrap();
    assert_eq!(sets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert_identical(&server, &db, &files, None);
    let full = backup::open(&files, &[], Some("first")).unwrap();
    let old = full.read_database().unwrap();
    assert!(old.table("dbo", "Later").is_none() && old.table("dbo", "Customer").is_some());

    // Data compare of the full backup with the changed database finds the changes.
    let spec = server.spec(&db);
    let live = catalog::read_database(&spec).unwrap();
    let r = compare_sides(Side::Backup(&full), Side::Live(&spec), &old, &live, &DataOptions::default_set(), &DataMappings::default(), |_| true, 4, |_, _| {}).unwrap();
    let customer = r.tables.iter().find(|t| t.pair.source.name == "Customer").unwrap();
    assert_eq!((customer.count(RowKind::Different), customer.count(RowKind::OnlyIn2)), (3, 1), "{:?}", customer.error);
}

#[test]
fn compressed_rows_of_every_type() {
    let Some(server) = server("types") else { return };
    let db = name("Types");
    let table = |name: &str, compression: &str| {
        format!(
            "CREATE TABLE dbo.{name} (id int NOT NULL PRIMARY KEY, ti tinyint, si smallint, bi bigint, b1 bit, b2 bit, de decimal(18,4), mo money, sm smallmoney, fl float, re real,
                dt datetime, sd smalldatetime, da date, tm time(3), d2 datetime2(7), dto datetimeoffset(2), ch char(10), vc varchar(20), nc nchar(5), nv nvarchar(30),
                bn binary(6), vb varbinary(10), g uniqueidentifier, mx nvarchar(max), tx text) WITH (DATA_COMPRESSION = {compression});
             INSERT INTO dbo.{name} VALUES
             (1, 5, -2, 1234567890123, 1, 0, 12.3456, 10.5, -3.25, 1.5, 2.25, '2020-05-06 07:08:09.123', '2020-05-06 07:08', '2020-05-06', '07:08:09.123', '2020-05-06 07:08:09.1234567', '2020-05-06 07:08:09.12 +02:00', 'ab', 'hello', N'xy', N'héllo wörld 日本', 0x0102, 0x0A0B, 'D8D63B6C-0000-4000-8000-000000000001', N'max', 'text'),
             (2, NULL, 0, -1, NULL, 1, -0.0001, 0, 0, 0, 0, '1900-01-01', '1900-01-01', '0001-01-01', '00:00', '0001-01-01', '0001-01-01 00:00 +00:00', '', '', N'', N'', 0x, 0x, NULL, NULL, NULL),
             (-300, 255, 32767, -9223372036854775808, 0, 0, 99999999999999.9999, -922337203685477.5808, 214748.3647, -1e300, 3.4e38, '9999-12-31 23:59:59.997', '2079-06-06 23:59', '9999-12-31', '23:59:59.999', '9999-12-31 23:59:59.9999999', '9999-12-31 23:59:59.99 +14:00', 'abcdefghij', REPLICATE('z', 20), N'abcde', REPLICATE(N'Ω', 30), 0xFFFFFFFFFFFF, 0xFFFF, NEWID(), REPLICATE(N'q', 5000), REPLICATE('t', 9000)),
             (3, 0, 0, 0, 0, 0, -12.3456, -10.5, 1, 100, -0.5, '1753-01-01', '1900-01-01 00:01', '2000-01-01', '00:00:00.001', '2000-01-01', '2000-01-01 00:00 -05:30', 'a b', 'x ', N'Ω', N'😀x', 0x00, 0x00, NEWID(), REPLICATE(CONVERT(nvarchar(max), N'ab'), 600000), '');"
        )
    };
    let script = [table("RowNone", "NONE"), table("RowRow", "ROW"), table("RowPage", "PAGE")].join("\nGO\n")
        + "\nGO\nINSERT INTO dbo.RowPage SELECT id + 1000, ti, si, bi, b1, b2, de, mo, sm, fl, re, dt, sd, da, tm, d2, dto, ch, vc, nc, nv, bn, vb, g, mx, tx FROM dbo.RowPage;
           ALTER INDEX ALL ON dbo.RowPage REBUILD;";
    server.create(&db, &script);
    let f = format!("{db}.bak");
    let _cleanup = Cleanup { server: &server, db: db.clone(), files: vec![f.clone()] };
    let files = server.backup(&db, &[&f], "INIT, FORMAT, COMPRESSION");
    assert_identical(&server, &db, &files, None);
}
