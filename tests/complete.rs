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
    run(sql).items.into_iter().filter(|i| i.kind != "picker").map(|i| i.label).collect()
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
fn inner_matches_come_after_the_names_that_start_with_it() {
    let l = labels("select * from us|");
    let customers = l.iter().position(|x| x == "Customers");
    let first_start = l.iter().position(|x| x.to_ascii_lowercase().starts_with("us"));
    if let (Some(c), Some(s)) = (customers, first_start) {
        assert!(s < c, "{l:?}");
    }
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
    // With an alias, every column is written with it.
    assert!(i.contains(&"o.Total".into()) && i.contains(&"c.Name".into()), "{i:?}");
    let i = inserts("select | from dbo.Customers");
    assert!(i.contains(&"Name".into()), "{i:?}");
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
    let expected = "\n(\n    Name,\n    [Order]\n)\nVALUES\n(\n    N'', -- Name - nvarchar(100)\n    0 -- Order - int\n)";
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
    assert!(inserts("insert into Cust|").first().is_some_and(|i| i.starts_with("dbo.Customers\n(\n    Name,") && i.contains("VALUES")));
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
#[test]
fn columns_after_where_and_by_and_comparisons() {
    let base = "select * from dbo.Customers c\njoin sales.Orders o on o.CustomerId = c.CustomerId\n";
    for tail in ["where |", "where c.Name > 'x' and |", "where c.Name > 'x' or |", "group by |", "order by |", "where o.Total > |"] {
        let l = labels(&format!("{base}{tail}"));
        let first: Vec<&str> = l.iter().take(8).map(String::as_str).collect();
        assert!(first.iter().any(|x| x.contains("Total")) && first.iter().any(|x| x.contains("Name")), "{tail}: {first:?}");
    }
}
#[test]
fn nothing_for_go_alone_on_its_line() {
    assert!(labels("select 1\nGO|").is_empty());
    assert!(labels("select 1\n  go|\nselect 2").is_empty());
    assert!(labels("GO|").is_empty());
    // Inside a statement it is the start of a word, such as GROUP.
    assert!(labels("select * from dbo.Customers c\nGRO|").iter().any(|l| l == "GROUP BY" || l.starts_with("GROUP")));
}
#[test]
fn a_whole_word_typed_in_another_case_comes_first() {
    // Customers has a column named Order, sales.Orders a table: `order` puts the column first.
    let l = labels("select c.Name from dbo.Customers c where c.order|");
    assert_eq!(l.first().map(String::as_str), Some("Order"), "{l:?}");
    let l = labels("select * from dbo.Customers c where c.Name = 'x' and|");
    assert_eq!(l.first().map(String::as_str), Some("AND"), "{l:?}");
}
#[test]
fn camel_case_initials_and_inner_matches() {
    let l = labels("select * from dbo.Customers c where c.cid|");
    assert_eq!(l.first().map(String::as_str), Some("CustomerId"), "{l:?}");
    // `ust` is only inside a name: listed, but not selected.
    let r = run("select * from dbo.Customers c where c.ust|");
    assert!(r.items.iter().any(|i| i.label == "CustomerId"), "{:?}", r.items.iter().map(|i| &i.label).collect::<Vec<_>>());
    assert_eq!(r.selected, None);
}

#[test]
fn only_sure_matches_are_selected() {
    // `and` matches RAND only inside, but the keyword AND exactly: AND is selected.
    let r = run("select * from dbo.Customers c where c.Name = 'x' and|");
    assert_eq!(r.items.first().map(|i| i.label.as_str()), Some("AND"));
    assert_eq!(r.selected, Some(0));
    // A list opened by a space, before anything is typed, selects nothing.
    assert_eq!(run("select * from dbo.Customers c where |").selected, None);
    // A prefix selects its first match.
    assert_eq!(run("sel|").selected, Some(0));
    // Something that only appears inside other words selects nothing.
    assert_eq!(run("select * from dbo.Customers c where c.Name = 'x' and c.ame|").selected, None);
}
fn first(sql: &str) -> Option<String> {
    let r = run(sql);
    r.selected.map(|i| r.items[i].label.clone())
}

#[test]
fn after_a_complete_operand_come_operators_and_clauses() {
    assert_eq!(first("select * from dbo.Customers c order by c.Name de|").as_deref(), Some("DESC"));
    assert_eq!(first("select * from dbo.Customers c where c.Name l|").as_deref(), Some("LIKE"));
    assert_eq!(first("select * from dbo.Customers c where c.Name is n|").as_deref(), Some("NULL"));
    assert_eq!(first("select * from dbo.Customers c where c.CustomerId = 1 gr|").as_deref(), Some("GROUP BY"));
    assert_eq!(first("select * from dbo.Customers c where c.CustomerId = 1 an|").as_deref(), Some("AND"));
    let l = labels("select * from dbo.Customers c where c.Name = 'x' |");
    assert_eq!(&l[..2], ["AND", "OR"], "{l:?}");
    assert!(!l.contains(&"CustomerId".into()) && !l.iter().any(|x| x == "ABS"), "{l:?}");
    // A function's arguments, the select list after TOP, and a new condition still get columns.
    assert!(labels("select count(c.Name |) from dbo.Customers c").contains(&"Name".into()));
    assert!(labels("select * from dbo.Customers c where (c.Name = 'x' |").starts_with(&["AND".into(), "OR".into()]));
    assert!(labels("select top 10 | from dbo.Customers c").contains(&"Name".into()));
    assert!(labels("select * from dbo.Customers c where c.Name = 'x' and |").contains(&"Name".into()));
}

#[test]
fn words_that_must_follow_a_keyword() {
    assert_eq!(labels("select * from dbo.Customers c inner |"), ["JOIN"]);
    assert_eq!(labels("select * from dbo.Customers c left |"), ["JOIN", "OUTER JOIN"]);
    assert_eq!(labels("select * from dbo.Customers c where c.Name is |"), ["NULL", "NOT NULL"]);
    assert_eq!(labels("select * from dbo.Customers c order |"), ["BY"]);
    assert_eq!(labels("insert in|"), ["INTO"]);
    assert_eq!(labels("select * from dbo.Customers c union |"), ["ALL", "SELECT"]);
    assert_eq!(labels("select * from dbo.Customers c where exists (|"), ["SELECT"]);
    assert_eq!(labels("select row_number() over (|"), ["PARTITION BY", "ORDER BY"]);
    assert!(labels("create |").starts_with(&["TABLE".into(), "PROCEDURE".into()]));
    assert!(labels("drop |").contains(&"VIEW".into()));
    assert!(labels("select * from dbo.Customers c with (|").starts_with(&["NOLOCK".into()]));
    assert!(labels("select * from dbo.Customers c option (|").starts_with(&["RECOMPILE".into()]));
    assert!(labels("select top |").is_empty());
    assert_eq!(labels("insert into dbo.Customers (Name) |")[0], "VALUES");
}

#[test]
fn data_types_where_a_type_comes_next() {
    assert_eq!(labels("select cast(1 as |")[0], "INT");
    assert_eq!(first("declare @x nv|").as_deref(), Some("NVARCHAR"));
    assert!(labels("create table t (Id |").contains(&"INT".into()));
    assert!(labels("create procedure p @Id |").contains(&"INT".into()));
}

#[test]
fn statement_keywords_where_a_statement_starts() {
    assert_eq!(labels("|")[0], "SELECT");
    assert!(labels("begin tran\n|").contains(&"UPDATE".into()));
    assert!(!labels("|").iter().any(|l| l == "ABS"));
    let set = labels("declare @x int; set |");
    assert!(set.contains(&"@x".into()) && set.contains(&"NOCOUNT ON".into()), "{set:?}");
}

#[test]
fn tables_after_insert_into_and_schemas_after_the_database() {
    assert!(labels("insert into |").contains(&"Customers".into()));
    assert_eq!(labels("select * from Shop.|"), ["dbo", "sales"]);
    assert!(labels("delete dbo.Customers where |").contains(&"Name".into()));
}

#[test]
fn the_functions_most_called_come_first() {
    assert_eq!(labels("select co|")[0], "COUNT");
    assert_eq!(labels("select su|")[0], "SUM");
}

#[test]
fn temporary_tables_and_table_variables_have_their_columns() {
    let script = "create table #t (Id int not null, Code varchar(10), constraint pk primary key (Id));\n";
    let l = labels(&format!("{script}select * from #t t where t.|"));
    assert_eq!(l, ["Code", "Id"]);
    assert!(labels(&format!("{script}select * from #|")).contains(&"#t".into()));
    // Still being typed, with no closing parenthesis.
    for partial in ["create table #t (Id int, |", "create table #t (|", "create table #t (Id |", "declare @t table (Id int, Code varchar(|"] {
        let _ = labels(partial);
    }
    let l = labels("declare @t table (Id int, Total money); select * from @t x where x.|");
    assert_eq!(l, ["Id", "Total"]);
    let l = labels("select c.CustomerId, c.Name into #c from dbo.Customers c; select * from #c x where x.|");
    assert_eq!(l, ["CustomerId", "Name"]);
    let l = labels("select * into #all from dbo.Customers; select * from #all a where a.|");
    assert!(l.contains(&"CustomerId".into()) && l.contains(&"Name".into()), "{l:?}");
}

#[test]
fn exec_goes_on_with_the_parameters_not_yet_given() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo"],
        "objects": [{"schema": "dbo", "name": "p", "kind": "procedure", "params": [
            {"name": "@A", "type": "int"}, {"name": "@B", "type": "date"}, {"name": "@C", "type": "bit"}]}]
    })).unwrap();
    let text = "exec dbo.p @A = 1, ";
    let l: Vec<String> = complete(text, text.len(), Some(&s)).items.into_iter().map(|i| i.label).collect();
    assert_eq!(l, ["@B", "@C"]);
}

