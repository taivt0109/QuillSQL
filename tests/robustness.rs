use quillsql::complete::{Schema, complete_across};
use quillsql::signature::signature_at;
use serde_json::json;

#[test]
fn no_panic_at_any_caret_position_while_typing() {
    let s: Schema = serde_json::from_value(json!({
        "database": "Shop", "defaultSchema": "dbo", "schemas": ["dbo", "sales"], "databases": ["Shop", "Archive"], "linkedServers": ["L1"],
        "objects": [
            {"schema": "dbo", "name": "Customers", "kind": "table", "columns": [{"name": "CustomerId", "type": "int"}, {"name": "Name", "type": "nvarchar(100)"}]},
            {"schema": "sales", "name": "Orders", "kind": "table", "columns": [{"name": "OrderId", "type": "int"}, {"name": "CustomerId", "type": "int"}]},
            {"schema": "dbo", "name": "p", "kind": "procedure", "params": [{"name": "@A", "type": "int"}, {"name": "@B", "type": "int"}]},
            {"schema": "dbo", "name": "f", "kind": "scalar_function", "params": [{"name": "@x", "type": "int"}]}
        ],
        "foreignKeys": [{"schema": "sales", "table": "Orders", "columns": ["CustomerId"], "refSchema": "dbo", "refTable": "Customers", "refColumns": ["CustomerId"]}]
    })).unwrap();
    let other: Schema = serde_json::from_value(json!({"database": "Archive", "defaultSchema": "dbo", "schemas": ["dbo"], "objects": [{"schema": "dbo", "name": "Old", "kind": "table", "columns": [{"name": "Id", "type": "int"}]}]})).unwrap();
    let scripts = [
        "create table #t (Id int not null primary key, Code varchar(10) default ('x'), constraint ck check (Id > 0)); select t.Id from #t t where t.Code like 'a%' order by t.Id desc;",
        "declare @t table (Id int, [Total Sum] money); insert into @t (Id) values (1); select x.Id, x.[Total Sum] from @t x;",
        "select c.*, o.OrderId into #c from dbo.Customers c join sales.Orders o on o.CustomerId = c.CustomerId; select * from #c;",
        "alter table sales.Orders with check add constraint fk foreign key (CustomerId) references dbo.Customers (CustomerId); alter table dbo.Customers alter column Name nvarchar(200) not null; alter table dbo.Customers drop column Name;",
        "create unique nonclustered index ix on sales.Orders (CustomerId desc, OrderId) include (OrderId) where OrderId > 0 with (online = on);",
        "grant select, insert on dbo.Customers to app; revoke execute on schema::dbo from app; deny view definition on dbo.p to app;",
        "merge dbo.Customers as t using (select 1 as CustomerId) as s on s.CustomerId = t.CustomerId when matched and t.Name is null then update set t.Name = 'x' when not matched by target then insert (Name) values ('y') when not matched by source then delete;",
        "exec dbo.p @A = 1, @B = dbo.f(dateadd(day, 1, getdate())); exec @r = dbo.p 1, 2;",
        "select * from Archive.dbo.Old o cross apply (select top (1) * from sales.Orders) x where o.Id in (select CustomerId from dbo.Customers) and exists (select 1) option (recompile, maxdop 1);",
        "if exists (select 1 from L1.db.dbo.t) begin try begin tran; update c set c.Name = 'x' from dbo.Customers c with (nolock, index(ix)); commit end try begin catch rollback end catch",
        "select cast(1 as decimal(10, 2)), convert(nvarchar(20), getdate(), 120), row_number() over (partition by c.Name order by c.CustomerId) from dbo.Customers c group by c.Name having count(*) > 1 union all select 1, 2, 3 except select 1, 2, 3",
        "create procedure dbo.q @Id int, @Out int output as begin set nocount on; select case when @Id is not null then 1 else 0 end; end\nGO\ndrop table if exists #t; truncate table dbo.Customers",
        "select 'unterminated, [also (open \n -- comment ( , \n /* block ( ",
    ];
    for script in scripts {
        for (end, _) in script.char_indices().chain(std::iter::once((script.len(), ' '))) {
            let prefix = &script[..end];
            let caret = prefix.encode_utf16().count();
            let _ = complete_across(prefix, caret, Some(&s), &[&other], &[], Default::default());
            let _ = complete_across(script, caret, Some(&s), &[&other], &[], Default::default());
            let _ = signature_at(prefix, caret, Some(&s));
            let _ = signature_at(script, caret, Some(&s));
        }
    }
}
