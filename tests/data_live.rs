//! Data compare against a real SQL Server (run when QUILL_TEST_SERVER is set, as in
//! compare_live.rs). Databases are named QD_data_* and dropped afterwards.

use quillsql::catalog::{self, ObjectName};
use quillsql::data::compare::{DataComparison, compare};
use quillsql::data::mapping::{DataMappings, TableOverride};
use quillsql::data::store::RowKind;
use quillsql::data::{DataOpt, DataOptions, sync};
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::deploy;

struct Server {
    host: String,
    user: String,
    password: String,
}

fn server() -> Option<Server> {
    Some(Server {
        host: std::env::var("QUILL_TEST_SERVER").ok()?,
        user: std::env::var("QUILL_TEST_USER").ok()?,
        password: std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default(),
    })
}

impl Server {
    fn spec(&self, database: &str) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(&self.host, database, Auth::Sql { user: self.user.clone(), password: self.password.clone() });
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

    fn query(&self, database: &str, sql: &str) -> Vec<tiberius::Row> {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut conn = db::connect_async(&self.spec(database)).await.unwrap();
            db::query(&mut conn, sql).await.unwrap()
        })
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

fn run(server: &Server, a: &str, b: &str, opts: &DataOptions, user: &DataMappings) -> (DataComparison, catalog::Database) {
    let (s1, s2) = (server.spec(a), server.spec(b));
    let db1 = catalog::read_database(&s1).unwrap();
    let db2 = catalog::read_database(&s2).unwrap();
    (compare(&s1, &s2, &db1, &db2, opts, user, |_| true, 4, |_, _| {}).unwrap(), db2)
}

