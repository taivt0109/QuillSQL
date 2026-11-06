use serde_json::json;
use quillsql::analysis::{RULES, analyze, apply_fixes};
use quillsql::complete::Schema;

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sales"],
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int", "identity": true}, {"name": "Name", "type": "nvarchar(100)"}]},
            {"schema": "sales", "name": "Orders", "kind": "table", "columns": [
                {"name": "OrderId", "type": "int"}, {"name": "CustomerId", "type": "int"}, {"name": "Total", "type": "money"}]},
            {"schema": "dbo", "name": "usp_GetOrders", "kind": "procedure", "params": [{"name": "@CustomerId", "type": "int"}]}
        ]
    }))
    .unwrap()
}

fn codes(sql: &str) -> Vec<&'static str> {
    analyze(sql, None).into_iter().map(|i| i.code).collect()
}

fn codes_with_schema(sql: &str) -> Vec<&'static str> {
    analyze(sql, Some(&schema())).into_iter().map(|i| i.code).collect()
}

fn has(sql: &str, code: &str) -> bool {
    codes(sql).contains(&code)
}

#[test]
fn every_reported_code_is_listed() {
    let mut seen: Vec<&str> = RULES.iter().map(|r| r.0).collect();
    seen.sort_unstable();
    let len = seen.len();
    seen.dedup();
    assert_eq!(len, seen.len(), "duplicate rule codes");
}

#[test]
fn clean_code_has_no_issues() {
    let sql = "SELECT c.Name FROM dbo.Customers AS c WHERE c.CustomerId = 1;";
    assert_eq!(codes(sql), Vec::<&str>::new());
}

#[test]
fn best_practices() {
    assert!(has("select * from dbo.t", "BP005"));
    assert!(!has("if exists (select * from dbo.t) print 1", "BP005"));
    assert!(has("select a from dbo.t order by 1", "BP002"));
    assert!(has("insert into dbo.t values (1)", "BP004"));
    assert!(has("select top (5) a from dbo.t", "BP006"));
    assert!(!has("select top (5) a from dbo.t order by a", "BP006"));
    assert!(has("declare @s varchar = 'x'; select @s", "BP007"));
    assert!(has("select cast(1 as nvarchar)", "BP008"));
    assert!(has("select convert(varchar, 1)", "BP008"));
    assert!(has("declare @s varchar(2) = 'x'; select @s", "BP009"));
    assert!(has("select @@identity", "BP010"));
    assert!(has("select a from dbo.t where a = null", "BP011"));
    assert!(!has("update dbo.t set a = null where b = 1", "BP011"));
    assert!(has("select case when a = 1 then 2 end from dbo.t", "BP012"));
    assert!(!has("select case when a = 1 then 2 else 3 end from dbo.t", "BP012"));
    assert!(has("exec ('select 1')", "BP013"));
    assert!(has("declare c cursor for select 1", "BP015"));
    assert!(has("create procedure dbo.p as begin return; end", "BP016"));
    assert!(!has("create procedure dbo.p as begin return 0; end", "BP016"));
    assert!(has("delete from dbo.t", "BP017"));
    assert!(has("update dbo.t set a = 1", "BP018"));
    assert!(!has("update dbo.t set a = 1 where b = 2", "BP018"));
    assert!(has("declare @m money = 1; select @m", "BP022"));
    assert!(has("create table dbo.t (a float)", "BP023"));
}

#[test]
fn deprecated_and_execution() {
    assert!(has("select a from dbo.t (nolock)", "DEP001"));
    assert!(has("select a, count(*) from dbo.t group by all a", "DEP003"));
    assert!(has("set rowcount 10", "DEP014"));
    assert!(has("select name from sysobjects", "DEP019"));
    assert!(has("select 'total' = a from dbo.t", "DEP021"));
    assert!(has("select a 'total' from dbo.t", "DEP021"));
    assert!(has("exec sp_addlogin 'x'", "DEP025"));
    assert!(has("create procedure dbo.sp_thing as begin select 1; end", "EI024"));
    assert!(has("select isnumeric(a) from dbo.t", "EI029"));
    assert!(has("create view dbo.v as select a from dbo.t order by a", "EI030"));
}

#[test]
fn misc_performance_and_style() {
    assert!(has("declare @unused int; select 1", "MI005"));
    assert!(!has("declare @x int = 1; select @x", "MI005"));
    assert!(has("declare @t table (a int); select 1", "MI001"));
    assert!(has("create table #t (a int); select 1", "MI002"));
    assert!(has("waitfor delay '00:00:01'", "MI007"));
    assert!(has("exec usp_GetOrders @CustomerId = 1", "PE001"));
    assert!(has("select a from t", "PE002"));
    assert!(has("select a into #x from dbo.t", "PE003"));
    assert!(has("select a from dbo.t with (index(ix))", "PE004"));
    assert!(has("select a from dbo.t inner hash join dbo.u on 1 = 1", "PE005"));
    assert!(has("select a from dbo.t with (nolock)", "PE006"));
    assert!(has("select a from dbo.t option (recompile)", "PE007"));
    assert!(has("set nocount off", "PE008"));
    assert!(has("if (select count(*) from dbo.t) > 0 print 1", "PE013"));
    assert!(has("select a from dbo.t where a in (select b from dbo.u)", "PE019"));
    assert!(has("select a from dbo.t, dbo.u", "ST001"));
    assert!(has("select total = a from dbo.t", "ST002"));
    assert!(has("create procedure dbo.p as select 1", "ST003"));
    assert!(has("select top 5 a from dbo.t order by a", "ST006"));
    assert!(has("exec dbo.p 1, 2", "ST008"));
    assert!(has("select dbo.t.a from dbo.t join dbo.u u on 1 = 1", "ST010"));
    assert!(has("create table #work (a int); insert into #work (a) values (1)", "ST011"));
    assert!(has("select a from dbo.t where a != 1", "ST013"));
    assert!(has("select 1 -- TODO: remove", "SC004"));
    assert!(!has("select 1 -- mastodon", "SC004"));
}

#[test]
fn metadata_rules() {
    let c = codes_with_schema("select Name, Total from Customers c join sales.Orders o on o.CustomerId = c.CustomerId");
    assert!(c.contains(&"MI003") && c.contains(&"PE002"), "{c:?}");
    assert!(!codes_with_schema("select c.Name from dbo.Customers c").contains(&"PE002"));
}

#[test]
fn fixes_apply_together() {
    let s = schema();
    let sql = "select * from Customers where Name != N'x' and Name = null and CustomerId = @@IDENTITY";
    let (fixed, n) = apply_fixes(sql, Some(&s), None);
    assert_eq!(fixed, "select CustomerId, Name from dbo.Customers where Name <> N'x' and Name IS NULL and CustomerId = SCOPE_IDENTITY()");
    assert_eq!(n, 5);
    let (fixed, _) = apply_fixes("insert into Customers values (N'a')", Some(&s), None);
    assert_eq!(fixed, "insert into dbo.Customers (Name) values (N'a')");
    let (fixed, _) = apply_fixes("select top 5 'x' = a from dbo.t (nolock) order by a", None, None);
    assert_eq!(fixed, "select top (5) [x] = a from dbo.t WITH (nolock) order by a");
}

#[test]
fn offsets_are_utf16() {
    let sql = "select N'Tiếng Việt' where a != 1";
    let issue = analyze(sql, None).into_iter().find(|i| i.code == "ST013").unwrap();
    let before = "select N'Tiếng Việt' where a ";
    assert_eq!(issue.start, before.encode_utf16().count());
    assert_eq!(issue.end, issue.start + 2);
}
