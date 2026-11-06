use serde_json::json;
use quillsql::complete::Schema;
use quillsql::info::{hover, occurrences};

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo",
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int", "identity": true}, {"name": "Name", "type": "nvarchar(100)", "nullable": true}]},
            {"schema": "dbo", "name": "usp_GetOrders", "kind": "procedure", "params": [{"name": "@CustomerId", "type": "int"}]}
        ]
    }))
    .unwrap()
}

fn at(sql: &str) -> (String, usize) {
    let i = sql.find('|').unwrap();
    (sql.replace('|', ""), sql[..i].encode_utf16().count())
}

fn hover_at(sql: &str) -> Option<String> {
    let (text, caret) = at(sql);
    hover(&text, caret, Some(&schema()))
}

#[test]
fn describes_objects_columns_aliases_and_variables() {
    assert_eq!(hover_at("select * from Cust|omers").unwrap(), "table dbo.Customers\n    CustomerId int NOT NULL IDENTITY\n    Name nvarchar(100) NULL");
    assert_eq!(hover_at("select c.Na|me from Customers c").unwrap(), "column dbo.Customers.Name nvarchar(100) NULL");
    assert_eq!(hover_at("select Na|me from Customers").unwrap(), "column dbo.Customers.Name nvarchar(100) NULL");
    assert!(hover_at("select c|.Name from Customers c").unwrap().starts_with("c = table dbo.Customers"));
    assert_eq!(hover_at("declare @d datetime2(3); select @d|").unwrap(), "@d datetime2(3)");
    assert_eq!(hover_at("exec usp_Get|Orders 1").unwrap(), "procedure dbo.usp_GetOrders (@CustomerId int)");
    assert_eq!(hover_at("sel|ect 1"), None);
}

#[test]
fn finds_occurrences() {
    let (text, caret) = at("declare @x int; set @x| = 1;\nGO\nselect @x");
    assert_eq!(occurrences(&text, caret).len(), 2);
    let (text, caret) = at("select c.Name from Customers c| where c.CustomerId = 1; select c from t");
    assert_eq!(occurrences(&text, caret).len(), 3);
    let (text, caret) = at("select * from Custo|mers; select 1 from Customers");
    let r = occurrences(&text, caret);
    assert_eq!((r.len(), r[0].start, r[0].length), (2, 14, 9));
}

#[test]
fn schema_context_lists_used_objects_or_all_tables() {
    let s = schema();
    let used = quillsql::info::schema_context("select * from Customers c; exec usp_GetOrders 1", &s, 10_000);
    assert_eq!(used, "Database Shop (default schema dbo).\nTABLE dbo.Customers (CustomerId int NOT NULL IDENTITY, Name nvarchar(100) NULL)\nPROCEDURE dbo.usp_GetOrders (@CustomerId int)\n");
    let all = quillsql::info::schema_context("", &s, 10_000);
    assert!(all.contains("TABLE dbo.Customers") && !all.contains("PROCEDURE"), "{all}");
    let short = quillsql::info::schema_context("", &s, 40);
    assert!(short.ends_with("... and 1 more objects not listed.\n"), "{short}");
}
