//! The objects beyond tables and modules against a real SQL Server (see compare_live.rs for
//! the setup): CLR, XML schema collections, defaults and rules, partitioning, Service Broker,
//! security policies, temporal and graph tables, keys. Databases are named QD_objects_*.
//! CLR needs `clr enabled` on and `clr strict security` off on the server.

use quillsql::catalog;
use quillsql::db::{self, Auth, ConnectionSpec};
use quillsql::schema::{self, DiffKind, Mappings, ObjectType, Opt, Options, deploy};

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
}

fn clr(hex: &str) -> String {
    format!("0x{}", hex.trim())
}

fn objects() -> String {
    include_str!("fixtures/objects/objects.sql").replace("{CLR}", &clr(include_str!("fixtures/objects/clr.hex")))
}

/// The fixture with something changed in every kind of object.
fn changed() -> String {
    objects()
        .replace(&clr(include_str!("fixtures/objects/clr.hex")), &clr(include_str!("fixtures/objects/clr2.hex")))
        .replace("<xs:element name=\"order\" type=\"xs:string\"/>", "<xs:element name=\"order\" type=\"xs:int\"/>")
        .replace("CREATE DEFAULT dbo.Zero AS 0", "CREATE DEFAULT dbo.Zero AS 1")
        .replace("decimal(10, 2)", "decimal(12, 2)")
        .replace("VALUES (2020, 2021, 2022)", "VALUES (2021, 2022, 2023)")
        .replace("(Yr, Qty) WITH NORECOMPUTE", "(Yr)")
        .replace("LABEL = 'Confidential'", "LABEL = 'Public'")
        .replace("FOR CREATE_PROCEDURE AS SET NOCOUNT ON", "FOR CREATE_PROCEDURE, DROP_PROCEDURE AS SET NOCOUNT ON")
        .replace("[//Quill/Typed] SENT BY TARGET", "[//Quill/Typed] SENT BY ANY")
        .replace("MAX_QUEUE_READERS = 2", "MAX_QUEUE_READERS = 4")
        .replace("ADDRESS = 'TCP://10.0.0.1:4022'", "ADDRESS = 'TCP://10.0.0.2:4022'")
        .replace("ANONYMOUS = OFF", "ANONYMOUS = ON")
        .replace("WITH (STATE = ON)", "WITH (STATE = OFF)")
        .replace("HISTORY_RETENTION_PERIOD = 6 MONTHS", "HISTORY_RETENTION_PERIOD = 1 YEAR")
        .replace("Name nvarchar(50) NULL,\n    ValidFrom", "Name nvarchar(80) NULL,\n    ValidFrom")
        .replace("GRANT SELECT (Qty) ON", "GRANT SELECT (Yr) ON")
        .replace("N'CLR helpers', 'ASSEMBLY'", "N'CLR', 'ASSEMBLY'")
        .replace("N'Queue', 'SCHEMA'", "N'The queue', 'SCHEMA'")
        .replace("EXEC sp_addextendedproperty N'MS_Description', N'Route', 'ROUTE', N'QuillRoute';\n", "")
}

fn options() -> Options {
    Options::default_set().without(Opt::IgnoreFileGroups).with(Opt::IgnoreCertificatesAndCryptoKeys)
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
    if let Ok(dir) = std::env::var("QUILL_TEST_SCRIPTS") {
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{from}-{to}.sql")), &script.sql);
    }
    if let Err(e) = deploy::execute(&server.spec(to), &script, |_, _| {}) {
        panic!("{e}\n\n{}", script.sql);
    }
    let after = catalog::read_database(&server.spec(to)).unwrap();
    let c = schema::compare(&db1, &after, opts, &Mappings::default());
    let left: Vec<_> = c.differences.iter().filter(|d| d.kind != DiffKind::Equal).collect();
    assert!(left.is_empty(), "still different after deployment: {left:#?}\n\n{}", script.sql);
    script
}

