//! Project files and command-line sources against a real SQL Server (run when
//! QUILL_TEST_SERVER is set, as in compare_live.rs). Databases are named QD_fixprojects_* and
//! dropped afterwards.

use std::process::Command;

use quillsql::db::{self, Auth, ConnectionSpec};

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
            db::execute(&mut conn, script).await.unwrap();
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

    fn side(&self, n: u8, database: &str) -> Vec<String> {
        vec![format!("/server{n}:{};TrustServerCertificate=True", self.host), format!("/database{n}:{database}"), format!("/username{n}:{}", self.user), format!("/password{n}:{}", self.password)]
    }
}

fn quill(command: &str, args: &[String]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_quill")).arg(command).args(args).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

const TABLE: &str = "CREATE TABLE dbo.Customer (Id int NOT NULL CONSTRAINT PK_Customer PRIMARY KEY, Name nvarchar(50) NULL); \
INSERT dbo.Customer VALUES (1, N'Anh'), (2, N'Binh');";

#[test]
fn data_makescripts_writes_a_folder_that_reads_back() {
    let Some(server) = server() else { return };
    let db = "QD_fixprojects_MakeScripts";
    server.create(db, TABLE);
    let dir = std::env::temp_dir().join(format!("quill-fixprojects-mk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut args = server.side(1, db);
    args.push(format!("/makescripts:{}", dir.display()));
    let (code, out) = quill("data", &args);
    assert_eq!(code, 0, "{out}");
    assert!(dir.join("QuillDatabaseInfo.xml").is_file() && dir.join("Data").join("dbo.Customer_Data.sql").is_file(), "{out}");
    // The folder is a data source: identical to the database it came from.
    let mut args = vec![format!("/scripts1:{}", dir.display())];
    args.extend(server.side(2, db));
    let (code, out) = quill("data", &args);
    assert_eq!(code, 63, "{out}");
    server.drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
