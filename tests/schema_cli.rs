//! `quill schema` as a replacement for SQLCompare.exe: switches, exit codes, console output,
//! reports and project files. The tests that need SQL Server run only when QUILL_TEST_SERVER is
//! set (see compare_live.rs); their databases are named `QD_schemacli_*`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use quillsql::db::{self, Auth, ConnectionSpec};

fn quill(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quill")).arg("schema").args(args).output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Messages are wrapped at the console width; this undoes it.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quill-schemacli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parse_errors_have_sql_compare_codes_and_messages() {
    let o = quill(&[]);
    assert_eq!(code(&o), 64);
    assert!(flat(&stdout(&o)).contains("You are required to specify at least one of the following parameters: /auth, /database1"), "{}", stdout(&o));
    assert!(flat(&stdout(&o)).contains("Please use quill schema /? or quill schema /verbose /? for more information."));
    let o = quill(&["/db1:a", "/bogus"]);
    assert_eq!(code(&o), 64);
    assert!(flat(&stdout(&o)).contains("Invalid command-line argument: /bogus."));
    assert_eq!(code(&quill(&["/db1:a", "/db1:b"])), 3);
    assert_eq!(code(&quill(&["/db1:a", "/sync:now"])), 11);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/q", "/v"])), 8);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/rt:Xml"])), 8);
    let o = quill(&["/db1:a", "/db2:b", "/r:x.xml", "/rt:Pdf"]);
    assert_eq!(code(&o), 34);
    assert!(flat(&stdout(&o)).contains("Invalid value for switch /reporttype: Pdf."));
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/options:Default,NotAnOption"])), 34);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/outputwidth:20"])), 32);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/outputwidth:wide"])), 34);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/outputwidth:12345678901"])), 33);
    assert_eq!(code(&quill(&["/db1:a", "/sn2:x.snp", "/sync"])), 8);
    assert_eq!(code(&quill(&["/db1:a", "/e2", "/db2:b"])), 8);
    assert_eq!(code(&quill(&["/db1:a", "/db2:b", "/include:Gadget"])), 34);
    // Help ignores the other switches.
    let o = quill(&["/?", "/db1:a"]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("Options\n=======") && stdout(&o).contains("/reportAllObjectsWithDifferences (Alias: /rad)"));
    assert!(stdout(&quill(&["/?", "/v"])).contains("Exit Codes"));
}

