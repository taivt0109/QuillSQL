//! The backup switches of `quill schema` and `quill data`: exit codes and messages as SQL
//! Compare's command line gives them, without a server.

use std::process::{Command, Output};

fn quill(command: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quill")).arg(command).args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", " ").replace('\n', " ")
}

#[test]
fn backup_switches() {
    let dir = std::env::temp_dir().join(format!("quill-backup-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("missing.bak");
    let junk = dir.join("junk.bak");
    std::fs::write(&junk, b"not a backup at all").unwrap();
    let (missing, junk) = (missing.to_string_lossy().to_string(), junk.to_string_lossy().to_string());

    for command in ["schema", "data"] {
        let o = quill(command, &[&format!("/backup1:{missing}"), &format!("/backup2:{junk}")]);
        assert_eq!(o.status.code(), Some(74), "{}", text(&o));
        assert!(text(&o).contains("does not exist"), "{}", text(&o));

        let o = quill(command, &[&format!("/backup1:{junk}"), &format!("/backup2:{junk}")]);
        assert_eq!(o.status.code(), Some(65), "{}", text(&o));
        assert!(text(&o).contains("The backup reader reported the following error:"), "{}", text(&o));

        let o = quill(command, &[&format!("/backup1:{junk};{junk}"), "/backuppasswords1:a", &format!("/backup2:{junk}")]);
        assert_eq!(o.status.code(), Some(64), "{}", text(&o));
        assert!(text(&o).contains("The number of passwords specified should equal the number of backup files supplied."), "{}", text(&o));
    }
    // An encrypted the .sqb backup tool file and no (or a wrong) password.
    let sqb = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sqb/v5_aes128_lz.sqb");
    for extra in [None, Some("/backuppasswords1:wrong")] {
        let mut args = vec![format!("/backup1:{sqb}"), format!("/backup2:{junk}")];
        args.extend(extra.map(str::to_string));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = quill("schema", &args);
        assert_eq!(o.status.code(), Some(65), "{}", text(&o));
        assert!(text(&o).contains("No passwords were supplied or they were not correct for the encrypted backup files - 'Cannot decrypt SQB file..'"), "{}", text(&o));
    }
    // A backup cannot be deployed to, and /backupset needs /backup.
    let o = quill("schema", &["/db1:x", &format!("/backup2:{junk}"), "/synchronize"]);
    assert_eq!(o.status.code(), Some(8), "{}", text(&o));
    let o = quill("schema", &["/db1:x", "/db2:y", "/backupset1:x"]);
    assert_eq!(o.status.code(), Some(8), "{}", text(&o));
    let _ = std::fs::remove_dir_all(&dir);
}
