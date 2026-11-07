//! Backups as a comparison source against a real SQL Server (see compare_live.rs for the
//! setup). The server writes the backups; they are copied out of the Docker container
//! `QUILL_TEST_CONTAINER` (default `quill-mssql-2022`) with `docker cp`, and the backup is then
//! compared with the live database it was taken from. Databases are named QD_backup_*.

use std::path::PathBuf;

use quillsql::catalog;
use quillsql::data::compare::{RowSource as Side, compare_sources as compare_sides};
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
        // Retried: another test's cleanup on the shared server may kill the session.
        rt.block_on(async {
            for _ in 0..3 {
                let Ok(mut c) = db::connect_async(&self.spec("master")).await else { continue };
                let sql = format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END", db::literal(db));
                if db::execute(&mut c, &sql).await.is_ok() {
                    break;
                }
            }
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
    let differences: Vec<String> = c.differences.iter().filter(|d| d.kind != schema::DiffKind::Equal).map(|d| format!("{:?} {:?} {} {:?}", d.kind, d.object_type, d.name, d.properties)).collect();
    if let Ok(dir) = std::env::var("QUILL_BACKUP_DEBUG") {
        std::fs::write(format!("{dir}/b.json"), serde_json::to_string_pretty(&from_backup).unwrap()).unwrap();
        std::fs::write(format!("{dir}/b.json.live"), serde_json::to_string_pretty(&from_live).unwrap()).unwrap();
    }
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
    // Each compressed backup lays its blocks out differently (some block headers once stopped
    // the reader halfway); a few more of them read whole too.
    let live = catalog::read_database(&server.spec(&db)).unwrap();
    for _ in 0..8 {
        let files = server.backup(&db, &[&compressed], "INIT, FORMAT, COMPRESSION, COPY_ONLY");
        let b = backup::open(&files, &[], None).unwrap().read_database().unwrap();
        let c = schema::compare(&b, &live, &Options::default_set(), &Mappings::default());
        assert!(c.differences.iter().all(|d| d.kind == schema::DiffKind::Equal), "{files:?}");
    }
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

/// A SQL Backup file holding `stripes` as its devices, uncompressed and unencrypted, with a
/// section before the extents and a padding block (specs/gaps/backup.md, section 6).
fn to_sqb(stripes: &[PathBuf], out: &std::path::Path) {
    let images: Vec<Vec<u8>> = stripes.iter().map(|f| std::fs::read(f).unwrap()).collect();
    let mut h = vec![0u8; 1024];
    let pascal = |h: &mut Vec<u8>, at: usize, s: &[u8]| {
        h[at] = s.len() as u8;
        h[at + 1..at + 1 + s.len()].copy_from_slice(s);
    };
    pascal(&mut h, 0, b"SSBE");
    h[5] = 4;
    let size: usize = images.iter().map(Vec::len).sum();
    h[9..17].copy_from_slice(&(size as i64).to_le_bytes());
    pascal(&mut h, 507, b"SSBE");
    pascal(&mut h, 512, b"SQB4");
    h[981] = 70;
    h[1016] = 1;
    h[1017] = images.len() as u8;
    pascal(&mut h, 1019, b"SQB4");
    let mut f = h;
    f.extend_from_slice(&2i32.to_le_bytes());
    f.extend_from_slice(&24i64.to_le_bytes());
    f.extend_from_slice(&[0; 12]);
    f.extend_from_slice(&1i32.to_le_bytes());
    f.extend_from_slice(&[0; 16]);
    let chunk = 256 * 1024;
    for i in 0.. {
        let mut any = false;
        for (d, image) in images.iter().enumerate() {
            let Some(piece) = image.get(i * chunk..((i + 1) * chunk).min(image.len())) else { continue };
            any = true;
            if i == 1 && d == 0 {
                f.push(0xff);
                f.extend_from_slice(&3i32.to_le_bytes());
                f.extend_from_slice(b"pad");
            }
            f.push(d as u8);
            f.extend_from_slice(&(piece.len() as i32).to_le_bytes());
            f.extend_from_slice(piece);
        }
        if !any {
            break;
        }
    }
    for d in 0..images.len() {
        f.push(d as u8);
        f.extend_from_slice(&0i32.to_le_bytes());
    }
    std::fs::write(out, f).unwrap();
}

#[test]
fn sql_backup_files_read_like_the_database() {
    let Some(server) = server("sqb") else { return };
    let db = name("Sqb");
    server.create(&db, &format!("{SHOP}\nGO\n{SHOP_DATA}"));
    let (s1, s2) = (format!("{db}_1.bak"), format!("{db}_2.bak"));
    let _cleanup = Cleanup { server: &server, db: db.clone(), files: vec![s1.clone(), s2.clone()] };
    let striped = server.backup(&db, &[&s1, &s2], "INIT, FORMAT, NAME = N'sqb'");
    let sqb = server.dir.join(format!("{db}.sqb"));
    to_sqb(&striped, &sqb);
    let sets = backup::list_sets(std::slice::from_ref(&sqb), &[]).unwrap();
    assert_eq!((sets.len(), sets[0].name.as_str(), sets[0].database.as_str()), (1, "sqb", db.as_str()));
    assert_identical(&server, &db, &[sqb], None);
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

const OBJECTS: &str = include_str!("fixtures/objects/objects.sql");
const CLR: &str = include_str!("fixtures/objects/clr.hex");
const XML_SPATIAL: &str = include_str!("fixtures/compare/sources_xml_spatial.sql");

/// Sparse columns with a column set, and typed xml.
const SPARSE_TYPED_XML: &str = r#"CREATE XML SCHEMA COLLECTION dbo.TS AS N'<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="o"><xs:complexType><xs:sequence><xs:element name="n" type="xs:int"/><xs:element name="s" type="xs:string"/><xs:element name="d" type="xs:decimal"/><xs:element name="b" type="xs:boolean"/><xs:element name="t" type="xs:dateTime"/></xs:sequence><xs:attribute name="a" type="xs:double"/></xs:complexType></xs:element></xs:schema>';
GO
CREATE TABLE dbo.SP (id int PRIMARY KEY, s1 int SPARSE NULL, s2 nvarchar(20) SPARSE NULL, s3 datetime2(3) SPARSE NULL, x xml(dbo.TS) NULL, cs xml COLUMN_SET FOR ALL_SPARSE_COLUMNS);
GO
INSERT dbo.SP (id, s1, s2, s3, x) VALUES (1, 5, N'abc', '2020-01-02 03:04:05.678', N'<o a="1.5"><n>42</n><s>hi</s><d>12.50</d><b>true</b><t>2020-01-02T03:04:05Z</t></o>'), (2, NULL, NULL, NULL, NULL), (3, NULL, N'only', NULL, N'<o><n>-7</n><s/><d>0</d><b>0</b><t>1999-12-31T23:59:59.123+02:00</t></o>');"#;

/// The objects beyond tables and modules: CLR, XML schema collections, Service Broker, security
/// policies, graph and temporal tables, partitioning, XML and spatial indexes. Needs `clr
/// enabled` on and `clr strict security` off. Selective XML indexes are left out: their paths
/// are not read from backups.
#[test]
fn backups_of_every_object_kind() {
    let Some(server) = server("objects") else { return };
    let db = name("Objects");
    let xml: String = XML_SPATIAL.split("\nGO\n").filter(|b| !b.contains("SELECTIVE") && !b.contains("SSX_Doc2")).collect::<Vec<_>>().join("\nGO\n");
    let keys = "CREATE MASTER KEY ENCRYPTION BY PASSWORD = 'Qu1ll!MasterKey';
CREATE CERTIFICATE QuillCert WITH SUBJECT = 'Quill test', EXPIRY_DATE = '2030-01-01';
CREATE ASYMMETRIC KEY QuillAsym WITH ALGORITHM = RSA_2048;
CREATE SYMMETRIC KEY QuillSym WITH ALGORITHM = AES_256 ENCRYPTION BY CERTIFICATE QuillCert;";
    let script = format!("{}\nGO\n{xml}\nGO\n{SPARSE_TYPED_XML}\nGO\n{keys}",OBJECTS.replace("{CLR}", &format!("0x{}", CLR.trim())));
    server.create(&db, &script);
    let f = format!("{db}.bak");
    let _cleanup = Cleanup { server: &server, db: db.clone(), files: vec![f.clone()] };
    let files = server.backup(&db, &[&f], "INIT, FORMAT");
    assert_identical(&server, &db, &files, None);
}

/// A TDE-encrypted database is listed as encrypted and refused, as SQL Compare does. Creates a
/// certificate in master for the test and drops it afterwards (and the master key when the test
/// made it).
#[test]
fn encrypted_databases_are_refused() {
    let Some(server) = server("tde") else { return };
    let db = name("Tde");
    let cert = format!("QD_backup_{}_TdeCert", std::process::id());
    let rt = db::runtime().unwrap();
    let had_key = rt.block_on(async {
        let mut c = db::connect_async(&server.spec("master")).await.unwrap();
        !db::query(&mut c, "SELECT 1 FROM sys.symmetric_keys WHERE name = N'##MS_DatabaseMasterKey##'").await.unwrap().is_empty()
    });
    if !had_key {
        server.run("master", "CREATE MASTER KEY ENCRYPTION BY PASSWORD = 'Qu1ll!TdeTest#Key'");
    }
    server.run("master", &format!("CREATE CERTIFICATE {cert} WITH SUBJECT = 'QuillSQL TDE test'"));
    struct Undo<'a>(&'a Server, String, String, bool);
    impl Drop for Undo<'_> {
        fn drop(&mut self) {
            let f = format!("{}.bak", self.1);
            self.0.remove(&[&f]);
            self.0.drop(&self.1);
            let rt = db::runtime().unwrap();
            rt.block_on(async {
                let mut c = db::connect_async(&self.0.spec("master")).await.unwrap();
                let _ = db::execute(&mut c, &format!("DROP CERTIFICATE {}", self.2)).await;
                if !self.3 {
                    let _ = db::execute(&mut c, "DROP MASTER KEY").await;
                }
            });
        }
    }
    let _undo = Undo(&server, db.clone(), cert.clone(), had_key);
    server.create(&db, "CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY);");
    server.run(&db, &format!("CREATE DATABASE ENCRYPTION KEY WITH ALGORITHM = AES_256 ENCRYPTION BY SERVER CERTIFICATE {cert}; ALTER DATABASE CURRENT SET ENCRYPTION ON;"));
    // Wait for the encryption scan to finish.
    for _ in 0..60 {
        let state = rt.block_on(async {
            let mut c = db::connect_async(&server.spec("master")).await.unwrap();
            let rows = db::query(&mut c, &format!("SELECT encryption_state FROM sys.dm_database_encryption_keys WHERE database_id = DB_ID({})", db::literal(&db))).await.unwrap();
            rows.first().and_then(|r| r.get::<i32, _>(0)).unwrap_or(0)
        });
        if state == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let f = format!("{db}.bak");
    let files = server.backup(&db, &[&f], "INIT, FORMAT");
    let sets = backup::list_sets(&files, &[]).unwrap();
    assert!(sets[0].encrypted && !sets[0].can_handle());
    assert_eq!(backup::open(&files, &[], None).err().unwrap().to_string(), "Encrypted backups are not supported");
}
