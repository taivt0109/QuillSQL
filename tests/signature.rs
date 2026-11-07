use serde_json::json;
use quillsql::complete::Schema;
use quillsql::signature::signature_at;

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo"],
        "objects": [
            {"schema": "dbo", "name": "usp_Orders", "kind": "procedure", "params": [
                {"name": "@CustomerId", "type": "int"}, {"name": "@From", "type": "date"}, {"name": "@Count", "type": "int", "output": true}]},
            {"schema": "dbo", "name": "fn_Tax", "kind": "scalar_function", "params": [{"name": "@amount", "type": "money"}, {"name": "@rate", "type": "decimal(5,2)"}]}
        ]
    })).unwrap()
}

/// (name, active parameter) at the `|`.
fn at(sql: &str) -> Option<(String, usize)> {
    let caret = sql.find('|').unwrap();
    let text = sql.replace('|', "");
    signature_at(&text, text[..caret].encode_utf16().count(), Some(&schema())).map(|s| (s.name, s.active))
}

#[test]
fn built_in_functions_and_the_argument_at_the_caret() {
    assert_eq!(at("select dateadd(|"), Some(("DATEADD".into(), 0)));
    assert_eq!(at("select dateadd(day, 1, |"), Some(("DATEADD".into(), 2)));
    // A call inside the arguments is its own.
    assert_eq!(at("select dateadd(day, len(x|), getdate())"), Some(("LEN".into(), 0)));
    assert_eq!(at("select dateadd(day, len(x), |"), Some(("DATEADD".into(), 2)));
    assert_eq!(at("select isnull(a, 'x,y|"), None);
    assert_eq!(at("select x from t where (|"), None);
}

#[test]
fn the_database_functions_and_procedures() {
    assert_eq!(at("select dbo.fn_Tax(1, |"), Some(("dbo.fn_Tax".into(), 1)));
    assert_eq!(at("exec dbo.usp_Orders |"), Some(("dbo.usp_Orders".into(), 0)));
    assert_eq!(at("exec dbo.usp_Orders 1, |"), Some(("dbo.usp_Orders".into(), 1)));
    // By name, whatever its position.
    assert_eq!(at("exec dbo.usp_Orders @Count = |"), Some(("dbo.usp_Orders".into(), 2)));
    assert_eq!(at("exec dbo.usp_Or|"), None);
    let s = signature_at("select dbo.fn_Tax(", 18, Some(&schema())).unwrap();
    assert_eq!(s.params, ["@amount money", "@rate decimal(5,2)"]);
}