#[test]
fn run_errors_have_sql_compare_codes() {
    let dir = temp_dir("errors");
    let p = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let o = quill(&["/db1:a"]);
    assert_eq!(code(&o), 64);
    assert!(flat(&stderr(&o)).contains("You must specify one of the following argument(s): /project, /database2"), "{}", stderr(&o));
    std::fs::write(dir.join("p.qsc"), serde_json::to_vec(&quillsql::project::Project::new(quillsql::project::ProjectKind::Schema)).unwrap()).unwrap();
    let o = quill(&["/project", &p("p.qsc"), "/exclude:Table"]);
    assert_eq!(code(&o), 8);
    assert!(flat(&stderr(&o)).contains("The /project switch cannot be used in conjunction with the /exclude switch."));
    let o = quill(&["/project", &p("p.qsc"), "/include:Table"]);
    assert_eq!(code(&o), 8);
    assert!(flat(&stderr(&o)).contains("except for /include:identical and /include:staticdata"));
    assert_eq!(code(&quill(&["/db1:a", "/p1:x", "/db2:b"])), 8);
    // A scripts folder that is not there.
    assert_eq!(code(&quill(&["/scripts1", &p("none"), "/db2:b"])), 60);
    assert_eq!(code(&quill(&["/project", &p("missing.qsc")])), 74);
    std::fs::write(dir.join("bad.qsc"), "not json").unwrap();
    assert_eq!(code(&quill(&["/project", &p("bad.qsc")])), 65);
    // Only QuillSQL projects open.
    std::fs::write(dir.join("other.scp"), "<Project />").unwrap();
    let o = quill(&["/project", &p("other.scp")]);
    assert_eq!(code(&o), 65);
    assert!(flat(&stderr(&o)).contains("only QuillSQL projects (.qsc, .qdc) can be opened"), "{}", stderr(&o));
    std::fs::write(dir.join("out.txt"), "x").unwrap();
    let o = quill(&["/db1:a", "/db2:b", "/out", &p("out.txt")]);
    assert_eq!(code(&o), 74);
    assert!(flat(&stderr(&o)).contains("because the file already exists"));
    // /argfile replaces the command line.
    std::fs::write(dir.join("args.xml"), "<commandline><database1>a</database1><bogus/></commandline>").unwrap();
    assert_eq!(code(&quill(&["/argfile", &p("args.xml")])), 78);
    assert_eq!(code(&quill(&["/argfile", &p("args.xml"), "/db2:b"])), 8);
    std::fs::remove_dir_all(dir).unwrap();
}

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

    fn run(&self, database: &str, script: &str) {
        let rt = db::runtime().unwrap();
        rt.block_on(async {
            let mut conn = db::connect_async(&self.spec(database)).await.unwrap();
            for batch in script.split("\nGO\n").filter(|b| !b.trim().is_empty()) {
                db::execute(&mut conn, batch).await.unwrap_or_else(|e| panic!("{e}\n{batch}"));
            }
        });
    }

    fn create(&self, name: &str, script: &str) {
        let q = db::quote(name);
        self.run("master", &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END; CREATE DATABASE {q};", db::literal(name)));
        self.run(name, script);
    }

    fn drop(&self, name: &str) {
        let q = db::quote(name);
        self.run("master", &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {q} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {q}; END;", db::literal(name)));
    }

    /// Connection switches for side `n`.
    fn side(&self, n: u8, database: &str) -> Vec<String> {
        vec![
            format!("/s{n}:{}", self.host),
            format!("/db{n}:{database}"),
            format!("/u{n}:{}", self.user),
            format!("/p{n}:{}", self.password),
            format!("/tsc{n}"),
        ]
    }
}

fn quill_owned(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quill")).arg("schema").args(args).output().unwrap()
}

const SOURCE: &str = "CREATE TABLE dbo.Customer (Id int NOT NULL PRIMARY KEY, Name nvarchar(100) NOT NULL, Email varchar(200) NULL)\nGO\n\
CREATE VIEW dbo.CustomerNames AS SELECT Id, Name FROM dbo.Customer\nGO\n\
CREATE PROCEDURE dbo.GetCustomer @Id int AS SELECT * FROM dbo.Customer WHERE Id = @Id\nGO\n\
CREATE TABLE dbo.Same (Id int NOT NULL)\nGO\n";

const TARGET: &str = "CREATE TABLE dbo.Customer (Id int NOT NULL PRIMARY KEY, Name nvarchar(50) NOT NULL)\nGO\n\
CREATE PROCEDURE dbo.Obsolete AS SELECT 1\nGO\n\
CREATE TABLE dbo.Same (Id int NOT NULL)\nGO\n";

fn read_utf16(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xFE], "{} has no UTF-16 byte order mark", path.display());
    let units: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).unwrap()
}

