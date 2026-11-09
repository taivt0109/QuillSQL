use quillsql::complete::Schema;
use quillsql::info::object_at;
use quillsql::script::{TableDetails, table_script, to_alter};
use quillsql::tools;
use serde_json::json;

fn caret(sql: &str) -> (String, usize) {
    let at = sql.find('|').unwrap();
    (sql.replace('|', ""), sql[..at].encode_utf16().count())
}

fn details(partitioned: bool) -> TableDetails {
    let mut value = json!({
        "schema": "sales", "name": "Orders",
        "columns": [
            {"name": "OrderId", "dataType": "int", "nullable": false, "identitySeed": "1", "identityIncrement": "1"},
            {"name": "OrderDate", "dataType": "date", "nullable": false, "defaultName": "DF_Orders_Date", "defaultDefinition": "(getdate())"},
            {"name": "Total", "dataType": "money", "nullable": true},
            {"name": "Year", "dataType": "", "computed": "(datepart(year,[OrderDate]))", "persisted": true}
        ],
        "indexes": [
            {"name": "PK_Orders", "kind": "CLUSTERED", "primaryKey": true, "unique": true, "keys": "[OrderId], [OrderDate]", "storage": {"filegroup": "PRIMARY"}},
            {"name": "IX_Orders_Total", "kind": "NONCLUSTERED", "keys": "[Total] DESC", "includes": "[OrderDate]", "filter": "([Total]>(0))", "storage": {"filegroup": "PRIMARY"}}
        ],
        "foreignKeys": [{"name": "FK_Orders_Customers", "columns": "[CustomerId]", "refSchema": "dbo", "refTable": "Customers", "refColumns": "[CustomerId]", "onDelete": "CASCADE", "onUpdate": "NO_ACTION"}],
        "checks": [{"name": "CK_Orders_Total", "definition": "([Total]>=(0))"}],
        "storage": {"filegroup": "PRIMARY"}
    });
    if partitioned {
        let by_date = json!({"scheme": "psOrderDate", "column": "OrderDate"});
        value["storage"] = by_date.clone();
        value["indexes"][0]["storage"] = by_date.clone();
        value["indexes"][1]["storage"] = by_date;
        value["partitioning"] = json!({"function": "pfOrderDate", "scheme": "psOrderDate", "column": "OrderDate", "parameterType": "date",
            "range": "RIGHT", "boundaries": ["'2024-01-01'", "'2025-01-01'"], "filegroups": ["PRIMARY", "PRIMARY", "PRIMARY"], "rows": [10, 20, 30]});
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn scripts_a_table() {
    let s = table_script(&details(false), "\n");
    assert_eq!(
        s,
        "CREATE TABLE [sales].[Orders]\n(\n    [OrderId] int IDENTITY(1, 1) NOT NULL,\n    [OrderDate] date NOT NULL CONSTRAINT [DF_Orders_Date] DEFAULT (getdate()),\n    [Total] money NULL,\n    [Year] AS (datepart(year,[OrderDate])) PERSISTED,\n    CONSTRAINT [PK_Orders] PRIMARY KEY CLUSTERED ([OrderId], [OrderDate]) ON [PRIMARY],\n    CONSTRAINT [CK_Orders_Total] CHECK ([Total]>=(0))\n) ON [PRIMARY];\nGO\nALTER TABLE [sales].[Orders] ADD CONSTRAINT [FK_Orders_Customers] FOREIGN KEY ([CustomerId]) REFERENCES [dbo].[Customers] ([CustomerId]) ON DELETE CASCADE;\nGO\nCREATE NONCLUSTERED INDEX [IX_Orders_Total] ON [sales].[Orders] ([Total] DESC) INCLUDE ([OrderDate]) WHERE ([Total]>(0)) ON [PRIMARY];\nGO\n"
    );
}

#[test]
fn scripts_a_partitioned_table() {
    let s = table_script(&details(true), "\n");
    assert!(s.starts_with("-- 3 partitions on [OrderDate]\nIF NOT EXISTS (SELECT 1 FROM sys.partition_functions WHERE name = N'pfOrderDate')\n    CREATE PARTITION FUNCTION [pfOrderDate] (date) AS RANGE RIGHT FOR VALUES ('2024-01-01', '2025-01-01');\nGO\n"), "{s}");
    assert!(s.contains("CREATE PARTITION SCHEME [psOrderDate] AS PARTITION [pfOrderDate] ALL TO ([PRIMARY]);"), "{s}");
    assert!(s.contains(") ON [psOrderDate] ([OrderDate]);"), "{s}");
    assert!(s.contains("PRIMARY KEY CLUSTERED ([OrderId], [OrderDate]) ON [psOrderDate] ([OrderDate])"), "{s}");
    assert!(s.contains("WHERE ([Total]>(0)) ON [psOrderDate] ([OrderDate]);"), "{s}");
}

#[test]
fn turns_create_into_alter() {
    assert_eq!(to_alter("-- note\nCREATE PROCEDURE dbo.p AS SELECT 1").unwrap(), "-- note\nALTER PROCEDURE dbo.p AS SELECT 1");
    assert_eq!(to_alter("create or alter view v as select 1").unwrap(), "ALTER view v as select 1");
}

#[test]
fn text_commands() {
    assert_eq!(tools::semicolons("select 1\nselect 2").unwrap(), "select 1;\nselect 2;");
    assert_eq!(tools::unformat("select a,\n    b -- x\nfrom t\nGO\nselect 2").unwrap(), "select a, b -- x\nfrom t\nGO\nselect 2");
    assert_eq!(tools::brackets("select [a] from [dbo].[t]", false).unwrap(), "select a from dbo.t");
    assert_eq!(tools::brackets("select a from dbo.t", true).unwrap(), "select a from [dbo].[t]");
    assert_eq!(tools::casing("Select count(*) From t", true).unwrap(), "SELECT COUNT(*) FROM t");
    assert_eq!(tools::remove_comments("select 1 -- one\n/* two */ select 2").unwrap(), "select 1\nselect 2");
}

#[test]
fn converts_sp_executesql() {
    let (sql, at) = caret("EXEC sp_execu|tesql N'SELECT * FROM t WHERE a = @a AND b = ''x''', N'@a int, @b nvarchar(10)', @a = 5, @b = N'y';");
    assert_eq!(
        tools::convert_sp_executesql(&sql, at).unwrap(),
        "DECLARE @a int = 5;\nDECLARE @b nvarchar(10) = N'y';\nSELECT * FROM t WHERE a = @a AND b = 'x'"
    );
    let (sql, at) = caret("exec sp_executesql| @sql");
    assert!(tools::convert_sp_executesql(&sql, at).is_err());
}

#[test]
fn ranges_and_outline() {
    let (sql, at) = caret("select 1;\nupdate t set a = 1| where b = 2;\nGO\nselect 3");
    let r = tools::statement_range(&sql, at).unwrap();
    assert_eq!(&sql[r.0..r.1], "update t set a = 1 where b = 2;");
    let b = tools::batch_range(&sql, at).unwrap();
    assert_eq!(&sql[b.0..b.1], "select 1;\nupdate t set a = 1 where b = 2;");
    let o = tools::outline(&sql);
    assert_eq!(o.iter().map(|e| (e.line, e.kind.as_str())).collect::<Vec<_>>(), vec![(1, "SELECT"), (2, "UPDATE"), (4, "SELECT")]);
}

#[test]
fn execution_warnings() {
    let w = tools::execution_warnings("select 1;\ndelete from dbo.t;\nupdate dbo.t set a = 1 where b = 2;\ntruncate table dbo.t;\ndrop table #tmp;\ndrop table dbo.u;");
    assert_eq!(w, vec![
        "line 2: DELETE without WHERE changes every row",
        "line 4: TRUNCATE TABLE removes every row",
        "line 6: DROP TABLE deletes a table and its data",
    ]);
}

#[test]
fn finds_the_object_at_the_caret() {
    let s: Schema = serde_json::from_value(json!({"defaultSchema": "dbo", "objects": [
        {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [{"name": "Name", "type": "int"}]},
        {"schema": "dbo", "name": "usp_Get", "kind": "procedure"}]})).unwrap();
    let at = |sql: &str| {
        let (text, c) = caret(sql);
        object_at(&text, c, &s).map(|o| o.name.clone())
    };
    assert_eq!(at("select * from dbo.Cust|omers"), Some("Customers".into()));
    assert_eq!(at("select * from d|bo.Customers"), Some("Customers".into()));
    assert_eq!(at("select c.Na|me from Customers c"), Some("Customers".into()));
    assert_eq!(at("select c|.Name from Customers c"), Some("Customers".into()));
    assert_eq!(at("exec usp_G|et"), Some("usp_Get".into()));
    assert_eq!(at("sel|ect 1"), None);
}

#[test]
fn a_keyword_typed_in_lower_case_is_found_before_the_caret() {
    let at = |s: &str| quillsql::tools::keyword_before(s, s.encode_utf16().count(), Some(' '));
    assert_eq!(at("select"), Some((0, 6, "SELECT".to_string())));
    assert_eq!(at("SELECT * from dbo.t whEre"), Some((20, 25, "WHERE".to_string())));
    assert_eq!(at("select * from t where n = 1 and"), Some((28, 31, "AND".to_string())));
    // Already upper case, not a keyword, after a dot, in a string or a comment: nothing.
    assert_eq!(at("SELECT"), None);
    assert_eq!(at("select customer"), None);
    assert_eq!(at("select t.order"), None);
    assert_eq!(at("select 'from"), None);
    assert_eq!(at("-- from"), None);
    assert_eq!(at("/* select from"), None);
    assert_eq!(at("select [from"), None);
    // UTF-16 positions after non-ASCII text.
    assert_eq!(at("select N'tiếng việt' from"), Some((21, 25, "FROM".to_string())));
}

#[test]
fn functions_before_a_parenthesis_and_data_types_where_one_is_expected_are_found_too() {
    let at = |s: &str, next: char| quillsql::tools::keyword_before(s, s.encode_utf16().count(), Some(next)).map(|f| f.2);
    assert_eq!(at("select dateadd", '(').as_deref(), Some("DATEADD"));
    assert_eq!(at("select count", '(').as_deref(), Some("COUNT"));
    // A column named like a function or a type keeps its case.
    assert_eq!(at("select count", ' '), None);
    assert_eq!(at("select date", ','), None);
    assert_eq!(at("declare @x int", ';').as_deref(), Some("INT"));
    assert_eq!(at("declare @d datetime2", ' ').as_deref(), Some("DATETIME2"));
    assert_eq!(at("select cast(x as varchar", '(').as_deref(), Some("VARCHAR"));
    assert_eq!(at("create table #t (Id int", ',').as_deref(), Some("INT"));
    assert_eq!(at("create table #t (Id int, Code varchar", '(').as_deref(), Some("VARCHAR"));
}