#[test]
fn ddl_permissions_and_merge() {
    assert!(labels("alter table dbo.Customers |").starts_with(&["ADD".into(), "ALTER COLUMN".into()]));
    assert_eq!(labels("alter table dbo.Customers alter column |"), ["CustomerId", "Name", "Order"]);
    assert_eq!(labels("alter table dbo.Customers drop column |"), ["CustomerId", "Name", "Order"]);
    assert!(labels("alter table sales.Orders add constraint fk foreign key (CustomerId) |").contains(&"REFERENCES".into()));
    assert!(labels("alter table sales.Orders add constraint fk foreign key (CustomerId) references |").contains(&"Customers".into()));
    assert_eq!(labels("alter table sales.Orders add constraint fk foreign key (CustomerId) references dbo.Customers (|"), ["CustomerId", "Name", "Order"]);
    assert!(labels("create index ix on |").contains(&"Customers".into()));
    assert_eq!(labels("create index ix on dbo.Customers (|"), ["CustomerId", "Name", "Order"]);
    assert_eq!(labels("create index ix on dbo.Customers (Name, |"), ["CustomerId", "Name", "Order"]);
    assert_eq!(labels("create index ix on dbo.Customers (Name) include (|"), ["CustomerId", "Name", "Order"]);
    assert!(labels("create index ix on dbo.Customers (Name) |").contains(&"INCLUDE".into()));
    assert!(labels("create table t (Id int |").starts_with(&["NOT NULL".into()]));
    assert_eq!(labels("create table t (Id int not |"), ["NULL"]);
    assert!(labels("create table t (Id |").contains(&"INT".into()));
    assert!(labels("grant |").starts_with(&["SELECT".into()]));
    let on = labels("grant select on |");
    assert!(on.contains(&"Customers".into()) && on.contains(&"usp_GetOrders".into()), "{on:?}");
    assert!(labels("grant select on dbo.Customers |").contains(&"TO".into()));
    assert!(labels("merge dbo.Customers as t using sales.Orders as s on s.CustomerId = t.CustomerId |").starts_with(&["WHEN MATCHED THEN".into()]));
    assert_eq!(labels("merge dbo.Customers as t using sales.Orders as s on s.CustomerId = t.CustomerId when |")[0], "MATCHED");
    assert!(labels("merge t using s on 1 = 1 when matched then |").contains(&"UPDATE SET".into()));
    assert!(labels("drop table |").contains(&"IF EXISTS".into()));
    assert!(labels("drop table if exists |").contains(&"Customers".into()));
}