#[test]
fn live_compare_report_project_and_deploy() {
    let Some(s) = server() else { return };
    let (src, tgt) = ("QD_schemacli_Src", "QD_schemacli_Tgt");
    s.create(src, SOURCE);
    s.create(tgt, TARGET);
    let dir = temp_dir("live");
    let p = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let both = |extra: &[&str]| -> Vec<String> { s.side(1, src).into_iter().chain(s.side(2, tgt)).chain(extra.iter().map(|x| x.to_string())).collect() };

    // Summary layout.
    let o = quill_owned(&both(&["/outputwidth:100"]));
    let out = stdout(&o);
    assert_eq!(code(&o), 0, "{out}{}", stderr(&o));
    assert!(out.contains("Summary Information\n") && out.contains(&format!("DB1 = {}.{src}", s.host)), "{out}");
    assert!(out.contains(&format!("{:<24}{:<67}{:<4}{:<4}", "Object type", "Name", "DB1", "DB2")), "{out}");
    assert!(out.contains(&format!("{:<24}{:<67}<>  <>  ", "Table", "[dbo].[Customer]")), "{out}");
    assert!(out.contains(&format!("{:<24}{:<67}>>      ", "View", "[dbo].[CustomerNames]")), "{out}");
    assert!(out.contains(&format!("{:<24}{:<67}    <<  ", "StoredProcedure", "[dbo].[Obsolete]")), "{out}");
    assert!(!out.contains("[dbo].[Same]"), "identical objects are not listed: {out}");
    assert!(out.contains("Please note: Only included differences are displayed above and only included differences are deployed"));

    // /include:Identical lists identical objects; /verbose shows the line diff.
    let out = stdout(&quill_owned(&both(&["/include:Identical", "/verbose"])));
    assert!(out.contains("==  ==") && out.contains("[dbo].[Same]"), "{out}");
    assert!(out.contains("Details - comparing") && out.contains("Table [dbo].[Customer]"), "{out}");
    assert!(out.lines().any(|l| l.starts_with(">> ") && l.contains("[nvarchar] (100)")) && out.lines().any(|l| l.starts_with("<< ") && l.contains("(50)")), "{out}");
    // /quiet prints nothing after the title.
    let out = stdout(&quill_owned(&both(&["/quiet"])));
    assert!(!out.contains("Summary Information"), "{out}");

    // Identical selection: 63, unless /include:Identical or /assertidentical.
    let o = quill_owned(&both(&["/include:Table:Same"]));
    assert_eq!(code(&o), 63, "{}", stderr(&o));
    assert!(flat(&stderr(&o)).contains("The selected objects are identical or no objects have been selected"));
    assert_eq!(code(&quill_owned(&both(&["/include:Table:Same", "/include:Identical"]))), 0);
    assert_eq!(code(&quill_owned(&both(&["/assertidentical"]))), 79);
    assert_eq!(code(&quill_owned(&both(&["/assertidentical", "/include:Table:Same"]))), 0);

    // XML report: SQL Compare's layout; /rad adds the deselected differences.
    let o = quill_owned(&both(&["/report", &p("r.xml"), "/include:Table"]));
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let x = read_utf16(&dir.join("r.xml"));
    assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"utf-16\"?><comparison direction=\"1to2\" timestamp=\""), "{x}");
    assert!(x.contains(&format!("<datasource type=\"live\" id=\"1\"><server>{}</server><database>{src}</database></datasource>", s.host)));
    assert!(x.contains("<difference objecttype=\"table\" status=\"different\" fqn=\"table-[dbo].[Customer]\">"), "{x}");
    assert!(x.contains("<line type=\"different\">") && !x.contains("fqn=\"view-"), "{x}");
    roxmltree::Document::parse(&x.replace("utf-16", "utf-8")).unwrap();
    assert_eq!(code(&quill_owned(&both(&["/report", &p("r.xml"), "/include:Table"]))), 74, "existing report without /force");
    quill_owned(&both(&["/report", &p("r.xml"), "/include:Table", "/rad", "/force"]));
    let x = read_utf16(&dir.join("r.xml"));
    assert!(x.contains("fqn=\"view-[dbo].[CustomerNames]\"") && x.contains("status=\"onlyin2\""), "{x}");

    for (file, ty, probe) in [("r.html", "Html", "Before (target)"), ("c.html", "Classic", "only"), ("r.xls", "Excel", "<?mso-application progid=\"Excel.Sheet\"?>")] {
        let o = quill_owned(&both(&["/report", &p(file), "/reporttype", ty]));
        assert_eq!(code(&o), 0, "{ty}: {}", stderr(&o));
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(text.contains(probe), "{ty}: {text}");
    }
    quill_owned(&both(&["/report", &p("r.xlsx"), "/rt:Excel"]));
    assert!(std::fs::read(dir.join("r.xlsx")).unwrap().starts_with(b"PK\x03\x04"));

    // /outputProject writes a .qsc that loads back.
    let o = quill_owned(&both(&["/outputProject", &p("run.qsc"), "/options:Default,IgnoreCollations"]));
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let qsc = std::fs::read_to_string(dir.join("run.qsc")).unwrap();
    assert!(qsc.contains("\"IgnoreCollations\"") && !qsc.contains(&s.password), "{qsc}");
    assert_eq!(code(&quill_owned(&both(&["/outputProject", &p("run.qsc")]))), 73);
    let o = quill_owned(&[format!("/project:{}", p("run.qsc")), format!("/p1:{}", s.password), format!("/p2:{}", s.password)]);
    assert_eq!(code(&o), 0, "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains("[dbo].[Customer]"));

    // /out writes the console output to a file; /empty2 compares with nothing.
    let o = quill_owned(&s.side(1, src).into_iter().chain(["/empty2".to_string(), "/out".into(), p("out.txt"), "/sf".into(), p("create.sql")]).collect::<Vec<_>>());
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let out = std::fs::read_to_string(dir.join("out.txt")).unwrap();
    assert!(out.contains("DB2 = Empty") && out.contains("[dbo].[Same]"), "{out}");
    assert!(std::fs::read_to_string(dir.join("create.sql")).unwrap().contains("CREATE TABLE"));

    // Deploy, then the databases are identical.
    let o = quill_owned(&both(&["/sync", "/sf", &p("deploy.sql"), "/showWarnings"]));
    assert_eq!(code(&o), 0, "{}{}", stdout(&o), stderr(&o));
    assert_eq!(&std::fs::read(dir.join("deploy.sql")).unwrap()[..3], &[0xEF, 0xBB, 0xBF]);
    assert_eq!(code(&quill_owned(&both(&[]))), 63);
    assert_eq!(code(&quill_owned(&both(&["/assertidentical"]))), 0);

    s.drop(src);
    s.drop(tgt);
    std::fs::remove_dir_all(dir).unwrap();
}
