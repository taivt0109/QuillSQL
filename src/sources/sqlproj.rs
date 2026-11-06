//! SQL Server Data Tools database projects (`.sqlproj`) used as scripts folders: the project's
//! `Build` items are the files that hold objects, its `DSP` property names the SQL Server
//! version, and `DefaultFileStructure` decides where new objects go. Deploying to such a folder
//! adds and removes `Build` items.

use std::path::{Path, PathBuf};

use super::parse::{FileKind, ObjectKey};
use crate::schema::ObjectType;

/// Where SSDT puts new objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    /// `schema.name.sql` in the project folder.
    Flat,
    /// `<schema>\schema.name.sql`.
    BySchema,
    /// `Tables\schema.name.sql`.
    ByObjectType,
    /// `<schema>\Tables\schema.name.sql`.
    BySchemaAndObjectType,
}

#[derive(Clone, Debug)]
pub struct SqlProj {
    pub path: PathBuf,
    /// `Build` items, relative to the project folder, as written (backslashes).
    pub items: Vec<String>,
    /// SDK-style projects build every `.sql` file unless told otherwise.
    pub sdk: bool,
    pub removed: Vec<String>,
    pub structure: Structure,
    /// From `DSP`: 16 for Sql160; `None` when not set.
    pub version: Option<u32>,
    pub azure: bool,
}

fn child_text(doc: &roxmltree::Document, name: &str) -> Option<String> {
    doc.descendants().find(|n| n.is_element() && n.tag_name().name() == name).and_then(|n| n.text()).map(|t| t.trim().to_string())
}

/// The single `.sqlproj` in `dir`, if there is one.
pub fn find(dir: &Path) -> Option<SqlProj> {
    let mut found = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sqlproj")));
    let path = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let text = super::folder::decode(&std::fs::read(&path).ok()?, "UTF8");
    let doc = roxmltree::Document::parse(&text).ok()?;
    let sdk = doc.root_element().attribute("Sdk").is_some() || doc.descendants().any(|n| n.tag_name().name() == "Sdk");
    let items_of = |kind: &str, attr: &str| -> Vec<String> {
        doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == kind).filter_map(|n| n.attribute(attr)).map(str::to_string).collect()
    };
    let structure = match child_text(&doc, "DefaultFileStructure").as_deref() {
        Some("BySchema") => Structure::BySchema,
        Some("BySchemaType") => Structure::ByObjectType,
        Some("BySchemaAndSchemaType") => Structure::BySchemaAndObjectType,
        _ => Structure::Flat,
    };
    let dsp = child_text(&doc, "DSP").unwrap_or_default();
    let version = dsp
        .strip_prefix("Microsoft.Data.Tools.Schema.Sql.Sql")
        .and_then(|r| r.strip_suffix("DatabaseSchemaProvider"))
        .and_then(|v| v.strip_suffix('0'))
        .and_then(|v| v.parse::<u32>().ok());
    Some(SqlProj {
        path,
        items: items_of("Build", "Include"),
        removed: items_of("Build", "Remove"),
        sdk,
        structure,
        version,
        azure: dsp.contains("SqlAzure"),
    })
}

fn native(rel: &str) -> PathBuf {
    rel.split(['\\', '/']).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
}