#[test]
fn procedures_are_not_offered_in_an_expression() {
    let l = labels("select dbo.|");
    assert!(l.contains(&"fn_Tax".into()) && !l.contains(&"usp_GetOrders".into()), "{l:?}");
}

#[test]
fn generated_aliases_leave_out_prefixes_or_use_your_own() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo"],
        "objects": [{"schema": "dbo", "name": "tblCustomer", "kind": "table"}, {"schema": "dbo", "name": "OrderLine", "kind": "table"}]
    })).unwrap();
    let inserts = |text: &str, options: quillsql::complete::CompleteOptions| -> Vec<String> {
        quillsql::complete::complete_full(text, text.len(), Some(&s), &[], options).items.into_iter().map(|i| i.insert).collect()
    };
    let defaults = quillsql::complete::CompleteOptions::default();
    assert!(inserts("select * from tblC", defaults.clone()).contains(&"dbo.tblCustomer AS c".into()));
    let mut own = defaults;
    own.aliases.insert("OrderLine".into(), "lines".into());
    assert!(inserts("select * from Ord", own).contains(&"dbo.OrderLine AS lines".into()));
}

#[test]
fn other_databases_named_in_the_script() {
    let other: Schema = serde_json::from_value(json!({
        "database": "Archive", "defaultSchema": "dbo", "schemas": ["dbo", "hist"],
        "objects": [{"schema": "hist", "name": "OldOrders", "kind": "table", "columns": [{"name": "OldId", "type": "int"}]}]
    })).unwrap();
    let here = schema();
    let run = |sql: &str| -> Vec<String> {
        let caret = sql.find('|').unwrap();
        let text = sql.replace('|', "");
        quillsql::complete::complete_across(&text, caret, Some(&here), &[&other], &[], Default::default()).items.into_iter().map(|i| i.label).collect()
    };
    assert_eq!(run("select * from Archive.|"), ["dbo", "hist"]);
    assert_eq!(run("select * from Archive.hist.|"), ["OldOrders"]);
    assert_eq!(run("select * from Archive.hist.OldOrders o where o.|"), ["OldId"]);
    // Its tables are not offered without the database.
    assert!(!run("select * from |").contains(&"OldOrders".into()));
    assert!(run("select * from |").contains(&"master".into()));
}

