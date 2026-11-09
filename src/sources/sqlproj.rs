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
    /// `Reference` items with a `HintPath`: CLR assemblies the project holds as `.dll` files.
    pub clr: Vec<ClrReference>,
}

/// A `Reference` item's `HintPath` (relative, as written) and, when the item has all of
/// `SqlPermissionSet`, `SqlAssembly` and `SqlOwnerName`, its permission set.
#[derive(Debug, Clone)]
pub struct ClrReference {
    pub hint_path: String,
    pub permission_set: Option<String>,
}

fn child_text(doc: &roxmltree::Document, name: &str) -> Option<String> {
    doc.descendants().find(|n| n.is_element() && n.tag_name().name() == name).and_then(|n| n.text()).map(|t| t.trim().to_string())
}

/// The SQL Server major version of a schema provider (`…Sql160DatabaseSchemaProvider` → 16).
pub fn dsp_version(dsp: &str) -> Option<u32> {
    dsp.strip_prefix("Microsoft.Data.Tools.Schema.Sql.Sql")
        .and_then(|r| r.strip_suffix("DatabaseSchemaProvider"))
        .and_then(|v| v.strip_suffix('0'))
        .and_then(|v| v.parse::<u32>().ok())
}

/// The permission set a database project gives an assembly file: from the nearest project in
/// the file's folder or above, the `Reference` whose `HintPath` is the file's path relative to
/// the project. One of SAFE, EXTERNAL_ACCESS and UNSAFE (as written); None otherwise.
pub fn assembly_permission_set(file: &Path) -> Option<String> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if let Some(p) = find(d) {
            let root = p.path.parent()?;
            let rel = file.strip_prefix(root).ok()?.to_string_lossy().replace('/', "\\");
            let set = p.clr.iter().find(|r| r.hint_path == rel)?.permission_set.clone()?;
            return ["SAFE", "EXTERNAL_ACCESS", "UNSAFE"].contains(&set.as_str()).then_some(set);
        }
        dir = d.parent();
    }
    None
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
    let version = dsp_version(&dsp);
    // Item metadata may be child elements or attributes.
    let meta = |n: roxmltree::Node, key: &str| -> Option<String> {
        n.attribute(key).map(str::to_string).or_else(|| n.children().find(|c| c.is_element() && c.tag_name().name() == key).map(|c| c.text().unwrap_or_default().trim().to_string()))
    };
    let clr = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Reference")
        .filter_map(|n| {
            let hint_path = meta(n, "HintPath")?;
            let complete = ["SqlPermissionSet", "SqlAssembly", "SqlOwnerName"].iter().all(|k| meta(n, k).is_some());
            Some(ClrReference { hint_path, permission_set: meta(n, "SqlPermissionSet").filter(|_| complete) })
        })
        .collect();
    Some(SqlProj {
        path,
        clr,
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
        // The project's CLR assemblies are read with its scripts.
        for item in self.items.iter().filter(|i| !i.contains('*')).chain(self.clr.iter().map(|r| &r.hint_path)) {
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
    /// `assemblies` gives the assembly behind each `.dll` among them: a `.dll` is a `None` item
    /// with a `Reference` holding its CLR metadata, as the classic compare tool adds it.
    pub fn update(&self, added: &[PathBuf], removed: &[PathBuf], assemblies: &[(PathBuf, &crate::catalog::Assembly)]) -> Result<(), String> {
        if self.sdk || (added.is_empty() && removed.is_empty()) {
            return Ok(());
        }
        let dll = |p: &PathBuf| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll"));
        let (added_dlls, added): (Vec<PathBuf>, Vec<PathBuf>) = added.iter().cloned().partition(dll);
        let (removed_dlls, removed): (Vec<PathBuf>, Vec<PathBuf>) = removed.iter().cloned().partition(dll);
        let (added, removed) = (&added[..], &removed[..]);
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
        for p in &removed_dlls {
            let item = win(p);
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            let mut kept = String::new();
            let mut in_reference = false;
            for l in lines {
                let t = l.trim();
                if in_reference {
                    in_reference = !t.starts_with("</Reference>");
                    continue;
                }
                if t.starts_with("<None ") && t.ends_with("/>") && attr(t, "Include").is_some_and(|i| same(&i, &item)) {
                    continue;
                }
                if t.starts_with("<Reference ") && attr(t, "Include").is_some_and(|i| i.eq_ignore_ascii_case(&stem)) {
                    in_reference = !t.ends_with("/>");
                    continue;
                }
                kept.push_str(l);
            }
            text = kept;
        }
        let new_dlls: Vec<&PathBuf> = added_dlls.iter().filter(|p| !self.clr.iter().any(|r| same(&r.hint_path, &win(p)))).collect();
        if !new_dlls.is_empty() {
            let names: Vec<String> = new_dlls.iter().map(|p| win(p)).collect();
            insert_items(&mut text, "None", &names, nl).ok_or_else(|| format!("{} is not an MSBuild project", self.path.display()))?;
            let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
            let mut group = format!("  <ItemGroup>{nl}");
            for p in new_dlls {
                let Some((_, a)) = assemblies.iter().find(|(r, _)| r == p) else { continue };
                let name = a.clr_name.split(',').next().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&a.name).to_string();
                group.push_str(&format!("    <Reference Include=\"{}\">{nl}", esc(&name)));
                for (k, v) in [
                    ("HintPath", win(p)),
                    ("SqlAssembly", name.clone()),
                    ("IsModelAware", "True".into()),
                    ("GenerateSqlClrDdl", "False".into()),
                    ("SqlOwnerName", a.owner.clone().unwrap_or_else(|| "dbo".into())),
                    ("SqlPermissionSet", a.permission_set.clone()),
                ] {
                    group.push_str(&format!("      <{k}>{}</{k}>{nl}", esc(&v)));
                }
                group.push_str(&format!("    </Reference>{nl}"));
            }
            group.push_str(&format!("  </ItemGroup>{nl}"));
            let at = text.rfind("</Project>").ok_or_else(|| format!("{} is not an MSBuild project", self.path.display()))?;
            text.insert_str(at, &group);
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
        p.update(&[Path::new("Sales").join("Views").join("Sales.V.sql")], &[Path::new("dbo").join("Tables").join("T.sql")], &[]).unwrap();
        let after = find(&dir).unwrap();
        assert_eq!(after.items, ["Sales\\Views\\Sales.V.sql"]);
        let text = std::fs::read_to_string(dir.join("Db.sqlproj")).unwrap();
        assert!(text.contains("    <Folder Include=\"Sales\\\" />\r\n    <Folder Include=\"Sales\\Views\\\" />"), "{text}");
        assert!(text.ends_with("</Project>"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn assemblies_as_project_references() {
        let dir = std::env::temp_dir().join(format!("quill-sqlproj-clr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Assemblies")).unwrap();
        std::fs::write(dir.join("Assemblies").join("Ext.dll"), b"MZ").unwrap();
        std::fs::write(dir.join("Assemblies").join("Bare.dll"), b"MZ").unwrap();
        let proj = "<Project xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\r\n  <ItemGroup>\r\n    <Reference Include=\"Ext\">\r\n      <HintPath>Assemblies\\Ext.dll</HintPath>\r\n      <SqlAssembly>Ext</SqlAssembly>\r\n      <SqlOwnerName>dbo</SqlOwnerName>\r\n      <SqlPermissionSet>EXTERNAL_ACCESS</SqlPermissionSet>\r\n    </Reference>\r\n    <Reference Include=\"Bare\">\r\n      <HintPath>Assemblies\\Bare.dll</HintPath>\r\n      <SqlPermissionSet>UNSAFE</SqlPermissionSet>\r\n    </Reference>\r\n  </ItemGroup>\r\n</Project>\r\n";
        std::fs::write(dir.join("Db.sqlproj"), proj).unwrap();
        // All three of SqlPermissionSet, SqlAssembly and SqlOwnerName, or no permission set.
        assert_eq!(assembly_permission_set(&dir.join("Assemblies").join("Ext.dll")).as_deref(), Some("EXTERNAL_ACCESS"));
        assert_eq!(assembly_permission_set(&dir.join("Assemblies").join("Bare.dll")), None);
        let p = find(&dir).unwrap();
        assert!(p.sql_files(&dir, Vec::new()).contains(&dir.join("Assemblies").join("Ext.dll")));
        let a = crate::catalog::Assembly { name: "New".into(), clr_name: "new, version=1.0.0.0".into(), permission_set: "UNSAFE".into(), ..Default::default() };
        let new = Path::new("Assemblies").join("New.dll");
        p.update(std::slice::from_ref(&new), &[Path::new("Assemblies").join("Ext.dll")], &[(new.clone(), &a)]).unwrap();
        std::fs::write(dir.join(&new), b"MZ").unwrap();
        let text = std::fs::read_to_string(dir.join("Db.sqlproj")).unwrap();
        assert!(text.contains("<None Include=\"Assemblies\\New.dll\" />") && !text.contains("Ext"), "{text}");
        assert!(!text.contains("<Build Include=\"Assemblies"), "{text}");
        assert_eq!(assembly_permission_set(&dir.join(&new)).as_deref(), Some("UNSAFE"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
