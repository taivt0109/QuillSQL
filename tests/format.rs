use quillsql::lexer::{Kind, lex};
use quillsql::{Enclose, Options, TableAlias, format};

fn layout(src: &str) -> String {
    format(src, &Options::default()).unwrap()
}

const CORPUS: &[&str] = &[
    "select a,b from dbo.t where x=1 and y in (1,2) order by a",
    "SELECT a, CASE WHEN x = 1 THEN 'a' WHEN x = 2 THEN 'b' ELSE 'c' END AS k, COUNT(*) OVER (PARTITION BY a ORDER BY b) n FROM t",
    "select * from a inner join b on a.id=b.id and a.x=b.x left outer join c on c.id=a.id cross apply f(a.id) z where a.d between 1 and 2 or a.e=3",
    "with c as (select 1 x), d as (select 2 y) select * from c, d",
    "insert into t (a,b) values (1,'x'),(2,'y')\ninsert t (a) select a from s\nupdate t set a=1,b=2 from t join u on t.id=u.id where u.x=1\ndelete from t where a=1",
    "if exists (select 1 from t) begin print 'x' select 1 end else begin select 2 end",
    "if @a=1 select 1 else if @a=2 select 2 else select 3",
    "begin try select 1/0 end try begin catch throw; end catch",
    "create table dbo.t (id int not null primary key, name nvarchar(50) default N'x', constraint fk foreign key (id) references u (id) on delete set null)",
    "create or alter proc dbo.p @a int = 1, @b varchar(10) with recompile as begin set nocount on; select @a; end\nGO\nexec dbo.p 1",
    "merge into t as a using s as b on a.id=b.id and a.x=1 when matched and b.y=1 then update set a.v=b.v when not matched then insert (id,v) values (b.id,b.v) when not matched by source then delete;",
    "-- header\n\n/* block\n comment */\nselect a, -- first\n b /* mid */ , c\nfrom t -- tail\n\n\nwhere x=1\n-- end\n",
    "declare @s nvarchar(max) = 'it''s é'; select [a]]b], \"q r\", N'x', 0xFF, 1.5e3, .5, $1.50, -1, a - -1, a-1, ~x, t.*, geography::Point(1,2,3).STAsText(), @@rowcount",
    ":setvar db foo\nuse $(db)\nGO\nselect 1\nGO 5\nselect 2",
    "select x.value('(/a)[1]','int') from t cross apply c.nodes('/a') as n(x) for xml path('a')",
    "drop table if exists #t\nselect a into #t from dbo.t with (nolock) option (recompile)\nalter table t drop column x\nalter table t alter column y int",
    "select a from t order by a offset 0 rows fetch next 5 rows only\ngrant select, insert on t to u\nset nocount on\nselect left(a,1), right(a,1) from t",
    "create trigger tr on t after insert, update as if update(a) select 1",
    "declare c cursor for select a from t\nopen c\nfetch next from c into @a\nwhile @@fetch_status=0 begin fetch next from c into @a end\nclose c\ndeallocate c",
    "select (select max(a) from u where u.id=t.id) m from (select 1 id) t where exists (select 1) and a in (select b from c)",
    "select 1\r\nfrom t\r\n",
];

#[test]
fn formats_a_simple_query() {
    assert_eq!(
        layout("select a,b from dbo.t where x=1 and y in (1,2) order by a"),
        "select a,\n       b\nfrom dbo.t\nwhere x = 1\n    and y in (1, 2)\norder by a"
    );
}

#[test]
fn formats_joins_and_case() {
    assert_eq!(
        layout("select a, case when x=1 then 2 else 3 end k from a left join b on a.i=b.i and a.j=b.j"),
        "select a,\n       case\n           when x = 1 then 2\n           else 3\n       end k\nfrom a\nleft join b\n    on a.i = b.i\n    and a.j = b.j"
    );
}

#[test]
fn indents_subqueries() {
    assert_eq!(
        layout("select * from (select a from t) s"),
        "select *\nfrom (\n    select a\n    from t\n) s"
    );
}

#[test]
fn between_and_stays_on_the_line() {
    assert_eq!(
        layout("select 1 where a between 1 and 2 and b=1"),
        "select 1\nwhere a between 1 and 2\n    and b = 1"
    );
}