#[test]
fn the_column_picker_opens_a_select_list() {
    let kinds = |sql: &str| run(sql).items.into_iter().map(|i| i.kind).collect::<Vec<_>>();
    assert_eq!(kinds("select | from dbo.Customers c")[0], "picker");
    assert_eq!(kinds("select c.| from dbo.Customers c")[0], "picker");
    assert!(!kinds("select * from dbo.Customers c where |").contains(&"picker"));
    assert!(!kinds("select c.Na| from dbo.Customers c").contains(&"picker"));
}

#[test]
fn the_shortest_name_that_starts_with_the_letters_comes_first() {
    let mut s = schema();
    let more: Schema = serde_json::from_value(json!({"objects": [
        {"schema": "sys", "name": "table_types", "kind": "view", "columns": [{"name": "name", "type": "sysname"}]},
        {"schema": "sys", "name": "tables", "kind": "view", "columns": [{"name": "name", "type": "sysname"}]}
    ]}))
    .unwrap();
    s.objects.extend(more.objects);
    let text = "select * from sys.tabl";
    let l: Vec<String> = complete(text, text.len(), Some(&s)).items.into_iter().map(|i| i.label).collect();
    assert_eq!(l.first().map(String::as_str), Some("tables"), "{l:?}");
}

#[test]
fn a_parameter_given_by_name_is_not_offered_again_nor_taken_for_a_variable() {
    let l = labels("exec dbo.usp_GetOrders @CustomerId = 1, @|");
    assert!(!l.contains(&"@CustomerId".into()), "{l:?}");
    let l = labels("declare @CustomerId int = 1;\nexec dbo.usp_GetOrders @CustomerId = @|");
    assert!(l.contains(&"@CustomerId".into()), "{l:?}");
}

