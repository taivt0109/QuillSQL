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

#[test]
fn finds_the_column_at_the_caret_with_its_description() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo",
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int"},
                {"name": "Status", "type": "tinyint", "default": "((0))", "description": "0 = new\n1 = active\n2 = closed, kept for the reports"}]},
            {"schema": "dbo", "name": "Orders", "kind": "table", "columns": [{"name": "CustomerId", "type": "int"}, {"name": "Total", "type": "money"}]}
        ]
    }))
    .unwrap();
    let column = |sql: &str| {
        let (text, caret) = at(sql);
        quillsql::info::column_at(&text, caret, &s).map(|(o, c)| format!("{}.{}.{}", o.schema, o.name, c.name))
    };
    assert_eq!(column("select c.Sta|tus from Customers c").as_deref(), Some("dbo.Customers.Status"));
    assert_eq!(column("select Sta|tus from Customers").as_deref(), Some("dbo.Customers.Status"));
    assert_eq!(column("select dbo.Customers.Sta|tus from dbo.Customers").as_deref(), Some("dbo.Customers.Status"));
    assert_eq!(column("select o.Customer|Id from Customers c join Orders o on o.CustomerId = c.CustomerId").as_deref(), Some("dbo.Orders.CustomerId"));
    // In two tables and not qualified: which one is not known.
    assert_eq!(column("select Customer|Id from Customers c join Orders o on o.CustomerId = c.CustomerId"), None);
    // The table itself, an alias before its dot, a keyword: not columns.
    assert_eq!(column("select * from Custo|mers"), None);
    assert_eq!(column("select c|.Status from Customers c"), None);
    assert_eq!(column("sel|ect Status from Customers"), None);
    // Quick Info shows the description too.
    let (text, caret) = at("select Sta|tus from Customers");
    assert_eq!(hover(&text, caret, Some(&s)).unwrap(), "column dbo.Customers.Status tinyint NOT NULL\n0 = new\n1 = active\n2 = closed, kept for the reports");
}
