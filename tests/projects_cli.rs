//! `quill data` and `quill schema` switches, project files and sources, without a server:
//! both sides are scripts folders.

use std::path::{Path, PathBuf};
use std::process::Command;

fn quill(command: &str, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_quill")).arg(command).args(args).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

const CUSTOMER: &str = "CREATE TABLE [dbo].[Customer]\r\n(\r\n[Id] [int] NOT NULL,\r\n[Name] [nvarchar] (50) NULL\r\n)\r\nGO\r\n\
ALTER TABLE [dbo].[Customer] ADD CONSTRAINT [PK_Customer] PRIMARY KEY CLUSTERED ([Id])\r\nGO\r\n";

/// A scripts folder with dbo.Customer and its rows.
fn folder(name: &str, rows: &[(i32, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quill-prjcli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Tables")).unwrap();
    std::fs::create_dir_all(dir.join("Data")).unwrap();
    std::fs::write(dir.join("Tables").join("dbo.Customer.sql"), CUSTOMER).unwrap();
    let inserts: String = rows.iter().map(|(id, n)| format!("INSERT INTO [dbo].[Customer] ([Id], [Name]) VALUES ({id}, N'{n}')\r\n")).collect();
    std::fs::write(dir.join("Data").join("dbo.Customer_Data.sql"), inserts).unwrap();
    dir
}

fn s(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

#[test]
fn data_options_aliases_repetition_and_rules() {
    let a = folder("opt-a", &[(1, "Anh"), (2, "Binh")]);
    let b = folder("opt-b", &[(1, "Anh")]);
    let (s1, s2) = (format!("/scripts1:{}", s(&a)), format!("/scripts2:{}", s(&b)));
    // Short aliases are known values; /options may be repeated.
    let (code, out) = quill("data", &[&s1, &s2, "/o:k,t", "/options:fbc"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("Unrecognised"), "{out}");
    let (code, out) = quill("data", &[&s1, &s2, "/options:NoSuchOption"]);
    assert_eq!(code, 34, "{out}");
    // /exportidenticaltables needs /export; /casesensitive is deprecated.
    let (code, out) = quill("data", &[&s1, &s2, "/eit"]);
    assert_eq!(code, 8, "{out}");
    let (code, out) = quill("data", &[&s1, &s2, "/cs"]);
    assert!(code == 0 && out.contains("/casesensitive has been deprecated"), "{code} {out}");
    // A comma list of object types.
    let (code, out) = quill("data", &[&s1, &s2, "/include:Table,View:Customer"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn data_empty2_and_sdc_output_projects() {
    let a = folder("empty-a", &[(1, "Anh"), (2, "Binh")]);
    let s1 = format!("/scripts1:{}", s(&a));
    let script = a.with_file_name(format!("quill-prjcli-empty-{}.sql", std::process::id()));
    let _ = std::fs::remove_file(&script);
    let (code, out) = quill("data", &[&s1, "/empty2", &format!("/scriptfile:{}", s(&script))]);
    assert_eq!(code, 0, "{out}");
    let sql = std::fs::read_to_string(&script).unwrap();
    assert!(sql.contains("INSERT INTO [dbo].[Customer]") && sql.contains("N'Binh'"), "{sql}");
    let (code, _) = quill("data", &[&s1, "/empty2", "/synchronize"]);
    assert_eq!(code, 8);

    // /outputProject writes a SQL Data Compare project for any sides, and it loads again.
    let b = folder("empty-b", &[(1, "Anh")]);
    let sdc = a.with_file_name(format!("quill-prjcli-{}.sdc", std::process::id()));
    let _ = std::fs::remove_file(&sdc);
    let (code, out) = quill("data", &[&s1, &format!("/scripts2:{}", s(&b)), &format!("/outputProject:{}", s(&sdc))]);
    assert_eq!(code, 0, "{out}");
    let p = quillsql::project::Project::load(&sdc).unwrap();
    assert!(p.source.is_scripts() && p.target.is_scripts() && p.target.path == s(&b), "{p:?}");
    let (code, out) = quill("data", &[&s1, &format!("/scripts2:{}", s(&b)), &format!("/outputProject:{}", s(&sdc))]);
    assert!(code == 73 && out.contains("already exists. Use /force to overwrite it."), "{code} {out}");
    // The project runs again.
    let (code, out) = quill("data", &[&format!("/project:{}", s(&sdc))]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn revisions_need_source_control() {
    let (code, out) = quill("schema", &["/server1:.", "/database1:A", "/revision1:5", "/server2:.", "/database2:B"]);
    assert!(code == 69 && out.contains("requires SQL Source Control"), "{code} {out}");
    let (code, out) = quill("schema", &["/sourcecontrol1", "/revision1:HEAD", "/server2:.", "/database2:B"]);
    assert!(code == 66 && out.contains("script folder location XML"), "{code} {out}");
}