#[test]
fn objects_are_read() {
    let Some(server) = server() else { return };
    let a = "QD_objects_Read";
    server.create(a, &objects());
    let m = catalog::read_database(&server.spec(a)).unwrap();
    assert_eq!(m.assemblies.len(), 1);
    assert_eq!(m.assemblies[0].permission_set, "SAFE");
    let add_one = m.routines.iter().find(|r| r.name == "AddOne").unwrap();
    assert_eq!((add_one.parameters.len(), add_one.returns.as_deref()), (1, Some("int")));
    assert_eq!(m.xml_schema_collections.len(), 1);
    assert_eq!((m.defaults.len(), m.rules.len()), (1, 1));
    assert_eq!(m.partition_functions[0].boundaries, vec!["2020", "2021", "2022"]);
    // ALL TO sets the NEXT USED filegroup too.
    assert_eq!(m.partition_schemes[0].filegroups.len(), 5);
    let orders = m.table("dbo", "Orders").unwrap();
    assert_eq!(orders.statistics.len(), 1);
    assert_eq!(orders.column("Secret").unwrap().sensitivity.as_ref().unwrap().label.as_deref(), Some("Confidential"));
    assert_eq!(m.ddl_triggers[0].events, vec!["CREATE_PROCEDURE"]);
    assert_eq!((m.message_types.len(), m.contracts.len(), m.queues.len(), m.services.len()), (2, 1, 1, 1));
    assert_eq!(m.contracts[0].messages.len(), 2);
    assert_eq!(m.queues[0].activation.as_ref().unwrap().procedure, "[dbo].[Activate]");
    assert_eq!((m.routes.len(), m.service_bindings.len(), m.event_notifications.len()), (1, 1, 1));
    assert_eq!(m.security_policies[0].predicates[0].function, "[rls].[fn_tenant]");
    assert_eq!(m.table("dbo", "Knows").unwrap().edge_constraints.len(), 1);
    assert!(m.principals.iter().any(|p| p.kind == "APPLICATION_ROLE" && p.default_schema.as_deref() == Some("dbo")));
    assert!(m.permissions.iter().any(|p| p.column.as_deref() == Some("Qty")));
    assert!(m.permissions.iter().any(|p| p.class == "TYPE" && p.object.as_deref() == Some("Amount")));
    assert!(m.permissions.iter().any(|p| p.class == "SERVICE"));
    // Every object matches itself.
    let c = schema::compare(&m, &m, &options(), &Mappings::default());
    assert!(c.identical());
    server.drop(a);
}

#[test]
fn objects_from_empty_and_back() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_objects_Full1", "QD_objects_Full2");
    server.create(a, &objects());
    server.create(b, "SELECT 1");
    let s = converge(&server, a, b, &options());
    for text in [
        "CREATE ASSEMBLY [QuillClr]",
        "AS EXTERNAL NAME [QuillClr].[QuillFunctions].[Hello]",
        "CREATE XML SCHEMA COLLECTION [dbo].[OrderSchema]",
        "CREATE PARTITION FUNCTION [pfYears]",
        "CREATE MESSAGE TYPE [//Quill/Order]",
        "CREATE SECURITY POLICY [rls].[TenantPolicy]",
        "SYSTEM_VERSIONING = ON (HISTORY_TABLE = [dbo].[VersionedHistory]",
        "ADD SENSITIVITY CLASSIFICATION TO [dbo].[Orders].[Secret]",
        "CREATE STATISTICS [ST_Orders_Yr]",
        "CONNECTION ([dbo].[Person] TO [dbo].[Person])",
    ] {
        assert!(s.sql.contains(text), "{text}\n{}", s.sql);
    }
    // Everything goes again.
    server.create(b, "SELECT 1");
    converge(&server, b, a, &options());
    server.drop(a);
    server.drop(b);
}

#[test]
fn objects_change_both_ways() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_objects_Chg1", "QD_objects_Chg2");
    server.create(a, &objects());
    server.create(b, &changed());
    let s = converge(&server, b, a, &options());
    assert!(s.sql.contains("ALTER ASSEMBLY [QuillClr]"), "{}", s.sql);
    assert!(s.sql.contains("SPLIT RANGE (2023)") && s.sql.contains("MERGE RANGE (2020)"), "{}", s.sql);
    assert!(s.sql.contains("Unbinding types from columns"), "{}", s.sql);
    server.create(a, &objects());
    converge(&server, a, b, &options());
    server.drop(a);
    server.drop(b);
}

#[test]
fn versioning_on_and_off() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_objects_Ver1", "QD_objects_Ver2");
    let versioned = "CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL,
        ValidFrom datetime2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL CONSTRAINT DF_T_From DEFAULT SYSUTCDATETIME(),
        ValidTo datetime2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL CONSTRAINT DF_T_To DEFAULT CONVERT(datetime2, '9999-12-31 23:59:59.9999999'),
        PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo))
        WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.T_History));
        GO
        INSERT INTO dbo.T (Id, Name) VALUES (1, N'one');";
    let plain = "CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL);
        GO
        INSERT INTO dbo.T (Id, Name) VALUES (1, N'one');";
    // On: the period columns come with their PERIOD, versioning starts.
    server.create(a, versioned);
    server.create(b, plain);
    let s = converge(&server, a, b, &options());
    assert!(s.sql.contains("PERIOD FOR SYSTEM_TIME") && s.sql.contains("SET (SYSTEM_VERSIONING = ON"), "{}", s.sql);
    // Off: versioning stops, the history table goes.
    server.create(a, versioned);
    server.create(b, plain);
    let s = converge(&server, b, a, &options());
    assert!(s.sql.contains("SET (SYSTEM_VERSIONING = OFF)") && s.sql.contains("DROP TABLE [dbo].[T_History]"), "{}", s.sql);
    // A rebuild (the IDENTITY can't be added in place) keeps the data of both tables.
    let changed = versioned
        .replace("Id int NOT NULL CONSTRAINT PK_T", "Id int IDENTITY(1, 1) NOT NULL CONSTRAINT PK_T")
        .replace("INSERT INTO dbo.T (Id, Name) VALUES (1, N'one');", "INSERT INTO dbo.T (Name) VALUES (N'one');");
    server.create(a, versioned);
    server.create(b, &changed);
    let s = converge(&server, b, a, &options());
    assert!(s.sql.contains("Rebuilding [dbo].[T]"), "{}", s.sql);
    server.drop(a);
    server.drop(b);
}

