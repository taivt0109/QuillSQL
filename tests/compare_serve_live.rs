//! `quill compare-serve` data operations of the Compare window against a real SQL Server (runs
//! when QUILL_TEST_SERVER is set, as in compare_live.rs).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use quillsql::db::{self, Auth, ConnectionSpec};
use serde_json::{Value, json};

struct Serve {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u64,
}

impl Serve {
    fn start() -> Serve {
        let mut child = Command::new(env!("CARGO_BIN_EXE_quill")).arg("compare-serve").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Serve { child, input, output, id: 0 }
    }

    fn call(&mut self, op: &str, mut fields: Value) -> Result<Value, String> {
        self.id += 1;
        fields["id"] = json!(self.id);
        fields["op"] = json!(op);
        writeln!(self.input, "{fields}").unwrap();
        self.input.flush().unwrap();
        loop {
            let mut line = String::new();
            self.output.read_line(&mut line).unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply.get("progress").is_some() {
                continue;
            }
            return match reply.get("error") {
                Some(e) => Err(e.as_str().unwrap_or_default().to_string()),
                None => Ok(reply["result"].clone()),
            };
        }
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn run(spec: &ConnectionSpec, sql: &str) {
    let rt = db::runtime().unwrap();
    rt.block_on(async {
        let mut c = db::connect_async(spec).await.unwrap();
        db::execute(&mut c, sql).await.unwrap_or_else(|e| panic!("{e}\n{sql}"));
    });
}

#[test]
fn data_results_operations() {
    let Ok(host) = std::env::var("QUILL_TEST_SERVER") else { return };
    let user = std::env::var("QUILL_TEST_USER").unwrap_or_default();
    let password = std::env::var("QUILL_TEST_PASSWORD").unwrap_or_default();
    let auth = if user.is_empty() { Auth::Windows } else { Auth::Sql { user: user.clone(), password: password.clone() } };
    let spec = |db: &str| {
        let mut s = ConnectionSpec::new(&host, db, auth.clone());
        s.trust_server_certificate = true;
        s
    };
    let (src, tgt) = (format!("QD_ui_Src_{}", std::process::id()), format!("QD_ui_Tgt_{}", std::process::id()));
    for (name, rows) in [(&src, "(1,'one'),(2,'two'),(3,'three'),(5,'five')"), (&tgt, "(1,'one'),(2,'deux'),(4,'four'),(5,'cinq')")] {
        run(&spec("master"), &format!("IF DB_ID('{name}') IS NOT NULL DROP DATABASE [{name}]; CREATE DATABASE [{name}];"));
        run(&spec(name), &format!("CREATE TABLE dbo.T (Id int PRIMARY KEY, Name nvarchar(400)); INSERT dbo.T VALUES {rows}; INSERT dbo.T VALUES (9, REPLICATE(N'x', 300));"));
    }
    let side = |db: &str| {
        json!({"type": "database", "server": host, "database": db, "authentication": if user.is_empty() { "windows" } else { "sql" },
               "user": user, "password": password, "encrypt": true, "trustServerCertificate": true})
    };
    let project = json!({"version": 1, "kind": "data", "source": side(&src), "target": side(&tgt), "options": ["DisableKeys", "OutputCommentHeader"]});
    let mut serve = Serve::start();
    let r = serve.call("data_compare", json!({"project": project})).unwrap();
    let s = r["session"].clone();
    assert_eq!(r["tables"][0]["counts"], json!([2, 2, 1, 1]), "{r}");
    let page = |serve: &mut Serve, kind: u64, sort: Value| serve.call("data_rows", json!({"session": s, "table": 0, "kind": kind, "start": 0, "count": 50, "sort": sort})).unwrap();

    // All rows: every row with a difference, in key order, each with its kind and position.
    let all = page(&mut serve, 4, Value::Null);
    let keys: Vec<String> = all["rows"].as_array().unwrap().iter().map(|r| r["source"].get(0).or(r["target"].get(0)).unwrap().as_str().unwrap().to_string()).collect();
    assert_eq!(all["total"], 4, "identical rows are not kept: {all}");
    assert_eq!(keys, ["2", "3", "4", "5"]);
    assert_eq!((all["rows"][1]["kind"].as_u64(), all["rows"][1]["row"].as_u64()), (Some(2), Some(0)));

    // Sorted by the target's Name, descending; the row only in the source sorts last.
    let sorted = page(&mut serve, 4, json!({"column": 1, "side": "target", "descending": true}));
    let names: Vec<&str> = sorted["rows"].as_array().unwrap().iter().map(|r| r["target"].get(1).and_then(Value::as_str).unwrap_or("-")).collect();
    assert_eq!(names, ["four", "deux", "cinq", "-"], "{sorted}");

    let found = serve.call("data_find", json!({"session": s, "table": 0, "kind": 4, "text": "CINQ", "start": 0})).unwrap();
    assert_eq!(found, 3);
    let none = serve.call("data_find", json!({"session": s, "table": 0, "kind": 4, "text": "CINQ", "start": 3, "backward": true})).unwrap();
    assert!(none.is_null());

    // Long values are cut in pages and whole in the value viewer.
    run(&spec(&tgt), "UPDATE dbo.T SET Name = REPLICATE(N'y', 300) WHERE Id = 9");
    let r = serve.call("data_compare", json!({"project": project})).unwrap();
    let s = r["session"].clone();
    let diff = serve.call("data_rows", json!({"session": s, "table": 0, "kind": 1, "start": 0, "count": 10})).unwrap();
    let long = diff["rows"].as_array().unwrap().iter().find(|r| r["source"][0] == "9").unwrap().clone();
    assert_eq!(long["target"][1].as_str().unwrap().chars().count(), 257);
    let full = serve.call("data_value", json!({"session": s, "table": 0, "kind": 1, "row": long["row"], "column": 1, "side": "target"})).unwrap();
    assert_eq!(full.as_str().unwrap(), "y".repeat(300));

    // CSV export and the HTML report.
    let dir = std::env::temp_dir().join(format!("quill-serve-live-{}", std::process::id()));
    serve.call("data_export", json!({"session": s, "folder": dir.to_string_lossy(), "kinds": [false, true, true, true]})).unwrap();
    let csv = std::fs::read_to_string(dir.join("dbo.T.csv")).unwrap();
    assert!(csv.contains("\"Different\",\"2\",\"two\",\"deux\""), "{csv}");
    assert!(dir.join("Results Summary.csv").exists());
    let html = dir.join("r.html");
    serve.call("data_report", json!({"session": s, "path": html.to_string_lossy()})).unwrap();
    assert!(std::fs::read_to_string(&html).unwrap().contains("cinq"));

    // Row selections reach the script: only row 0 of the rows only in the target is deleted.
    let script = serve
        .call("data_script", json!({"session": s, "selections": [{"table": 0, "kinds": [false, false, false, true], "rows": {"onlyIn2": {"include": [0]}}}]}))
        .unwrap();
    let sql = script["sql"].as_str().unwrap();
    assert!(sql.contains("DELETE") && !sql.contains("UPDATE") && !sql.contains("INSERT"), "{sql}");
    std::fs::remove_dir_all(dir).ok();
    drop(serve);
    for name in [&src, &tgt] {
        run(&spec("master"), &format!("ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{name}];"));
    }
}
