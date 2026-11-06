use serde_json::json;
use quillsql::TableAlias;
use quillsql::complete::Schema;
use quillsql::refactor::{self, Renamed};

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sales"],
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int"}, {"name": "Name", "type": "nvarchar(100)"}]},
            {"schema": "sales", "name": "Orders", "kind": "table", "columns": [
                {"name": "OrderId", "type": "int"}, {"name": "CustomerId", "type": "int"}, {"name": "Total", "type": "money"}]},
            {"schema": "dbo", "name": "usp_GetOrders", "kind": "procedure", "params": [{"name": "@CustomerId", "type": "int"}]}
        ]
    }))
    .unwrap()
}

fn caret(sql: &str) -> (String, usize) {
    let at = sql.find('|').unwrap();
    (sql.replace('|', ""), sql[..at].encode_utf16().count())
}

#[test]
fn expands_wildcards() {
    let s = schema();
    let (t, n) = refactor::expand_wildcards("select * from Customers", Some(&s), None).unwrap();
    assert_eq!((t.as_str(), n), ("select CustomerId, Name from Customers", 1));
    let (t, _) = refactor::expand_wildcards("select o.*, c.Name from Customers c join sales.Orders o on o.CustomerId = c.CustomerId", Some(&s), None).unwrap();
    assert!(t.starts_with("select o.OrderId, o.CustomerId, o.Total, c.Name from"), "{t}");
    let (t, _) = refactor::expand_wildcards("select top (5) * from (select OrderId, Total from sales.Orders) d", Some(&s), None).unwrap();
    assert_eq!(t, "select top (5) OrderId, Total from (select OrderId, Total from sales.Orders) d");
}

#[test]
fn leaves_multiplication_and_count_alone() {
    let s = schema();
    assert!(refactor::expand_wildcards("select count(*), Total * 2 from sales.Orders", Some(&s), None).is_err());
}

#[test]
fn expands_only_the_wildcard_at_the_caret() {
    let s = schema();
    let (sql, at) = caret("select * from Customers; select *| from sales.Orders");
    let (t, n) = refactor::expand_wildcards(&sql, Some(&s), Some(at)).unwrap();
    assert_eq!((t.as_str(), n), ("select * from Customers; select OrderId, CustomerId, Total from sales.Orders", 1));
}

#[test]
fn qualifies_objects_and_ambiguous_columns() {
    let s = schema();
    let (t, _) = refactor::qualify_object_names("select Name, Total from Customers c join Orders o on o.CustomerId = c.CustomerId where Total > 5", &s).unwrap();
    assert_eq!(t, "select c.Name, o.Total from dbo.Customers c join sales.Orders o on o.CustomerId = c.CustomerId where o.Total > 5");
    let (t, _) = refactor::qualify_object_names("exec usp_GetOrders 1", &s).unwrap();
    assert_eq!(t, "exec dbo.usp_GetOrders 1");
    let (t, _) = refactor::qualify_object_names("with x as (select 1 a) select a as Name from x join Customers c on 1 = 1", &s).unwrap();
    assert_eq!(t, "with x as (select 1 a) select x.a as Name from x join dbo.Customers c on 1 = 1");
}

#[test]
fn adds_and_removes_as() {
    assert_eq!(refactor::table_aliases("select * from t x", TableAlias::AddAs).unwrap(), "select * from t AS x");
    assert_eq!(refactor::table_aliases("select * from t as x", TableAlias::RemoveAs).unwrap(), "select * from t x");
}

#[test]
fn encapsulates_a_selection() {
    let text = "declare @from date = '2020-01-01', @n nvarchar(20);\nselect * from sales.Orders where d >= @from and n = @n;";
    let start = text.find("select").unwrap();
    let script = refactor::encapsulate_as_procedure(text, start, text.len(), "dbo.GetRecent").unwrap();
    assert_eq!(
        script,
        "CREATE PROCEDURE dbo.GetRecent\n    @from date,\n    @n nvarchar(20)\nAS\nBEGIN\n    SET NOCOUNT ON;\n\n    select * from sales.Orders where d >= @from and n = @n;\nEND;\nGO\n"
    );
}

#[test]
fn renames_variables_aliases_and_ctes() {
    let (sql, at) = caret("declare @x int = 1; select @x|;\nGO\nselect @x");
    let Ok(Renamed::Text(t, n)) = refactor::rename(&sql, at, "count", None) else { panic!() };
    assert_eq!((t.as_str(), n), ("declare @count int = 1; select @count;\nGO\nselect @x", 2));

    let (sql, at) = caret("select c.Name from Customers c| where c.CustomerId = 1");
    let Ok(Renamed::Text(t, _)) = refactor::rename(&sql, at, "cust", None) else { panic!() };
    assert_eq!(t, "select cust.Name from Customers cust where cust.CustomerId = 1");

    let (sql, at) = caret("with a| as (select 1 n) select a.n from a");
    let Ok(Renamed::Text(t, _)) = refactor::rename(&sql, at, "b", None) else { panic!() };
    assert_eq!(t, "with b as (select 1 n) select b.n from b");
}

#[test]
fn renaming_an_object_makes_a_script() {
    let s = schema();
    let (sql, at) = caret("select * from sales.Ord|ers");
    let Ok(Renamed::Script(script)) = refactor::rename(&sql, at, "Purchases", Some(&s)) else { panic!() };
    assert!(script.contains("EXEC sp_rename N'sales.Orders', N'Purchases';"), "{script}");
    let (sql, at) = caret("select o.Tot|al from sales.Orders o");
    let Ok(Renamed::Script(script)) = refactor::rename(&sql, at, "Amount", Some(&s)) else { panic!() };
    assert!(script.contains("EXEC sp_rename N'sales.Orders.Total', N'Amount', N'COLUMN';"), "{script}");
}

#[test]
fn inlines_an_exec() {
    let definition = "CREATE PROCEDURE dbo.usp_GetOrders @CustomerId int, @Max int = 10, @Count int OUTPUT\nAS\nBEGIN\n    SELECT TOP (@Max) * FROM sales.Orders WHERE CustomerId = @CustomerId;\n    SET @Count = @@ROWCOUNT;\nEND\nGO";
    let (sql, at) = caret("declare @c int;\n  exec dbo.usp_G|etOrders 5, @Count = @c OUTPUT;\nselect @c;");
    let t = refactor::inline_exec(&sql, at, definition).unwrap();
    assert_eq!(
        t,
        "declare @c int;\n  BEGIN -- inlined dbo.usp_GetOrders\n      DECLARE @CustomerId int = 5;\n      DECLARE @Max int = 10;\n      DECLARE @Count int = @c;\n      SELECT TOP (@Max) * FROM sales.Orders WHERE CustomerId = @CustomerId;\n      SET @Count = @@ROWCOUNT;\n      SET @c = @Count;\n  END;\nselect @c;"
    );
    assert!(refactor::inline_exec("exec p", 2, "CREATE PROC p AS BEGIN IF 1 = 1 RETURN; SELECT 1; END").is_err());
}
