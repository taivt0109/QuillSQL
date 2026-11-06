//! Scripts folders at a revision in source control, checked out with the version control
//! command line (git, Subversion or Mercurial) into a temporary folder.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A checked-out folder; deleted when dropped.
pub struct Checkout {
    /// The scripts folder inside the checkout.
    pub dir: PathBuf,
    root: PathBuf,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn temp_root() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("quill-scm-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Runs a command and returns its standard output.
fn run(cmd: &mut Command) -> Result<String, String> {
    let name = cmd.get_program().to_string_lossy().to_string();
    let out = cmd.output().map_err(|e| format!("could not run {name}: {e} (is it installed and on PATH?)"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("{name} failed: {err}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `HEAD` and `LATEST` mean the newest revision.
fn is_latest(revision: &str) -> bool {
    let r = revision.trim();
    r.is_empty() || r.eq_ignore_ascii_case("HEAD") || r.eq_ignore_ascii_case("LATEST")
}

fn is_url(s: &str) -> bool {
    s.contains("://") || s.starts_with("git@")
}

/// Checks out `location` (a folder in a working copy, or a repository URL with an optional
/// `#path` inside it) at `revision`.
pub fn checkout(location: &str, revision: &str) -> Result<Checkout, String> {
    let (repo, sub) = match location.split_once('#') {
        Some((r, s)) if is_url(r) => (r.to_string(), s.trim_matches(['/', '\\']).to_string()),
        _ => (location.to_string(), String::new()),
    };
    if is_url(&repo) {
        if repo.ends_with(".git") || repo.starts_with("git@") || !repo.contains("svn") {
            return git_url(&repo, &sub, revision);
        }
        return svn(&repo, &sub, revision);
    }
    let path = Path::new(&repo);
    if !path.is_dir() {
        return Err(format!("The scripts folder '{}' could not be found.", path.display()));
    }
    if let Ok(top) = run(Command::new("git").arg("-C").arg(path).args(["rev-parse", "--show-toplevel"])) {
        let prefix = run(Command::new("git").arg("-C").arg(path).args(["rev-parse", "--show-prefix"]))?;
        return git_local(Path::new(&top), prefix.trim_end_matches('/'), revision);
    }
    if run(Command::new("hg").arg("-R").arg(path).arg("root")).is_ok() {
        return hg(path, revision);
    }
    if run(Command::new("svn").arg("info").arg(path)).is_ok() {
        return svn(&repo, "", revision);
    }
    Err(format!("{} is not in a git, Subversion or Mercurial working copy", path.display()))
}

/// Writes the tree of `revision` (under `prefix`) to a new folder without touching the
/// repository's index or working tree.
fn git_local(top: &Path, prefix: &str, revision: &str) -> Result<Checkout, String> {
    let rev = if is_latest(revision) { "HEAD".to_string() } else { revision.trim().to_string() };
    let root = temp_root()?;
    let tree = if prefix.is_empty() { format!("{rev}^{{tree}}") } else { format!("{rev}:{prefix}") };
    let index = root.join(".quill-index");
    let files = root.join("files");
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C").arg(top).env("GIT_INDEX_FILE", &index).args(args);
        c
    };
    run(&mut git(&["read-tree", &tree])).map_err(|e| format!("revision {rev} of {}: {e}", top.join(prefix).display()))?;
    let mut target = files.to_string_lossy().replace('\\', "/");
    target.push('/');
    run(&mut git(&["checkout-index", "-a", "-f", &format!("--prefix={target}")]))?;
    let _ = std::fs::remove_file(&index);
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    Ok(Checkout { dir: files, root })
}

fn git_url(url: &str, sub: &str, revision: &str) -> Result<Checkout, String> {
    let root = temp_root()?;
    let clone = root.join("repo");
    run(Command::new("git").args(["clone", "--quiet", "--no-checkout", url]).arg(&clone))?;
    let inner = git_local(&clone, sub, revision)?;
    // Keep the clone with the files so both go when the checkout is dropped.
    let files = root.join("files");
    std::fs::rename(&inner.dir, &files).map_err(|e| e.to_string())?;
    Ok(Checkout { dir: files, root })
}

fn svn(url: &str, sub: &str, revision: &str) -> Result<Checkout, String> {
    let root = temp_root()?;
    let files = root.join("files");
    let src = if sub.is_empty() { url.to_string() } else { format!("{}/{sub}", url.trim_end_matches('/')) };
    let rev = if is_latest(revision) { "HEAD".to_string() } else { revision.trim().to_string() };
    run(Command::new("svn").args(["export", "--quiet", "--force", "-r", &rev, &src]).arg(&files))?;
    Ok(Checkout { dir: files, root })
}

fn hg(path: &Path, revision: &str) -> Result<Checkout, String> {
    let top = run(Command::new("hg").arg("-R").arg(path).arg("root"))?;
    let top = PathBuf::from(top);
    let prefix = path.canonicalize().ok().and_then(|p| top.canonicalize().ok().and_then(|t| p.strip_prefix(&t).ok().map(Path::to_path_buf))).unwrap_or_default();
    let rev = if is_latest(revision) { "tip".to_string() } else { revision.trim().to_string() };
    let root = temp_root()?;
    let files = root.join("files");
    let mut c = Command::new("hg");
    c.arg("-R").arg(&top).args(["archive", "-r", &rev, "--type", "files"]);
    if !prefix.as_os_str().is_empty() {
        c.arg("-I").arg(format!("path:{}", prefix.to_string_lossy().replace('\\', "/")));
    }
    run(c.arg(&files))?;
    Ok(Checkout { dir: files.join(prefix), root })
}

/// The repository location in a scripts folder location file (`/scriptsfolderxml`): an XML
/// fragment whose `RepositoryUrl`/`Url`/`Path`-like elements give the repository and the folder
/// in it, or a plain path or URL.
pub fn location_from_xml(text: &str) -> Option<String> {
    let text = text.trim_start_matches('\u{feff}').trim();
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string());
    };
    let find = |names: &[&str]| {
        doc.descendants()
            .filter(|n| n.is_element())
            .find(|n| names.iter().any(|w| n.tag_name().name().eq_ignore_ascii_case(w)))
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let repo = find(&["RepositoryUrl", "RepositoryURL", "Url", "Repository", "WorkingFolder", "Location"])?;
    match find(&["Path", "FolderPath", "ScriptsFolder", "ScriptFolder", "Folder"]) {
        Some(p) if is_url(&repo) => Some(format!("{repo}#{p}")),
        Some(p) => Some(Path::new(&repo).join(p).to_string_lossy().to_string()),
        None => Some(repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_files() {
        assert_eq!(location_from_xml("C:\\repo\\db\r\n").as_deref(), Some("C:\\repo\\db"));
        assert_eq!(
            location_from_xml("<ScriptFolderLocation><RepositoryUrl>https://example.com/r.git</RepositoryUrl><Path>db/schema</Path></ScriptFolderLocation>").as_deref(),
            Some("https://example.com/r.git#db/schema")
        );
        assert_eq!(location_from_xml("<x/>"), None);
    }

    #[test]
    fn checks_out_a_git_revision() {
        let dir = std::env::temp_dir().join(format!("quill-scm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db/Tables")).unwrap();
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&dir).args(args).output().map(|o| o.status.success()).unwrap_or(false);
        if !git(&["init", "-q"]) {
            return;
        }
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("db/Tables/dbo.T.sql"), "CREATE TABLE [dbo].[T] ([a] [int] NULL)\r\nGO\r\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        std::fs::write(dir.join("db/Tables/dbo.T.sql"), "CREATE TABLE [dbo].[T] ([a] [int] NULL, [b] [int] NULL)\r\nGO\r\n").unwrap();
        git(&["commit", "-q", "-am", "two"]);
        let co = checkout(&dir.join("db").to_string_lossy(), "HEAD~1").unwrap();
        let text = std::fs::read_to_string(co.dir.join("Tables/dbo.T.sql")).unwrap();
        assert!(!text.contains("[b]"));
        let latest = checkout(&dir.join("db").to_string_lossy(), "HEAD").unwrap();
        assert!(std::fs::read_to_string(latest.dir.join("Tables/dbo.T.sql")).unwrap().contains("[b]"));
        let root = co.root.clone();
        drop(co);
        assert!(!root.exists());
        // The working tree is untouched.
        assert!(std::fs::read_to_string(dir.join("db/Tables/dbo.T.sql")).unwrap().contains("[b]"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
