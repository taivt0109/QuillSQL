//! Gives `quill.exe` the QuillSQL icon and version details on Windows.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo/quillsql.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/logo/quillsql.ico").set("ProductName", "QuillSQL").set("FileDescription", "QuillSQL T-SQL engine");
        if let Err(e) = resource.compile() {
            // A missing resource compiler only costs the icon, not the build.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
