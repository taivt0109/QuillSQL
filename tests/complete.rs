use serde_json::json;
use quillsql::complete::{Schema, complete, complete_with};
use quillsql::snippets;

fn schema() -> Schema {
    serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo",
        "schemas": ["dbo", "sales"], "databases": ["Shop", "master"],
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [
                {"name": "CustomerId", "type": "int", "identity": true}, {"name": "Name", "type": "nvarchar(100)"}, {"name": "Order", "type": "int"}]},
            {"schema": "sales", "name": "Orders", "kind": "table", "columns": [
                {"name": "OrderId", "type": "int"}, {"name": "CustomerId", "type": "int"}, {"name": "Total", "type": "money"}]},
            {"schema": "dbo", "name": "ActiveCustomers", "kind": "view", "columns": [{"name": "CustomerId", "type": "int"}]},
            {"schema": "dbo", "name": "usp_GetOrders", "kind": "procedure", "params": [{"name": "@CustomerId", "type": "int"}]},
            {"schema": "dbo", "name": "fn_Tax", "kind": "scalar_function", "params": [{"name": "@amount", "type": "money"}]},
            {"schema": "sys", "name": "all_objects", "kind": "view", "columns": [{"name": "name", "type": "sysname"}]}
        ],
        "foreignKeys": [{"schema": "sales", "table": "Orders", "columns": ["CustomerId"], "refSchema": "dbo", "refTable": "Customers", "refColumns": ["CustomerId"]}]
    }))
    .unwrap()
}

fn run(sql: &str) -> quillsql::complete::Completion {
    let caret = sql.find('|').unwrap();
    let text = sql.replace('|', "");
    complete(&text, text[..caret].encode_utf16().count(), Some(&schema()))
}

/// `|` marks the caret.
fn labels(sql: &str) -> Vec<String> {
    run(sql).items.into_iter().map(|i| i.label).collect()
}

fn inserts(sql: &str) -> Vec<String> {
    run(sql).items.into_iter().map(|i| i.insert).collect()
}

#[test]
fn tables_and_views_after_from() {
    let l = labels("select * from |");
    assert!(l.contains(&"Customers".into()) && l.contains(&"Orders".into()) && l.contains(&"ActiveCustomers".into()), "{l:?}");
    assert!(!l.contains(&"usp_GetOrders".into()));
    assert!(inserts("select * from Ord|").contains(&"sales.Orders AS o".into()));
}

#[test]
fn system_views_appear_once_their_schema_is_typed() {
    let l = labels("select * from |");
    assert!(l.contains(&"Customers".into()) && !l.contains(&"all_objects".into()), "{l:?}");
    assert_eq!(inserts("select * from sys.all_o|"), vec!["all_objects AS ao"]);
}

#[test]
fn short_prefixes_only_match_the_start() {
    let l = labels("select * from us|");
    assert!(!l.contains(&"Customers".into()), "{l:?}");
}

#[test]
fn prefix_filters_and_ranks_prefix_matches_first() {
    let l = labels("select * from cust|");
    assert_eq!(l.first().map(String::as_str), Some("Customers"));
    assert!(l.contains(&"ActiveCustomers".into()), "substring match is still offered: {l:?}");
}

#[test]
fn procedures_after_exec() {
    assert_eq!(labels("exec usp|"), vec!["usp_GetOrders".to_string()]);
}

#[test]
fn columns_through_alias_and_table_name() {
    assert_eq!(labels("select c.| from dbo.Customers c"), vec!["CustomerId", "Name", "Order"]);
    assert!(labels("select Orders.| from sales.Orders").contains(&"Total".into()));
    assert!(inserts("select c.Or| from Customers c").contains(&"[Order]".into()));
}

#[test]
fn columns_of_all_sources_with_ambiguous_names_qualified() {
    let i = inserts("select | from Customers c join sales.Orders o on o.CustomerId = c.CustomerId");
    assert!(i.contains(&"c.CustomerId".into()) && i.contains(&"o.CustomerId".into()), "{i:?}");
    assert!(i.contains(&"Total".into()) && i.contains(&"Name".into()));
}

#[test]
fn objects_in_a_schema() {
    assert_eq!(labels("select * from sales.|"), vec!["Orders"]);
}

#[test]
fn cte_and_derived_table_columns() {
    assert_eq!(labels("with top_c as (select CustomerId, Name as N from Customers) select t.| from top_c t"), vec!["CustomerId", "N"]);
    assert_eq!(labels("select d.| from (select OrderId, Total * 2 as Doubled from sales.Orders) d"), vec!["Doubled", "OrderId"]);
    assert!(labels("with x as (select 1 a) select * from |").contains(&"x".into()));
}

#[test]
fn variables_declared_earlier() {
    let l = labels("declare @from date, @to date; select * from t where d between @f|");
    assert_eq!(l.first().map(String::as_str), Some("@from"));
}

#[test]
fn insert_column_list() {
    assert_eq!(labels("insert into sales.Orders (|"), vec!["CustomerId", "OrderId", "Total"]);
}

#[test]
fn nothing_inside_strings_comments_or_numbers() {
    assert!(labels("select 'from |").is_empty());
    assert!(labels("select 1 -- from |").is_empty());
    assert!(labels("select /* from | */").is_empty());
    assert!(labels("select 12|").is_empty());
}

#[test]
fn bracketed_prefix() {
    let c = run("select * from [Cus|");
    assert_eq!(c.start, 14);
    assert_eq!(c.items[0].label, "Customers");
}

#[test]
fn offsets_are_utf16() {
    let c = run("select N'Tiếng Việt', c.Na| from Customers c");
    let before = "select N'Tiếng Việt', c.";
    assert_eq!(c.start, before.encode_utf16().count());
    assert_eq!(c.items[0].label, "Name");
}

