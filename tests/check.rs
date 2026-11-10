use quillsql::check::problems;
use quillsql::complete::Schema;
use serde_json::json;

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sales"],
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int"}, {"name": "Name", "type": "nvarchar(100)"}, {"name": "Email", "type": "varchar(200)"}]},
            {"schema": "sales", "name": "Orders", "kind": "table", "columns": [
                {"name": "OrderId", "type": "int"}, {"name": "CustomerId", "type": "int"}, {"name": "Total", "type": "money"}, {"name": "OrderDate", "type": "date"}]},
            {"schema": "dbo", "name": "fn_Top", "kind": "table_function", "columns": [{"name": "Id", "type": "int"}]}
        ]
    }))
    .unwrap()
}

fn check(sql: &str) -> Vec<String> {
    problems(sql, &schema())
}

#[test]
fn sound_sql_has_no_problems() {
    for sql in [
        "SELECT c.Name, SUM(o.Total) AS Total FROM dbo.Customers c JOIN sales.Orders o ON o.CustomerId = c.CustomerId WHERE o.OrderDate >= DATEADD(day, -30, GETDATE()) GROUP BY c.Name ORDER BY Total DESC;",
        "SELECT TOP (10) Name, Email FROM dbo.Customers ORDER BY Name;",
        "WITH t AS (SELECT CustomerId, COUNT(*) n FROM sales.Orders GROUP BY CustomerId) SELECT c.Name, t.n FROM t JOIN dbo.Customers c ON c.CustomerId = t.CustomerId;",
        "CREATE TABLE #x (Id int); SELECT x.Id FROM #x x;",
        "DECLARE @t TABLE (Id int); SELECT Id FROM @t;",
        "SELECT value FROM STRING_SPLIT('a,b', ',');",
        "SELECT name FROM sys.objects;",
        "SELECT * FROM Other.dbo.Anything;",
        "SELECT Name, ROW_NUMBER() OVER (PARTITION BY Email ORDER BY CustomerId ROWS UNBOUNDED PRECEDING) rn FROM dbo.Customers;",
        "SELECT CAST(Total AS decimal(10, 2)) t, YEAR(OrderDate) y FROM sales.Orders;",
        "UPDATE c SET c.Name = N'x' FROM dbo.Customers c WHERE c.CustomerId = 1;",
        "SELECT Id FROM dbo.fn_Top(5);",
    ] {
        assert_eq!(check(sql), Vec::<String>::new(), "{sql}");
    }
}

#[test]
fn names_the_database_does_not_have() {
    assert_eq!(check("SELECT * FROM dbo.Customer"), ["dbo.Customer: no such table or view"]);
    let p = check("SELECT c.Emial FROM dbo.Customers c");
    assert!(p.len() == 1 && p[0].starts_with("c.Emial: no such column (columns of c: CustomerId, Name, Email)"), "{p:?}");
    let p = check("SELECT Phone FROM dbo.Customers WHERE Name = N'x'");
    assert_eq!(p, ["Phone: no such column in Customers"]);
    assert_eq!(check("SELECT 'abc FROM dbo.Customers"), ["string literal left open (line 1)"]);
}