#[test]
fn over_clause_stays_inline() {
    assert!(layout("select count(*) over (partition by a order by b) from t").contains("over (partition by a order by b)"));
}

#[test]
fn indents_blocks_and_if_bodies() {
    assert_eq!(
        layout("if @a=1 begin select 1 end else select 2"),
        "if @a = 1\nbegin\n    select 1\nend\nelse\n    select 2"
    );
}

#[test]
fn create_table_columns_one_per_line() {
    assert_eq!(
        layout("create table t (id int, name varchar(5))"),
        "create table t (\n    id int,\n    name varchar(5)\n)"
    );
}

#[test]
fn go_and_sqlcmd_stay_alone_on_their_lines() {
    assert_eq!(layout("select 1\nGO\n:setvar a b\nselect 2"), "select 1\nGO\n:setvar a b\nselect 2");
}

#[test]
fn go_with_a_count_and_a_word_named_go_are_distinct() {
    assert_eq!(layout("select 1\nGO 5\nselect 2"), "select 1\nGO 5\nselect 2");
    assert_eq!(lex("go x").unwrap().tokens[0].kind, Kind::Word);
}

#[test]
fn preserves_crlf_and_trailing_newline() {
    assert_eq!(layout("select 1 from t\r\n"), "select 1\r\nfrom t\r\n");
    assert_eq!(layout("select 1 from t"), "select 1\nfrom t");
}

#[test]
fn line_comment_does_not_swallow_the_next_token() {
    assert_eq!(layout("select a -- c\nfrom t"), "select a -- c\nfrom t");
}

#[test]
fn multiline_strings_are_untouched() {
    let out = layout("select 'a\n   b' , c from t");
    assert!(out.contains("'a\n   b'"), "{out}");
}

#[test]
fn unterminated_input_is_refused() {
    assert!(format("select 'abc", &Options::default()).is_err());
    assert!(format("select /* abc", &Options::default()).is_err());
    assert!(format("select [abc", &Options::default()).is_err());
}

#[test]
fn corpus_is_idempotent_and_keeps_every_token() {
    for src in CORPUS {
        let once = format(src, &Options::default()).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(once, layout(&once), "not idempotent for: {src}");
    }
}

#[test]
fn corpus_survives_every_option() {
    for leading_commas in [false, true] {
        for enclose in [None, Some(Enclose::WithBrackets), Some(Enclose::WithoutBrackets)] {
            for alias in [None, Some(TableAlias::AddAs), Some(TableAlias::RemoveAs)] {
                let opts = Options {
                    leading_commas,
                    apply_casing: true,
                    prefix_unicode: true,
                    insert_semicolons: true,
                    enclose_identifiers: enclose,
                    table_alias_style: alias,
                    ..Options::default()
                };
                for src in CORPUS {
                    format(src, &opts).unwrap_or_else(|e| panic!("{src}: {e}"));
                }
            }
        }
    }
}

fn rewrite(src: &str, opts: Options) -> String {
    format(src, &Options { layout: false, ..opts }).unwrap()
}

#[test]
fn no_layout_keeps_whitespace_exactly() {
    for src in CORPUS {
        assert_eq!(rewrite(src, Options::default()), *src);
    }
}

#[test]
fn casing_touches_only_reserved_words() {
    let o = Options { apply_casing: true, ..Options::default() };
    assert_eq!(
        rewrite("select name, t.order, [from], 'from' from t", o),
        "SELECT name, t.order, [from], 'from' FROM t"
    );
}

#[test]
fn prefix_unicode_only_on_variable_assignment() {
    let o = || Options { prefix_unicode: true, ..Options::default() };
    assert_eq!(rewrite("set @a = 'é'", o()), "set @a = N'é'");
    assert_eq!(rewrite("declare @a nvarchar(5) = 'é'", o()), "declare @a nvarchar(5) = N'é'");
    assert_eq!(rewrite("set @a = 'e'", o()), "set @a = 'e'");
    assert_eq!(rewrite("select 'é'", o()), "select 'é'");
}

#[test]
fn without_brackets_keeps_the_ones_that_are_needed() {
    let o = Options { enclose_identifiers: Some(Enclose::WithoutBrackets), ..Options::default() };
    assert_eq!(
        rewrite("select [a], [a b], [order], [1x], [go] from [dbo].[t]", o),
        "select a, [a b], [order], [1x], [go] from dbo.t"
    );
}