#[test]
fn works_without_metadata() {
    let c = complete("sel", 3, None);
    assert!(c.items.iter().any(|i| i.label == "SELECT"));
}

#[test]
fn statement_scope_ignores_other_statements() {
    let l = labels("select * from sales.Orders o;\nselect o.| from Customers c");
    assert!(l.is_empty(), "{l:?}");
}

#[test]
fn join_suggestions_come_from_foreign_keys() {
    let i = inserts("select * from Customers c join |");
    assert_eq!(i.first().map(String::as_str), Some("sales.Orders AS o ON o.CustomerId = c.CustomerId"), "{i:?}");
    let on = inserts("select * from Customers c join sales.Orders o on |");
    assert_eq!(on.first().map(String::as_str), Some("o.CustomerId = c.CustomerId"), "{on:?}");
}

#[test]
fn exec_inserts_the_parameter_list() {
    assert_eq!(inserts("exec usp_G|"), vec!["dbo.usp_GetOrders @CustomerId = 0 -- int"]);
    assert_eq!(inserts("exec usp_GetOrders |").first().map(String::as_str), Some("@CustomerId = 0 -- int"));
    assert_eq!(labels("exec usp_GetOrders @C|"), vec!["@CustomerId"]);
}

#[test]
fn insert_and_update_bodies_skip_identity_columns() {
    let i = inserts("insert into dbo.Customers |");
    let expected = "(Name, [Order])\nVALUES\n(\n    N'', -- Name nvarchar(100)\n    0 -- Order int\n)";
    assert_eq!(i.first().map(String::as_str), Some(expected), "{i:?}");
    let u = inserts("update Customers set |");
    let expected = "Name = N'', -- nvarchar(100)\n                     [Order] = 0 -- int";
    assert_eq!(u.first().map(String::as_str), Some(expected), "{u:?}");
}

#[test]
fn snippets_are_offered_and_an_exact_prefix_comes_first() {
    let all = snippets::builtins();
    let c = complete_with("ssf", 3, Some(&schema()), &all);
    assert_eq!((c.items[0].label.as_str(), c.items[0].kind), ("ssf", "snippet"));
}

#[test]
fn snippet_expansion() {
    let all = snippets::builtins();
    let env = snippets::Env { selected_text: "SELECT 1;".into(), ..Default::default() };
    let e = snippets::expand(snippets::find(&all, "SSF").unwrap(), &env, "", "\n");
    assert_eq!((e.text.as_str(), e.select_start, e.select_end), ("SELECT * FROM ", 14, 14));
    let e = snippets::expand(snippets::find(&all, "be").unwrap(), &env, "  ", "\r\n");
    assert_eq!(e.text, "BEGIN\r\n      SELECT 1;\r\n  END");
    let e = snippets::expand(snippets::find(&all, "df").unwrap(), &env, "", "\n");
    assert_eq!((e.text.as_str(), &e.text[e.select_start..e.select_end]), ("DELETE FROM table\nWHERE ", "table"));
}


#[test]
fn tables_get_an_alias_unless_one_is_written() {
    assert_eq!(inserts("select * from Cust|").first().map(String::as_str), Some("dbo.Customers AS c"));
    assert_eq!(inserts("select * from Customers c join sales.Ord|").first().map(String::as_str), Some("Orders AS o"));
    assert_eq!(inserts("select * from Cust| x").first().map(String::as_str), Some("dbo.Customers"));
    assert_eq!(inserts("insert into Cust|").first().map(String::as_str), Some("dbo.Customers"));
    let text = "select * from Cust";
    let plain = quillsql::complete::CompleteOptions { assign_aliases: false, qualify_object_names: false, ..Default::default() };
    let c = quillsql::complete::complete_full(text, text.len(), Some(&schema()), &[], plain);
    assert_eq!(c.items[0].insert, "Customers");
    let short = quillsql::complete::CompleteOptions { alias_with_as: false, ..Default::default() };
    let c = quillsql::complete::complete_full(text, text.len(), Some(&schema()), &[], short);
    assert_eq!(c.items[0].insert, "dbo.Customers c");
}

#[test]
fn functions_are_inserted_with_parentheses() {
    let i = inserts("select coun|");
    assert!(i.contains(&"COUNT()".into()), "{i:?}");
    assert!(inserts("select fn_|").contains(&"dbo.fn_Tax()".into()));
}

#[test]
fn objects_of_the_kind_after_alter() {
    assert_eq!(inserts("alter procedure usp|"), vec!["dbo.usp_GetOrders"]);
    assert_eq!(labels("alter view |"), vec!["ActiveCustomers", "dbo", "sales"]);
    assert_eq!(labels("drop function fn|"), vec!["fn_Tax"]);
}

#[test]
fn a_second_table_after_a_comma_whether_or_not_the_alias_has_as() {
    for sql in ["select * from sales.Orders o, Cust|", "select * from sales.Orders AS o, Cust|"] {
        assert!(labels(sql).iter().any(|l| l == "Customers"), "{sql}: {:?}", labels(sql));
    }
}

#[test]
fn a_new_statement_or_clause_after_a_table_and_its_alias() {
    let l = labels("select * from dbo.Customers c\n\nsel|");
    assert_eq!(l.first().map(String::as_str), Some("SELECT"), "{l:?}");
    let l = labels("select * from dbo.Customers AS c\r\n\r\nwh|");
    assert_eq!(l.first().map(String::as_str), Some("WHERE"), "{l:?}");
    let l = labels("select * from dbo.Customers c le|");
    assert!(l.iter().any(|x| x == "LEFT JOIN"), "{l:?}");
    // Still nothing while the alias itself is being typed.
    assert!(labels("select * from dbo.Customers cu|").is_empty());
}