impl SqlProj {
    /// The files the project builds. Wildcards in items are not expanded.
    pub fn sql_files(&self, dir: &Path, all: Vec<PathBuf>) -> Vec<PathBuf> {
        let rel = |p: &Path| p.strip_prefix(dir).unwrap_or(p).to_string_lossy().replace('/', "\\");
        let mut out: Vec<PathBuf> = if self.sdk {
            all.into_iter()
                .filter(|p| {
                    let r = rel(p).to_ascii_lowercase();
                    !r.starts_with("bin\\") && !r.starts_with("obj\\") && !self.removed.iter().any(|x| same(x, &r))
                })
                .collect()
        } else {
            Vec::new()
        };
        for item in self.items.iter().filter(|i| !i.contains('*')) {
            let p = dir.join(native(item));
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
        out.sort();
        out
    }

    /// Where a new object goes, relative to the project folder.
    pub fn ideal_path(&self, key: &ObjectKey) -> PathBuf {
        let (kind, schema, name) = key;
        let san = super::folder::sanitize;
        let file = if schema.is_empty() { format!("{}.sql", san(name)) } else { format!("{}.{}.sql", san(schema), san(name)) };
        let folder = match kind {
            FileKind::DdlTrigger => "Database Triggers",
            FileKind::Object(t) => match t {
                ObjectType::Table => "Tables",
                ObjectType::StoredProcedure => "Stored Procedures",
                ObjectType::View => "Views",
                ObjectType::Function => "Functions",
                ObjectType::Role | ObjectType::User | ObjectType::Schema => "Security",
                ObjectType::UserDefinedType => "User Defined Types",
                ObjectType::Synonym => "Synonyms",
                ObjectType::Sequence => "Sequences",
                ObjectType::ExtendedProperty => "Extended Properties",
                ObjectType::Assembly => "Assemblies",
                ObjectType::XmlSchemaCollection => "XML Schema Collections",
                ObjectType::Default => "Defaults",
                ObjectType::Rule => "Rules",
                ObjectType::DdlTrigger => "Database Triggers",
                ObjectType::PartitionFunction | ObjectType::PartitionScheme | ObjectType::FullTextCatalog | ObjectType::FullTextStoplist | ObjectType::SearchPropertyList => "Storage",
                ObjectType::MessageType | ObjectType::Contract | ObjectType::Queue | ObjectType::Service | ObjectType::Route | ObjectType::ServiceBinding | ObjectType::EventNotification => {
                    "Service Broker"
                }
                ObjectType::Certificate | ObjectType::AsymmetricKey | ObjectType::SymmetricKey | ObjectType::SecurityPolicy => "Security",
                ObjectType::ExternalDataSource | ObjectType::ExternalFileFormat | ObjectType::ExternalTable => "External Resources",
            },
        };
        let by_type = Path::new(folder).join(&file);
        match self.structure {
            Structure::Flat => PathBuf::from(file),
            Structure::BySchema if !schema.is_empty() => Path::new(&san(schema)).join(file),
            Structure::BySchema => PathBuf::from(file),
            Structure::ByObjectType => by_type,
            Structure::BySchemaAndObjectType if !schema.is_empty() => Path::new(&san(schema)).join(by_type),
            Structure::BySchemaAndObjectType => by_type,
        }
    }

    /// Adds `Build` items for new files and removes those of deleted files (SDK-style projects
    /// build every file anyway).
    pub fn update(&self, added: &[PathBuf], removed: &[PathBuf]) -> Result<(), String> {
        if self.sdk || (added.is_empty() && removed.is_empty()) {
            return Ok(());
        }
        let raw = std::fs::read(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let bom = raw.starts_with(&[0xEF, 0xBB, 0xBF]);
        let mut text = super::folder::decode(&raw, "UTF8");
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let win = |p: &PathBuf| p.to_string_lossy().replace('/', "\\");
        for p in removed {
            let item = win(p);
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            let kept: String = lines
                .into_iter()
                .filter(|l| {
                    let t = l.trim();
                    !(t.starts_with("<Build ") && t.ends_with("/>") && attr(t, "Include").is_some_and(|i| same(&i, &item)))
                })
                .collect();
            text = kept;
        }
        let new: Vec<String> = added.iter().map(win).filter(|i| !self.items.iter().any(|x| same(x, i))).collect();
        if !new.is_empty() {
            insert_items(&mut text, "Build", &new, nl).ok_or_else(|| format!("{} is not an MSBuild project", self.path.display()))?;
            // Each new folder gets a Folder item, as Visual Studio adds them.
            let mut folders: Vec<String> = Vec::new();
            for item in &new {
                let parts: Vec<&str> = item.split('\\').collect();
                for n in 1..parts.len() {
                    let f = format!("{}\\", parts[..n].join("\\"));
                    let known = text.contains(&format!("<Folder Include=\"{f}\"")) || self.items.iter().any(|x| x.to_ascii_lowercase().starts_with(&f.to_ascii_lowercase()));
                    if !known && !folders.iter().any(|x| same(x, &f)) {
                        folders.push(f);
                    }
                }
            }
            if !folders.is_empty() {
                insert_items(&mut text, "Folder", &folders, nl).ok_or_else(|| format!("{} is not an MSBuild project", self.path.display()))?;
            }
        }
        // MSBuild saves the project without a final line break.
        while text.ends_with('\n') || text.ends_with('\r') {
            text.pop();
        }
        let bytes = [if bom { &[0xEF, 0xBB, 0xBF][..] } else { &[][..] }, text.as_bytes()].concat();
        std::fs::write(&self.path, bytes).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// Adds `<kind Include="..." />` items to the first ItemGroup holding that kind, keeping it in
/// sorted order, or to a new ItemGroup before `</Project>`.
fn insert_items(text: &mut String, kind: &str, items: &[String], nl: &str) -> Option<()> {
    let tag = format!("<{kind} Include=");
    let line = |i: &str| format!("    <{kind} Include=\"{}\" />", i.replace('&', "&amp;").replace('"', "&quot;"));
    let Some(first) = text.find(&tag) else {
        let at = text.rfind("</Project>")?;
        let mut sorted = items.to_vec();
        sorted.sort_by_key(|x| x.to_lowercase());
        let lines: String = sorted.iter().map(|i| format!("{}{nl}", line(i))).collect();
        text.insert_str(at, &format!("  <ItemGroup>{nl}{lines}  </ItemGroup>{nl}"));
        return Some(());
    };
    let group_start = text[..first].rfind("<ItemGroup")?;
    let body_start = text[group_start..].find('>')? + group_start + 1;
    let group_end = text[body_start..].find("</ItemGroup>")? + body_start;
    let body = &text[body_start..group_end];
    let mut lines: Vec<String> = body.lines().filter(|l| !l.trim().is_empty()).map(|l| l.trim_end_matches('\r').to_string()).collect();
    let include = |l: &str| attr(l.trim(), "Include").unwrap_or_default().to_lowercase();
    for i in items {
        let l = line(i);
        let at = lines.iter().position(|x| x.trim().starts_with(&tag) && include(x) > i.to_lowercase()).unwrap_or_else(|| {
            lines.iter().rposition(|x| x.trim().starts_with(&tag)).map(|p| p + 1).unwrap_or(lines.len())
        });
        lines.insert(at, l);
    }
    let indent = text[..group_end].rsplit('\n').next().unwrap_or("").to_string();
    let rebuilt = format!("{nl}{}{nl}{indent}", lines.join(nl));
    text.replace_range(body_start..group_end, &rebuilt);
    Some(())
}

/// The value of `name="..."` in an XML start tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].replace("&quot;", "\"").replace("&amp;", "&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_updates_a_project() {
        let dir = std::env::temp_dir().join(format!("quill-sqlproj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dbo").join("Tables")).unwrap();
        std::fs::write(dir.join("dbo").join("Tables").join("T.sql"), "CREATE TABLE dbo.T (a int)").unwrap();
        std::fs::write(dir.join("loose.sql"), "CREATE TABLE dbo.L (a int)").unwrap();
        let proj = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<Project DefaultTargets=\"Build\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\r\n  <PropertyGroup>\r\n    <DSP>Microsoft.Data.Tools.Schema.Sql.Sql150DatabaseSchemaProvider</DSP>\r\n    <DefaultFileStructure>BySchemaAndSchemaType</DefaultFileStructure>\r\n  </PropertyGroup>\r\n  <ItemGroup>\r\n    <Build Include=\"dbo\\Tables\\T.sql\" />\r\n  </ItemGroup>\r\n</Project>\r\n";
        std::fs::write(dir.join("Db.sqlproj"), proj).unwrap();
        let p = find(&dir).unwrap();
        assert_eq!((p.version, p.structure, p.sdk), (Some(15), Structure::BySchemaAndObjectType, false));
        let files = p.sql_files(&dir, vec![dir.join("loose.sql"), dir.join("dbo").join("Tables").join("T.sql")]);
        assert_eq!(files, vec![dir.join("dbo").join("Tables").join("T.sql")]);
        let key = (FileKind::Object(ObjectType::View), "Sales".to_string(), "V".to_string());
        assert_eq!(p.ideal_path(&key), Path::new("Sales").join("Views").join("Sales.V.sql"));
        p.update(&[Path::new("Sales").join("Views").join("Sales.V.sql")], &[Path::new("dbo").join("Tables").join("T.sql")]).unwrap();
        let after = find(&dir).unwrap();
        assert_eq!(after.items, ["Sales\\Views\\Sales.V.sql"]);
        let text = std::fs::read_to_string(dir.join("Db.sqlproj")).unwrap();
        assert!(text.contains("    <Folder Include=\"Sales\\\" />\r\n    <Folder Include=\"Sales\\Views\\\" />"), "{text}");
        assert!(text.ends_with("</Project>"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