#[test]
fn with_brackets_skips_methods_and_variables() {
    let o = || Options { enclose_identifiers: Some(Enclose::WithBrackets), ..Options::default() };
    assert_eq!(rewrite("select a.b from dbo.t", o()), "select [a].[b] from [dbo].[t]");
    assert_eq!(rewrite("select x.value('a','int')", o()), "select [x].value('a','int')");
    assert_eq!(rewrite("select @x.value('a','int')", o()), "select @x.value('a','int')");
    assert_eq!(rewrite("select inserted.a", o()), "select inserted.[a]");
    assert_eq!(rewrite("select * from f(1)", o()), "select * from f(1)");
}

#[test]
fn table_alias_styles() {
    let add = || Options { table_alias_style: Some(TableAlias::AddAs), ..Options::default() };
    let remove = || Options { table_alias_style: Some(TableAlias::RemoveAs), ..Options::default() };
    assert_eq!(
        rewrite("select a b from t x join dbo.u y on 1=1", add()),
        "select a b from t AS x join dbo.u AS y on 1=1"
    );
    assert_eq!(
        rewrite("select a as b from t as x join u as y on 1=1", remove()),
        "select a as b from t x join u y on 1=1"
    );
    assert_eq!(rewrite("select * from t with (nolock) where 1=1", add()), "select * from t with (nolock) where 1=1");
    assert_eq!(
        rewrite("select * from t for system_time as of '2020' as x", remove()),
        "select * from t for system_time as of '2020' x"
    );
    assert_eq!(
        rewrite("select cast(a as int) from (select 1 a) as s", remove()),
        "select cast(a as int) from (select 1 a) s"
    );
}

fn semis(src: &str) -> String {
    rewrite(src, Options { insert_semicolons: true, ..Options::default() })
}

#[test]
fn semicolons_terminate_statements() {
    assert_eq!(semis("select 1\nselect 2"), "select 1;\nselect 2;");
    assert_eq!(semis("select 1 -- c\nGO\nselect 2"), "select 1; -- c\nGO\nselect 2;");
    assert_eq!(semis("select 1;"), "select 1;");
    assert_eq!(semis("begin select 1 end"), "begin select 1; end");
    assert_eq!(semis("set nocount on\nselect 1"), "set nocount on;\nselect 1;");
}

#[test]
fn semicolons_stay_out_of_statement_interiors() {
    for (src, want) in [
        ("if @a=1 select 1 else select 2", "if @a=1 select 1; else select 2;"),
        ("insert t select 1", "insert t select 1;"),
        ("select 1 union all select 2", "select 1 union all select 2;"),
        ("with c as (select 1 x) select * from c", "with c as (select 1 x) select * from c;"),
        ("update t set a=1", "update t set a=1;"),
        ("drop table if exists t", "drop table if exists t;"),
        ("alter table t drop column x", "alter table t drop column x;"),
        ("select case when a=1 then 2 else 3 end", "select case when a=1 then 2 else 3 end;"),
        ("declare c cursor for select 1", "declare c cursor for select 1;"),
        (
            "select a from t order by a offset 0 rows fetch next 5 rows only",
            "select a from t order by a offset 0 rows fetch next 5 rows only;",
        ),
        ("create view v as select 1", "create view v as select 1;"),
        (
            "create trigger x on t after insert, update as if update(a) select 1",
            "create trigger x on t after insert, update as if update(a) select 1;",
        ),
        (
            "merge t using s on 1=1 when matched then update set a=1 when not matched then insert (a) values (1);",
            "merge t using s on 1=1 when matched then update set a=1 when not matched then insert (a) values (1);",
        ),
        ("select throw from t", "select throw from t;"),
    ] {
        assert_eq!(semis(src), want);
    }
}

#[test]
fn format_off_regions_are_kept_verbatim() {
    let src = "select a,b from t;\n-- quill-format-off\nselect   x ,y   from   u;\n-- quill-format-on\nselect c,d from v;\n";
    let out = layout(src);
    assert!(out.contains("-- quill-format-off\nselect   x ,y   from   u;\n-- quill-format-on\n"), "{out}");
    assert!(!out.starts_with("select a,b"), "{out}");
    assert!(!out.contains("select c,d"), "{out}");
}