#[test]
fn keys_and_certificates_are_described() {
    let Some(server) = server() else { return };
    let (a, b) = ("QD_objects_Key1", "QD_objects_Key2");
    server.create(
        a,
        "CREATE CERTIFICATE QuillCert ENCRYPTION BY PASSWORD = 'Str0ng!Pass1' WITH SUBJECT = 'Quill', EXPIRY_DATE = '20300101';
        GO
        CREATE SYMMETRIC KEY QuillKey WITH ALGORITHM = AES_256 ENCRYPTION BY PASSWORD = 'Str0ng!Pass1';",
    );
    server.create(b, "SELECT 1");
    let opts = Options::default_set();
    let (s, _) = script(&server, a, b, &opts);
    assert!(s.sql.contains("-- Certificate [QuillCert]") && s.sql.contains("-- This certificate cannot be synchronized."), "{}", s.sql);
    assert!(s.sql.contains("-- Symmetric key [QuillKey]"), "{}", s.sql);
    deploy::execute(&server.spec(b), &s, |_, _| {}).unwrap();
    // Dropping them is scripted.
    let (s, _) = script(&server, b, a, &opts);
    assert!(s.sql.contains("DROP CERTIFICATE [QuillCert]") && s.sql.contains("DROP SYMMETRIC KEY [QuillKey]"), "{}", s.sql);
    let c = schema::compare(
        &catalog::read_database(&server.spec(a)).unwrap(),
        &catalog::read_database(&server.spec(b)).unwrap(),
        &opts.clone().with(Opt::IgnoreCertificatesAndCryptoKeys),
        &Mappings::default(),
    );
    assert!(!c.differences.iter().any(|d| matches!(d.object_type, ObjectType::Certificate | ObjectType::SymmetricKey)));
    server.drop(a);
    server.drop(b);
}

#[test]
fn objects_round_trip_through_scripts_folders() {
    use quillsql::sources::folder;
    let Some(server) = server() else { return };
    let a = "QD_objects_Folder";
    server.create(a, &objects());
    let live = catalog::read_database(&server.spec(a)).unwrap();
    let dir = std::env::temp_dir().join(format!("{a}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let opts = options();
    folder::write_all(&live, &dir, &opts).unwrap();
    let f = folder::read(&dir).unwrap();
    assert!(f.messages.is_empty(), "{:?}", f.messages);
    for (x, y) in [(&f.database, &live), (&live, &f.database)] {
        let c = schema::compare(x, y, &opts, &Mappings::default());
        let left: Vec<_> = c.differences.iter().filter(|d| d.kind != DiffKind::Equal).collect();
        assert!(left.is_empty(), "{left:#?}");
    }
    // A deployment from the folder to an empty database gives the same objects.
    let b = "QD_objects_FromFolder";
    server.create(b, "SELECT 1");
    let target = catalog::read_database(&server.spec(b)).unwrap();
    let c = schema::compare(&f.database, &target, &opts, &Mappings::default());
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: "SERIALIZABLE".into(),
        source: dir.display().to_string(),
        target: b.into(),
        target_database: b.into(),
        target_major_version: target.server.major_version,
    };
    let s = deploy::generate(&c, &f.database, &target, &opts, &Mappings::default(), &settings).unwrap();
    deploy::execute(&server.spec(b), &s, |_, _| {}).unwrap_or_else(|e| panic!("{e}\n\n{}", s.sql));
    let after = catalog::read_database(&server.spec(b)).unwrap();
    let c = schema::compare(&live, &after, &opts, &Mappings::default());
    let left: Vec<_> = c.differences.iter().filter(|d| d.kind != DiffKind::Equal).collect();
    assert!(left.is_empty(), "{left:#?}");
    let _ = std::fs::remove_dir_all(&dir);
    server.drop(a);
    server.drop(b);
}