#[test]
fn picking_the_table_of_an_insert_writes_the_whole_statement() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo"],
        "objects": [{"schema": "dbo", "name": "JitBin", "kind": "table", "columns": [
            {"name": "Id", "type": "int", "identity": true},
            {"name": "Name", "type": "varchar(100)"},
            {"name": "Note", "type": "nvarchar(max)", "nullable": true},
            {"name": "IsActive", "type": "bit", "default": "((1))"},
            {"name": "CreatedOnUtc", "type": "datetime", "default": "(getutcdate())"}]}]
    }))
    .unwrap();
    let at = |sql: &str| {
        let caret = sql.find('|').unwrap();
        let text = sql.replace('|', "");
        complete(&text, caret, Some(&s)).items.into_iter().next().map(|i| i.insert)
    };
    let expected = "dbo.JitBin
(
    Name,
    Note,
    IsActive,
    CreatedOnUtc
)
VALUES
(
    '', -- Name - varchar(100)
    NULL, -- Note - nvarchar(max)
    DEFAULT, -- IsActive - bit (default 1)
    DEFAULT -- CreatedOnUtc - datetime (default getutcdate())
)";
    assert_eq!(at("insert into Jit|").as_deref(), Some(expected));
    assert_eq!(at("INSERT dbo.Jit|").as_deref(), Some(expected.trim_start_matches("dbo.")));
    // Written further already: only the name.
    assert_eq!(at("insert into Jit| (Name) values ('a')").as_deref(), Some("dbo.JitBin"));
}

