//! `quill schema` with scripts folders and filters, without a server.

use std::path::{Path, PathBuf};
use std::process::Command;

fn quill(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_quill")).arg("schema").args(args).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn folder(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quill-srccli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn s(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

const CUSTOMER: &str = "CREATE TABLE [dbo].[Customer]\r\n(\r\n[Id] [int] NOT NULL,\r\n[Name] [nvarchar] (50) NULL\r\n)\r\nGO\r\n\
ALTER TABLE [dbo].[Customer] ADD CONSTRAINT [PK_Customer] PRIMARY KEY CLUSTERED ([Id])\r\nGO\r\n";
const AUDIT: &str = "CREATE TABLE [audit].[Log]\r\n(\r\n[Id] [int] NOT NULL\r\n)\r\nGO\r\n";
const SCHEMA: &str = "CREATE SCHEMA [audit]\r\nAUTHORIZATION [dbo]\r\nGO\r\n";

fn filter_file(dir: &Path, table_expr: &str) -> PathBuf {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-16\" standalone=\"yes\"?>\n<NamedFilter version=\"1\" type=\"SQLCompareFilter\">\n  <FilterName>t</FilterName>\n  \
<Filter version=\"1\" type=\"DifferenceFilter\">\n    <FilterCaseSensitive>False</FilterCaseSensitive>\n    <Filters version=\"1\">\n      \
<None version=\"1\"><Include>True</Include><Expression>TRUE</Expression></None>\n      \
<Table version=\"1\"><Include>True</Include><Expression>{table_expr}</Expression></Table>\n    </Filters>\n  </Filter>\n</NamedFilter>"
    );
    let path = dir.join("f.scpf");
    let bytes: Vec<u8> = [0xFF, 0xFE].into_iter().chain(xml.encode_utf16().flat_map(u16::to_le_bytes)).collect();
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn compares_and_writes_scripts_folders() {
    let a = folder("a", &[("Tables/dbo.Customer.sql", CUSTOMER), ("Tables/audit.Log.sql", AUDIT), ("Security/Schemas/audit.sql", SCHEMA)]);
    let b = folder("b", &[("Tables/dbo.Customer.sql", &CUSTOMER.replace("(50)", "(80)"))]);
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b))]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[dbo].[Customer]") && out.contains("[audit].[Log]"), "{out}");

    // A filter that keeps only dbo tables, and the switches it refuses.
    let f = filter_file(&a, "@SCHEMA = 'dbo'");
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b)), &format!("/filter:{}", s(&f))]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[dbo].[Customer]") && !out.contains("[audit].[Log]"), "{out}");
    let (code, _) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b)), &format!("/filter:{}", s(&f)), "/exclude:Table"]);
    assert_eq!(code, 8);
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b)), &format!("/filter:{}", s(&f)), "/include:Table"]);
    assert_eq!(code, 8, "{out}");
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b)), &format!("/filter:{}", s(&f)), "/include:Identical"]);
    assert_eq!(code, 0, "{out}");

    // Deploying to folder b makes it like a.
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b)), "/synchronize"]);
    assert_eq!(code, 0, "{out}");
    assert!(b.join("Tables").join("audit.Log.sql").is_file());
    assert!(std::fs::read_to_string(b.join("Tables").join("dbo.Customer.sql")).unwrap().contains("(50)"));
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&b))]);
    assert_eq!(code, 63, "{out}");

    // makescripts from a scripts folder.
    let c = a.with_file_name(format!("{}-made", a.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&c);
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/makescripts:{}", s(&c))]);
    assert_eq!(code, 0, "{out}");
    assert!(c.join("RedGateDatabaseInfo.xml").is_file() && c.join("Security").join("Schemas").join("audit.sql").is_file());
    for d in [a, b, c] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn reads_and_writes_ssdt_projects() {
    let proj = r#"<?xml version="1.0" encoding="utf-8"?>
<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <PropertyGroup>
    <DSP>Microsoft.Data.Tools.Schema.Sql.Sql160DatabaseSchemaProvider</DSP>
    <DefaultFileStructure>BySchemaAndSchemaType</DefaultFileStructure>
  </PropertyGroup>
  <ItemGroup>
    <Build Include="dbo\Tables\Customer.sql" />
  </ItemGroup>
</Project>
"#;
    let ssdt = folder("ssdt", &[("Db.sqlproj", proj), ("dbo/Tables/Customer.sql", CUSTOMER), ("dbo/Tables/NotBuilt.sql", AUDIT)]);
    let a = folder("ssdt-src", &[("Tables/dbo.Customer.sql", CUSTOMER), ("Tables/audit.Log.sql", AUDIT), ("Security/Schemas/audit.sql", SCHEMA)]);
    // Only the project's Build items are read.
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&ssdt))]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[audit].[Log]") && !out.contains("[dbo].[Customer]"), "{out}");
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&ssdt)), "/synchronize"]);
    assert_eq!(code, 0, "{out}");
    let new_file = ssdt.join("audit").join("Tables").join("audit.Log.sql");
    assert!(new_file.is_file(), "{out}");
    let text = std::fs::read_to_string(ssdt.join("Db.sqlproj")).unwrap();
    assert!(text.contains(r#"<Build Include="audit\Tables\audit.Log.sql" />"#) && text.contains("Customer.sql"), "{text}");
    assert!(ssdt.join("RedGateDatabaseProperties.xml").is_file());
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&a)), &format!("/scripts2:{}", s(&ssdt))]);
    assert_eq!(code, 63, "{out}");
    for d in [a, ssdt] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn reports_scripts_folder_problems() {
    let (code, out) = quill(&["/scripts1:C:\\no\\such\\folder", "/scripts2:C:\\no\\such\\folder"]);
    assert_eq!(code, 60, "{out}");
    assert!(out.contains("The scripts folder 'C:\\no\\such\\folder' could not be found."), "{out}");

    let bad = folder("bad", &[("Tables/x.sql", "CREATE TABLE [dbo].[X] ( [a] int NOT NULL,, )\r\nGO\r\n"), ("Tables/y.sql", AUDIT)]);
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&bad)), &format!("/scripts2:{}", s(&bad))]);
    assert_eq!(code, 63, "{out}");
    assert!(out.contains("Warning: Invalid syntax found in file") && out.contains("at line 1"), "{out}");
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&bad)), &format!("/scripts2:{}", s(&bad)), "/options:Default,ThrowOnFileParseFailed"]);
    assert_eq!(code, 62, "{out}");
    assert!(out.contains("Errors occurred whilst parsing file"), "{out}");
    let (code, out) = quill(&[&format!("/scripts1:{}", s(&bad)), &format!("/scripts2:{}", s(&bad)), "/options:Default,ThrowOnFileParseFailed", "/ignoreParserErrors"]);
    assert_eq!(code, 63, "{out}");
    assert!(!out.contains("Invalid syntax"), "{out}");
    let _ = std::fs::remove_dir_all(bad);
}