fn deploy_to(server: &Server, a: &str, b: &str, result: &DataComparison, db2: &catalog::Database, opts: &DataOptions, settings: sync::SyncSettings) -> String {
    let mut settings = settings;
    settings.source_connection = Some(server.spec(a));
    let script = sync::generate(result, db2, opts, &settings).unwrap();
    if let Err(e) = deploy::execute(&server.spec(b), &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    script.sql
}

const TYPES1: &str = "CREATE SCHEMA dev;
GO
CREATE SCHEMA g;
GO
CREATE TABLE dev.Orders (Id int PRIMARY KEY, Amount money, Code int, Day date, G char(36), X xml, V sql_variant, H hierarchyid,
    Name nvarchar(20) COLLATE Latin1_General_CI_AI, Big varbinary(max), Flag bit);
CREATE TABLE g.Same (Id int PRIMARY KEY, V nvarchar(10));
CREATE TABLE g.N (Id int PRIMARY KEY, Name nvarchar(10)) AS NODE;
CREATE TABLE g.E (W int) AS EDGE;
GO
INSERT INTO g.Same VALUES (1, N'a'), (2, N'b');
INSERT INTO g.N VALUES (1, N'x'), (2, N'y');
INSERT INTO g.E ($from_id, $to_id, W) SELECT a.$node_id, b.$node_id, 7 FROM g.N a, g.N b WHERE a.Id = 1 AND b.Id = 2;
INSERT INTO dev.Orders VALUES
 (1, 1.5, 5, '2020-01-02', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '<a x=\"1\">t</a>', CAST(CAST(1 AS int) AS sql_variant), '/1/', N'Café', CAST(REPLICATE(CAST(0x41 AS varbinary(max)), 2000000) AS varbinary(max)), 1),
 (2, 2.5, 6, '2020-01-03', NULL, NULL, CAST(CAST(1 AS int) AS sql_variant), '/2/', N'x', CAST(REPLICATE(CAST(0x41 AS varbinary(max)), 2000000) AS varbinary(max)) + 0x42, 0),
 (3, 3.5, 7, NULL, NULL, NULL, CAST(CAST(N'hi' AS nvarchar(10)) AS sql_variant), NULL, N'only', 0x01, 1);";

const TYPES2: &str = "CREATE SCHEMA g;
GO
CREATE TABLE dbo.Orders (Id int PRIMARY KEY, Total money, Code nvarchar(10), Day datetime, G uniqueidentifier, X nvarchar(max), V sql_variant, H hierarchyid,
    Name nvarchar(20) COLLATE Latin1_General_CI_AI, Big varbinary(max), Flag int, Extra int NULL);
CREATE TABLE g.Same (Id int PRIMARY KEY, V nvarchar(10));
CREATE TABLE g.N (Id int PRIMARY KEY, Name nvarchar(10)) AS NODE;
CREATE TABLE g.E (W int) AS EDGE;
GO
INSERT INTO g.Same VALUES (1, N'a'), (2, N'b');
INSERT INTO g.N VALUES (1, N'x'), (2, N'z');
INSERT INTO g.E ($from_id, $to_id, W) SELECT a.$node_id, b.$node_id, 8 FROM g.N a, g.N b WHERE a.Id = 1 AND b.Id = 2;
INSERT INTO dbo.Orders VALUES
 (1, 1.5, N'5', '2020-01-02T00:00:00', 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', N'<a x=\"1\">t</a>', CAST(CAST(1 AS int) AS sql_variant), '/1/', N'cafe', CAST(REPLICATE(CAST(0x41 AS varbinary(max)), 2000000) AS varbinary(max)), 1, NULL),
 (2, 2.5, N'8', '2020-01-03', NULL, NULL, CAST(CAST(1 AS bigint) AS sql_variant), '/2/', N'x', CAST(REPLICATE(CAST(0x41 AS varbinary(max)), 2000000) AS varbinary(max)) + 0x43, 0, NULL);";

#[test]
fn data_types_owners_and_column_mappings() {
    let Some(server) = server() else { return };
    server.create("QD_data_T1", TYPES1);
    server.create("QD_data_T2", TYPES2);
    let user = DataMappings {
        schemas: vec![("dev".into(), "dbo".into())],
        tables: vec![TableOverride { source: ObjectName::new("dev", "Orders"), columns: vec![("Amount".into(), "Total".into())], ..Default::default() }],
    };
    let opts = DataOptions::default_set();
    let (result, db2) = run(&server, "QD_data_T1", "QD_data_T2", &opts, &user);
    let get = |r: &DataComparison, n: &str| r.tables.iter().position(|t| t.pair.source.name == n).unwrap_or_else(|| panic!("{n}: {:?} {:?} {:?}", r.tables.iter().map(|t| t.pair.source.to_string()).collect::<Vec<_>>(), r.only_in_1, r.only_in_2));
    let orders = &result.tables[get(&result, "Orders")];
    assert!(orders.error.is_none(), "{:?}", orders.error);
    assert_eq!(orders.pair.target, ObjectName::new("dbo", "Orders"));
    let cols: Vec<&str> = orders.pair.columns.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(cols, ["Id", "Amount", "Code", "Day", "G", "X", "V", "H", "Name", "Big"]);
    let not_compared: Vec<&str> = orders.pair.notes.iter().filter(|n| n.not_compared()).map(|n| n.text.as_str()).collect();
    assert!(not_compared.iter().any(|t| t.contains("[Flag]") && t.contains("incomparable types bit and int")), "{not_compared:?}");
    assert!(not_compared.iter().any(|t| t.contains("[Extra]")), "{not_compared:?}");
    assert_eq!((orders.count(RowKind::Identical), orders.count(RowKind::Different), orders.count(RowKind::OnlyIn1)), (1, 1, 1), "{:?}", orders.column_differences);
    let differ: Vec<&str> = orders.pair.columns.iter().zip(&orders.column_differences).filter(|(_, n)| **n > 0).map(|(c, _)| c.source.as_str()).collect();
    assert_eq!(differ, ["Code", "V", "Big"]);
    // Graph tables: nodes by key, edges by $edge_id.
    let edges = &result.tables[get(&result, "E")];
    assert_eq!(edges.pair.key_name.as_deref(), Some("$edge_id"));
    assert_eq!(edges.count(RowKind::Different), 1, "{:?}", edges.error);
    assert_eq!(result.tables[get(&result, "N")].count(RowKind::Different), 1);

    let mut settings = sync::SyncSettings::new("a.QD_data_T1", "b.QD_data_T2", "QD_data_T2");
    settings.max_transaction_bytes = Some(1);
    let sql = deploy_to(&server, "QD_data_T1", "QD_data_T2", &result, &db2, &opts, settings);
    assert!(sql.contains("due to table being a SQL Graph EDGE table"), "{sql}");
    assert!(sql.contains("JSON_MODIFY("));
    let (after, _) = run(&server, "QD_data_T1", "QD_data_T2", &opts, &user);
    for t in &after.tables {
        assert!(t.identical(), "{} still differs: {:?} {:?} {:?}", t.pair.source, t.counts, t.column_differences, t.error);
    }
    // The checksum shortcut decides identical tables without reading rows.
    let (ck, _) = run(&server, "QD_data_T1", "QD_data_T2", &opts.clone().with(DataOpt::UseChecksumComparison), &user);
    let same = &ck.tables[get(&ck, "Same")];
    assert!(same.by_checksum && same.count(RowKind::Identical) == 2, "{:?}", same.counts);
    server.drop("QD_data_T1");
    server.drop("QD_data_T2");
}

const DEPLOY1: &str = "CREATE TABLE dbo.A (Id int PRIMARY KEY, B int NULL, Name nvarchar(20) INDEX IX_A_Name NONCLUSTERED);
CREATE TABLE dbo.B (Id int IDENTITY(1, 1) PRIMARY KEY NONCLUSTERED, A int NULL, V int, INDEX CX_B CLUSTERED (V));
CREATE TABLE dbo.Log (Id int IDENTITY PRIMARY KEY, Msg nvarchar(100));
CREATE TABLE dbo.Child (Id int PRIMARY KEY, A int REFERENCES dbo.A (Id));
GO
ALTER TABLE dbo.A ADD CONSTRAINT FK_A_B FOREIGN KEY (B) REFERENCES dbo.B (Id);
ALTER TABLE dbo.B ADD CONSTRAINT FK_B_A FOREIGN KEY (A) REFERENCES dbo.A (Id);
GO
CREATE TRIGGER dbo.TR_A ON dbo.A AFTER INSERT, UPDATE, DELETE AS INSERT INTO dbo.Log (Msg) VALUES (N'changed');
GO
CREATE TRIGGER TR_DDL ON DATABASE FOR CREATE_TABLE AS PRINT 'ddl';
GO
SET IDENTITY_INSERT dbo.B ON;
INSERT INTO dbo.B (Id, A, V) VALUES (1, NULL, 10), (2, NULL, 20), (5, NULL, 50);
SET IDENTITY_INSERT dbo.B OFF;
INSERT INTO dbo.A VALUES (1, 1, N'one'), (2, 2, N'two'), (3, 5, N'three');
UPDATE dbo.B SET A = Id WHERE Id < 3;
INSERT INTO dbo.Child VALUES (1, 1);";

#[test]
fn deployment_options_converge() {
    let Some(server) = server() else { return };
    server.create("QD_data_D1", DEPLOY1);
    // The target: rows missing, changed and extra, on both sides of the A ↔ B cycle.
    let target = DEPLOY1.replace("(1, NULL, 10), (2, NULL, 20), (5, NULL, 50)", "(1, NULL, 11), (2, NULL, 20), (7, NULL, 70)").replace(
        "(1, 1, N'one'), (2, 2, N'two'), (3, 5, N'three')",
        "(1, 1, N'uno'), (2, 2, N'two'), (4, 7, N'four')",
    );
    server.create("QD_data_D2", &target);
    let opts = DataOptions::default_set()
        .with(DataOpt::DisableTriggers)
        .with(DataOpt::DisableDdlTriggers)
        .with(DataOpt::DropConstraintsAndIndexes)
        .with(DataOpt::ReseedIdentity);
    let (result, db2) = run(&server, "QD_data_D1", "QD_data_D2", &opts, &DataMappings::default());
    assert_eq!(result.ddl_triggers, ["TR_DDL"]);
    let script = sync::generate(&result, &db2, &opts, &sync::SyncSettings::new("a.D1", "b.D2", "QD_data_D2")).unwrap();
    let warnings: Vec<&str> = script.warnings.iter().map(|w| w.message.as_str()).collect();
    assert!(warnings.contains(&"Circular dependency for the table or view [dbo].[A]"), "{warnings:?}");
    let sql = &script.sql;
    for expected in [
        "\r\nPRINT(N'Disable all DDL triggers for database')\r\nGO\r\nDISABLE TRIGGER ALL ON DATABASE\r\nGO\r\n",
        "ALTER TABLE [dbo].[A] DISABLE TRIGGER [TR_A]\r\n",
        "ALTER TABLE [dbo].[A] DROP CONSTRAINT [FK_A_B]\r\n",
        "PRINT(N'Drop constraint [",
        "DROP INDEX [IX_A_Name] ON [dbo].[A]\r\n",
        "DROP INDEX [CX_B] ON [dbo].[B]\r\n",
        "ALTER TABLE [dbo].[A] ENABLE TRIGGER [TR_A]\r\n",
        "ENABLE TRIGGER [TR_DDL] ON DATABASE\r\nGO\r\n",
    ] {
        assert!(sql.contains(expected), "missing {expected:?} in\n{sql}");
    }
    if let Err(e) = deploy::execute(&server.spec("QD_data_D2"), &script, |_, _| {}) {
        panic!("{e}\n\n{sql}");
    }
    let (after, _) = run(&server, "QD_data_D1", "QD_data_D2", &DataOptions::default_set(), &DataMappings::default());
    for t in after.tables.iter().filter(|t| t.pair.source.name != "Log") {
        assert!(t.identical(), "{} still differs: {:?} {:?}\n\n{sql}", t.pair.source, t.counts, t.error);
    }
    // The trigger did not fire (the one row is from creating the database); indexes, keys and
    // triggers are back.
    let n = |db: &str, q: &str| server.query(db, q)[0].get::<i32, _>(0).unwrap();
    assert_eq!(n("QD_data_D2", "SELECT COUNT(*) FROM dbo.Log"), 1);
    assert_eq!(n("QD_data_D2", "SELECT COUNT(*) FROM sys.indexes WHERE name IN ('IX_A_Name', 'CX_B')"), 2);
    assert_eq!(n("QD_data_D2", "SELECT COUNT(*) FROM sys.foreign_keys WHERE is_not_trusted = 0"), 3);
    assert_eq!(n("QD_data_D2", "SELECT COUNT(*) FROM sys.triggers WHERE is_disabled = 0"), 2);
    let ident = |db: &str| server.query(db, "SELECT CAST(IDENT_CURRENT('dbo.B') AS int)")[0].get::<i32, _>(0).unwrap();
    assert_eq!(ident("QD_data_D2"), ident("QD_data_D1"), "reseeded");
    server.drop("QD_data_D1");
    server.drop("QD_data_D2");
}

fn quill(server: &Server, args: &[&str]) -> (i32, String, String) {
    let mut all: Vec<String> = vec![
        format!("/server1:{}", server.host),
        format!("/username1:{}", server.user),
        format!("/password1:{}", server.password),
        "/trustServerCertificate1".into(),
        format!("/server2:{}", server.host),
        format!("/username2:{}", server.user),
        format!("/password2:{}", server.password),
        "/trustServerCertificate2".into(),
    ];
    all.extend(args.iter().map(|s| s.to_string()));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_quill")).arg("data").args(&all).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Console text with its line wrapping and repeated spaces undone.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn command_line_parity() {
    let Some(server) = server() else { return };
    let base = "CREATE TABLE dbo.T (Id int PRIMARY KEY, V nvarchar(10) NULL, B varbinary(10) NULL, D datetime NULL, F bit NULL);
CREATE UNIQUE INDEX UX_T_V ON dbo.T (V);
GO
INSERT INTO dbo.T VALUES (1, N'a', 0x01, '2025-04-03T13:00:00', 1), (2, N'b', NULL, NULL, 0);";
    server.create("QD_data_C1", base);
    server.create("QD_data_C2", &format!("{base}\nGO\nUPDATE dbo.T SET V = N'B\"' WHERE Id = 2; INSERT INTO dbo.T VALUES (3, NULL, NULL, NULL, NULL);"));
    let db = |a: &str, b: &str| vec![format!("/database1:{a}"), format!("/database2:{b}")];
    let call = |dbs: Vec<String>, extra: &[&str]| {
        let mut v: Vec<&str> = dbs.iter().map(String::as_str).collect();
        v.extend(extra);
        quill(&server, &v)
    };

    // Identical databases: 63, unless identical results are allowed.
    let (code, out, err) = call(db("QD_data_C1", "QD_data_C1"), &[]);
    assert_eq!(code, 63, "{out}{err}");
    assert!(flat(&err).contains("Nothing to compare. Either there are no tables/views"), "{err}");
    let (code, out, _) = call(db("QD_data_C1", "QD_data_C1"), &["/include:identical"]);
    assert_eq!(code, 0);
    assert!(out.contains("Table           [dbo].[T]                                           2  ==  ==  "), "{out}");
    assert_eq!(call(db("QD_data_C1", "QD_data_C1"), &["/allowIdenticalDatabases"]).0, 0);
    // Nothing compared: 60.
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &["/include:Table:^nomatch$"]).0, 60);
    // A bad comparison key: 34; a password without a user: 8.
    let (code, _, err) = call(db("QD_data_C1", "QD_data_C2"), &["/comparisonkeys:T:NoSuchIndex"]);
    assert_eq!(code, 34, "{err}");
    assert!(flat(&err).contains("Invalid comparison keys specified for [dbo].[T] 'NoSuchIndex'. Only a single key or unique index may be specified."), "{err}");
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &["/comparisonkeys:T:UX_T_V"]).0, 0);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_quill")).args(["data", "/database1:x", "/password1:p", "/database2:y"]).output().unwrap();
    assert_eq!(out.status.code(), Some(8));
    // An unknown option is an invalid value of the /options enum: 34.
    let (code, out, err) = call(db("QD_data_C1", "QD_data_C2"), &["/options:Default,NoSuchOption"]);
    assert_eq!(code, 34, "{err}");
    assert!(flat(&format!("{out}{err}")).contains("Invalid value for switch /options: NoSuchOption."), "{out}{err}");
    // Differences: summary, CSV export, assert.
    let dir = std::env::temp_dir().join(format!("QD_data_export_{}", std::process::id()));
    let dir_s = dir.to_string_lossy().to_string();
    let (code, out, err) = call(db("QD_data_C1", "QD_data_C2"), &[&format!("/export:{dir_s}"), "/options:Default"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.replace("\r\n", "\n").contains("\nFinished\n\nSummary Information\n"), "{out}");
    assert!(out.contains("Table           [dbo].[T]                                           1  <>  <>  "), "{out}");
    assert!(out.contains("Table           [dbo].[T]                                           1      <<  "), "{out}");
    let summary = std::fs::read_to_string(dir.join("Results Summary.csv")).unwrap();
    assert_eq!(
        summary,
        "\u{feff}\"Object Type\",\"Object Owner\",\"Object Name\",\"QD_data_C1 Only\",\"Different\",\"QD_data_C2 Only\",\"Object Name\",\"Object Owner\"\r\n\"Table\",\"dbo\",\"T\",\"0\",\"1\",\"1\",\"T\",\"dbo\"\r\n"
    );
    let rows = std::fs::read_to_string(dir.join("dbo.T.csv")).unwrap();
    assert_eq!(
        rows,
        "\u{feff}\"Type\",\"Id_k\",\"V_s\",\"V_t\",\"B_s\",\"B_t\",\"D_s\",\"D_t\",\"F_s\",\"F_t\"\r\n\
\"Different\",\"2\",\"b\",\"B\"\"\",\"<NULL>\",\"<NULL>\",\"<NULL>\",\"<NULL>\",\"False\",\"False\"\r\n\
\"In2\",\"3\",\"\",\"<NULL>\",\"\",\"<NULL>\",\"\",\"<NULL>\",\"\",\"<NULL>\"\r\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &["/assertidentical"]).0, 79);
    // A script split into transactions, a project written once.
    let script = std::env::temp_dir().join(format!("QD_data_{}.sql", std::process::id()));
    let project = std::env::temp_dir().join(format!("QD_data_{}.qdc", std::process::id()));
    let (s, p) = (script.to_string_lossy().to_string(), project.to_string_lossy().to_string());
    let (code, out, err) = call(db("QD_data_C1", "QD_data_C2"), &[&format!("/scriptfile:{s}"), "/MaxTransactionSizeMB:1", &format!("/outputProject:{p}"), "/force"]);
    assert_eq!(code, 0, "{out}{err}");
    let text = std::fs::read_to_string(&script).unwrap();
    // The classic data compare tool's command line prints row messages without counts.
    assert!(text.contains("PRINT(N'Update row in [dbo].[T]')") && text.contains("PRINT(N'Delete row from [dbo].[T]')"), "{text}");
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &[&format!("/outputProject:{p}")]).0, 73);
    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&project);
    // Deploy, then the databases are identical.
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &["/synchronize"]).0, 0);
    assert_eq!(call(db("QD_data_C1", "QD_data_C2"), &[]).0, 63);
    server.drop("QD_data_C1");
    server.drop("QD_data_C2");
}