#[test]
fn tables_and_into_after_insert() {
    let l = labels("insert |");
    assert!(l.contains(&"INTO".into()) && l.contains(&"Customers".into()), "{l:?}");
    assert!(inserts("insert Custo|").first().is_some_and(|i| i.starts_with("dbo.Customers
(") || i.starts_with("dbo.Customers
(")), "{:?}", inserts("insert Custo|"));
}

#[test]
fn a_column_of_a_table_with_an_alias_is_written_with_it() {
    let i = inserts("select top 10 Na| from dbo.Customers AS c");
    assert_eq!(i.first().map(String::as_str), Some("c.Name"), "{i:?}");
}

#[test]
fn system_procedures_after_exec_and_their_parameters() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sys"],
        "objects": [
            {"schema": "dbo", "name": "usp_Sync", "kind": "procedure"},
            {"schema": "sys", "name": "sp_who2", "kind": "procedure", "params": [{"name": "@loginame", "type": "sysname"}]},
            {"schema": "sys", "name": "sp_helptext", "kind": "procedure", "params": [{"name": "@objname", "type": "nvarchar(776)"}, {"name": "@columnname", "type": "sysname"}]}
        ]
    }))
    .unwrap();
    let at = |sql: &str| {
        let caret = sql.find('|').unwrap();
        let text = sql.replace('|', "");
        complete(&text, caret, Some(&s)).items
    };
    let items = at("exec sp_w|");
    assert_eq!(items.first().map(|i| i.label.as_str()), Some("sp_who2"), "{items:?}");
    assert!(items[0].insert.starts_with("sp_who2"), "{:?}", items[0].insert);
    // The database's own procedures come first.
    let all: Vec<String> = at("exec |").into_iter().map(|i| i.label).collect();
    assert!(all.iter().position(|l| l == "usp_Sync") < all.iter().position(|l| l == "sp_who2"), "{all:?}");
    let text = "exec sp_helptext 'dbo.t', ";
    let sig = quillsql::signature::signature_at(text, text.len(), Some(&s)).expect("signature");
    assert_eq!((sig.params.len(), sig.active), (2, 1), "{sig:?}");
}

#[test]
fn built_in_functions_come_from_the_data_file_with_what_they_do() {
    let all = quillsql::functions::all();
    assert!(all.len() >= 180, "{}", all.len());
    for f in all {
        assert!(!f.what.is_empty() && !f.returns.is_empty(), "{f:?}");
    }
    let d = quillsql::functions::describe("dateadd").unwrap();
    assert!(d.starts_with("DATEADD(datepart, number, date)") && d.contains("Adds"), "{d}");
    let text = "select type_name(";
    let sig = quillsql::signature::signature_at(text, text.len(), None).expect("signature");
    assert_eq!(sig.name, "TYPE_NAME");
}

#[test]
fn descriptions_people_gave_objects_columns_and_parameters_are_shown() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo"],
        "objects": [
            {"schema": "dbo", "name": "JitBin", "kind": "table", "description": "Bins of the just-in-time warehouse",
             "columns": [{"name": "Code", "type": "varchar(20)", "description": "Printed on the label"}]},
            {"schema": "dbo", "name": "usp_Move", "kind": "procedure", "description": "Moves a bin",
             "params": [{"name": "@BinId", "type": "int", "description": "The bin to move"}]}
        ]
    }))
    .unwrap();
    let table = quillsql::info::describe_object(&s.objects[0]);
    assert!(table.contains("Bins of the just-in-time warehouse") && !table.contains("Code varchar(20) NULL") && table.contains("-- Printed on the label"), "{table}");
    let procedure = quillsql::info::describe_object(&s.objects[1]);
    assert!(procedure.contains("Moves a bin") && procedure.contains("@BinId: The bin to move"), "{procedure}");
}

#[test]
fn procedures_where_a_statement_starts_without_exec() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sys"],
        "objects": [
            {"schema": "sys", "name": "sp_who2", "kind": "procedure", "params": [{"name": "@loginame", "type": "sysname"}]},
            {"schema": "dbo", "name": "usp_Sync", "kind": "procedure"},
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [{"name": "Name", "type": "int"}]}
        ]
    }))
    .unwrap();
    let first = |sql: &str| {
        let caret = sql.find('|').unwrap();
        let text = sql.replace('|', "");
        complete(&text, caret, Some(&s)).items.into_iter().next().map(|i| i.insert)
    };
    // Tab or Enter brings its parameters, as after EXEC.
    let who = first("sp_w|").unwrap();
    assert!(who.starts_with("sp_who2 @loginame = ") && who.contains("-- sysname"), "{who}");
    assert_eq!(first("select 1;\nusp|").as_deref(), Some("dbo.usp_Sync"));
    // Not inside an expression.
    assert_eq!(first("select * from dbo.Customers where sp_w|"), None);
    let text = "sp_who2 ";
    let sig = quillsql::signature::signature_at(text, text.len(), Some(&s)).expect("signature without EXEC");
    assert_eq!(sig.params, vec!["@loginame sysname".to_string()]);
}

#[test]
fn three_letters_inside_a_built_in_function_select_it() {
    let r = run("select dateadd(day, 1, getdate()), add|");
    assert_eq!(r.selected.map(|i| r.items[i].label.as_str()), Some("DATEADD"), "{:?}", r.items.iter().map(|i| &i.label).collect::<Vec<_>>());
}
