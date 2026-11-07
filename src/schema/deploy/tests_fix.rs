//! Deployment details checked against SQL Compare's behaviour (fix round).

use super::*;

fn col(name: &str, ty: &str) -> Column {
    Column { name: name.into(), data_type: DataType { name: ty.into(), ..Default::default() }, nullable: true, ..Default::default() }
}

fn sized(name: &str, ty: &str, len: i32) -> Column {
    let mut c = col(name, ty);
    c.data_type.length = Some(len);
    c
}

fn table(name: &str, columns: Vec<Column>) -> Table {
    Table { schema: "dbo".into(), name: name.into(), columns, lock_escalation: "TABLE".into(), ..Default::default() }
}

fn pk(name: &str, column: &str) -> Index {
    Index {
        name: name.into(),
        kind: IndexKind::Clustered,
        primary_key: true,
        unique: true,
        columns: vec![IndexColumn { name: column.into(), descending: false }],
        allow_row_locks: true,
        allow_page_locks: true,
        ..Default::default()
    }
}

fn db(tables: Vec<Table>) -> Database {
    Database { tables, ..Default::default() }
}

fn settings() -> ScriptSettings {
    ScriptSettings { transaction_isolation_level: "SERIALIZABLE".into(), source: "s".into(), target: "t".into(), target_database: "t".into(), target_major_version: 0 }
}

fn script_with(src: &Database, tgt: &Database, opts: &Options, mappings: &Mappings) -> DeploymentScript {
    let c = super::super::compare::compare(src, tgt, opts, mappings);
    generate(&c, src, tgt, opts, mappings, &settings()).unwrap()
}

fn script(src: &Database, tgt: &Database, opts: &Options) -> DeploymentScript {
    script_with(src, tgt, opts, &Mappings::default())
}

fn has_warning(s: &DeploymentScript, text: &str) -> bool {
    s.warnings.iter().any(|w| w.message.contains(text))
}

fn in_order(sql: &str, parts: &[&str]) {
    let pos: Vec<usize> = parts.iter().map(|p| sql.find(p).unwrap_or_else(|| panic!("missing {p}\n{sql}"))).collect();
    assert!(pos.windows(2).all(|w| w[0] < w[1]), "{parts:?}\n{sql}");
}

fn view(name: &str, text: &str) -> View {
    View { schema: "dbo".into(), name: name.into(), definition: text.into(), ..Default::default() }
}

fn dep(from: &str, to: &str, strong: bool) -> Dependency {
    Dependency { from: ObjectName::new("dbo", from), to: ObjectName::new("dbo", to), strong }
}

#[test]
fn varchar_to_nvarchar_is_altered_in_place() {
    let src = db(vec![table("T", vec![sized("B", "nvarchar", 30)])]);
    let tgt = db(vec![table("T", vec![sized("B", "varchar", 20)])]);
    let s = script(&src, &tgt, &Options::default_set());
    assert!(s.sql.contains("ALTER TABLE [dbo].[T] ALTER COLUMN [B] [nvarchar] (30) NULL") && !s.sql.contains("RG_Recovery"), "{}", s.sql);
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
}

#[test]
fn becoming_system_versioned_adds_the_period_before_the_copy() {
    let mut a = col("Id", "int");
    a.nullable = false;
    let mut t = table("T", vec![a.clone(), col("A", "int")]);
    t.indexes.push(pk("PK_T", "Id"));
    let tgt = db(vec![t.clone()]);
    let period = |n: &str, g: &str| {
        let mut c = col(n, "datetime2");
        c.nullable = false;
        c.generated_always = Some(g.into());
        c
    };
    t.columns.push(period("ValidFrom", "AS_ROW_START"));
    t.columns.push(period("ValidTo", "AS_ROW_END"));
    t.temporal = Some(Temporal { history_table: ObjectName::new("dbo", "T_History"), period_start: "ValidFrom".into(), period_end: "ValidTo".into(), ..Default::default() });
    let mut h = table("T_History", vec![a, col("A", "int"), col("ValidFrom", "datetime2"), col("ValidTo", "datetime2")]);
    h.history_table = true;
    let src = db(vec![t, h]);
    let s = script(&src, &tgt, &Options::default_set());
    in_order(&s.sql, &["CREATE TABLE [dbo].[RG_Recovery_1_T]", "ALTER TABLE [dbo].[RG_Recovery_1_T] ADD PERIOD FOR SYSTEM_TIME ([ValidFrom], [ValidTo])", "INSERT INTO [dbo].[RG_Recovery_1_T]([Id], [A])", "SYSTEM_VERSIONING = ON"]);
    assert!(s.sql.contains("CREATE TABLE [dbo].[T_History]"), "{}", s.sql);
}

#[test]
fn views_on_changed_tables_are_refreshed() {
    let mut src = db(vec![table("T", vec![col("a", "int"), col("b", "int")])]);
    let mut tgt = db(vec![table("T", vec![col("a", "int")])]);
    for d in [&mut src, &mut tgt] {
        d.views.push(view("V", "CREATE VIEW dbo.V AS SELECT * FROM dbo.T"));
        d.views.push(view("VS", "CREATE VIEW dbo.VS AS SELECT * FROM dbo.S"));
        d.views.push(view("V2", "CREATE VIEW dbo.V2 AS SELECT * FROM dbo.V"));
        let mut bound = view("B", "CREATE VIEW dbo.B WITH SCHEMABINDING AS SELECT a FROM dbo.T");
        bound.schema_bound = true;
        d.views.push(bound);
        d.synonyms.push(Synonym { schema: "dbo".into(), name: "S".into(), base_object: "[dbo].[T]".into(), ..Default::default() });
        d.dependencies = vec![dep("V", "T", false), dep("S", "T", false), dep("VS", "S", false), dep("V2", "V", false), dep("B", "T", true)];
    }
    let s = script(&src, &tgt, &Options::default_set());
    in_order(&s.sql, &["ALTER TABLE [dbo].[T] ADD", "PRINT N'Refreshing [dbo].[V]'\r\nGO\r\nEXEC sp_refreshview N'[dbo].[V]'", "EXEC sp_refreshview N'[dbo].[V2]'"]);
    assert!(s.sql.contains("EXEC sp_refreshview N'[dbo].[VS]'") && !s.sql.contains("sp_refreshview N'[dbo].[B]'"), "{}", s.sql);
    let s = script(&src, &tgt, &Options::default_set().with(Opt::ObjectExistenceChecks));
    assert!(s.sql.contains("IF OBJECT_ID(N'[dbo].[V]', 'V') IS NOT NULL\r\nEXEC sp_refreshview N'[dbo].[V]'"), "{}", s.sql);
}

#[test]
fn schema_bound_dependants_warn_when_brought_back_unchanged() {
    let mut src = db(vec![table("T", vec![col("a", "bigint")])]);
    let mut tgt = db(vec![table("T", vec![col("a", "int")])]);
    for d in [&mut src, &mut tgt] {
        let mut v = view("V", "CREATE VIEW dbo.V WITH SCHEMABINDING AS SELECT a FROM dbo.T");
        v.schema_bound = true;
        d.views.push(v);
        d.dependencies.push(dep("V", "T", true));
    }
    let s = script(&src, &tgt, &Options::default_set());
    assert!(has_warning(&s, "[dbo].[V] will have its schemabinding dropped and reapplied."), "{:?}", s.warnings);
}

#[test]
fn mapped_tables_are_renamed_first() {
    let mut a = table("Client", vec![col("Id", "int"), col("Name", "nvarchar")]);
    a.indexes.push(pk("PK_Client", "Id"));
    let mut b = a.clone();
    b.name = "Customer".into();
    b.schema = "old".into();
    b.columns.pop();
    let src = db(vec![a]);
    let tgt = db(vec![b]);
    let mappings = Mappings { schemas: vec![], tables: vec![super::super::compare::TableMapping { source: ObjectName::new("dbo", "Client"), target: ObjectName::new("old", "Customer"), columns: vec![] }] };
    let mut c = super::super::compare::compare(&src, &tgt, &Options::default_set(), &mappings);
    // The model reports the pair as Different (Name / Owner); force it for this test.
    for d in c.differences.iter_mut().filter(|d| d.object_type == ObjectType::Table) {
        d.kind = DiffKind::Different;
        d.selected = true;
        d.target_name = Some(ObjectName::new("old", "Customer"));
    }
    let opts = Options::default_set().with(Opt::ObjectExistenceChecks);
    let s = generate(&c, &src, &tgt, &opts, &mappings, &settings()).unwrap();
    in_order(
        &s.sql,
        &[
            "BEGIN TRANSACTION",
            "IF (OBJECT_ID(N'[old].[Customer]', 'U') IS NOT NULL) AND (OBJECT_ID(N'[old].[Client]', 'U') IS NULL)\r\nEXEC sp_rename N'[old].[Customer]', N'Client', N'OBJECT'",
            "IF (OBJECT_ID(N'[old].[Client]', 'U') IS NOT NULL) AND (OBJECT_ID(N'[dbo].[Client]', 'U') IS NULL)\r\nALTER SCHEMA [dbo] TRANSFER [old].[Client]",
            "ALTER TABLE [dbo].[Client] ADD",
        ],
    );
    assert!(!s.sql.contains("[old].[Customer] ADD"), "{}", s.sql);
}

#[test]
fn windows_and_entra_users_get_their_login() {
    let user = |name: &str, kind: &str, login: Option<&str>| Principal { name: name.into(), kind: kind.into(), login: login.map(Into::into), ..Default::default() };
    let mut src = db(vec![]);
    src.principals = vec![user("DEV\\svc", "WINDOWS_USER", Some("DEV\\svc")), user("ann@x.com", "EXTERNAL_USER", Some("ann@x.com")), user("bob@x.com", "EXTERNAL_USER", None)];
    let s = script(&src, &db(vec![]), &Options::default_set());
    assert!(s.sql.contains("IF NOT EXISTS (SELECT * FROM master.dbo.syslogins WHERE loginname = N'DEV\\svc')\r\nCREATE LOGIN [DEV\\svc] FROM WINDOWS\r\nGO"), "{}", s.sql);
    assert!(s.sql.contains("CREATE USER [DEV\\svc] FOR LOGIN [DEV\\svc]"), "{}", s.sql);
    assert!(s.sql.contains("CREATE LOGIN [ann@x.com] FROM EXTERNAL PROVIDER") && s.sql.contains("CREATE USER [ann@x.com] FOR LOGIN [ann@x.com]"), "{}", s.sql);
    assert!(s.sql.contains("CREATE USER [bob@x.com] FROM EXTERNAL PROVIDER") && !s.sql.contains("CREATE LOGIN [bob@x.com]"), "{}", s.sql);
    // On-premises targets before Azure have no Entra users.
    let mut tgt = db(vec![]);
    tgt.server = ServerInfo { major_version: 15, product_version: "15.0.4000.0".into(), edition: String::new(), engine_edition: 3 };
    let s = script(&src, &tgt, &Options::default_set());
    assert!(has_warning(&s, "The user type MicrosoftEntraUser is not supported in the target database version."), "{:?}", s.warnings);
    // Azure SQL Database takes Entra users but cannot map them to logins.
    tgt.server = ServerInfo { major_version: 12, product_version: "12.0.2000.8".into(), edition: String::new(), engine_edition: 5 };
    let s = script(&src, &tgt, &Options::default_set());
    assert!(has_warning(&s, "The Microsoft Entra user ann@x.com is mapped to a server-level login"), "{:?}", s.warnings);
}

#[test]
fn rerunnable_scripts_drop_before_creating() {
    let mut src = db(vec![table("T", vec![col("a", "int")])]);
    src.views.push(view("V", "CREATE VIEW dbo.V AS SELECT 1 AS c"));
    src.tables[0].triggers.push(Trigger { name: "tr".into(), definition: "CREATE TRIGGER dbo.tr ON dbo.T FOR INSERT AS SELECT 1".into(), ..Default::default() });
    src.extended_properties.push(ExtendedProperty { class: "DATABASE".into(), name: "p".into(), value: Some("v".into()), ..Default::default() });
    let mut tgt = db(vec![table("T", vec![col("a", "int")])]);
    tgt.tables[0].triggers.push(Trigger { name: "tr".into(), definition: "CREATE TRIGGER dbo.tr ON dbo.T FOR INSERT AS SELECT 2".into(), ..Default::default() });
    tgt.extended_properties.push(ExtendedProperty { class: "DATABASE".into(), name: "p".into(), value: Some("old".into()), ..Default::default() });
    let s = script(&src, &tgt, &Options::default_set().with(Opt::DropAndCreateInsteadOfAlter));
    in_order(&s.sql, &["IF OBJECT_ID(N'[dbo].[V]', 'V') IS NOT NULL\r\nDROP VIEW [dbo].[V]", "CREATE VIEW [dbo].[V]"]);
    in_order(&s.sql, &["IF OBJECT_ID(N'[dbo].[tr]', 'TR') IS NOT NULL\r\nDROP TRIGGER [dbo].[tr]", "CREATE TRIGGER [dbo].[tr]"]);
    assert!(!s.sql.contains("ALTER TRIGGER") && !s.sql.contains("sp_updateextendedproperty"), "{}", s.sql);
    in_order(&s.sql, &["EXEC sp_dropextendedproperty N'p'", "EXEC sp_addextendedproperty N'p', N'v'"]);
}

#[test]
fn triggers_use_create_or_alter() {
    let mut src = db(vec![table("T", vec![col("a", "int")])]);
    src.tables[0].triggers.push(Trigger { name: "tr".into(), definition: "CREATE TRIGGER dbo.tr ON dbo.T FOR INSERT AS SELECT 1".into(), ..Default::default() });
    let mut tgt = db(vec![table("T", vec![col("a", "int")])]);
    tgt.tables[0].triggers.push(Trigger { name: "tr".into(), definition: "CREATE TRIGGER dbo.tr ON dbo.T FOR INSERT AS SELECT 2".into(), ..Default::default() });
    let s = script(&src, &tgt, &Options::default_set().with(Opt::CreateOrAlterInsteadOfAlter));
    assert!(s.sql.contains("CREATE OR ALTER TRIGGER [dbo].[tr]"), "{}", s.sql);
}

#[test]
fn explicit_owners_are_deployed() {
    let mut a = table("T", vec![col("a", "int")]);
    a.owner = Some("Bob".into());
    let src = db(vec![a.clone(), table("N", vec![col("a", "int")])]);
    let mut b = a.clone();
    b.owner = None;
    let tgt = db(vec![b]);
    let s = script(&src, &tgt, &Options::default_set());
    assert!(s.sql.contains("PRINT N'Altering schema object authorizations'") || s.sql.contains("ALTER AUTHORIZATION ON [dbo].[T] TO [Bob]"), "{}", s.sql);
    assert!(s.sql.contains("ALTER AUTHORIZATION ON [dbo].[T] TO [Bob]") && !s.sql.contains("ON [dbo].[N]"), "{}", s.sql);
    let s = script(&tgt, &src, &Options::default_set());
    assert!(s.sql.contains("ALTER AUTHORIZATION ON [dbo].[T] TO SCHEMA OWNER"), "{}", s.sql);
    assert!(!script(&src, &tgt, &Options::default_set().with(Opt::IgnoreSchemaObjectAuthorization)).sql.contains("ALTER AUTHORIZATION"));
}

#[test]
fn file_tables_and_large_values() {
    let mut f = table("Docs", vec![col("stream_id", "uniqueidentifier")]);
    f.file_table = Some(FileTable { directory: "Docs".into(), filename_collation: "Latin1_General_CI_AS".into(), primary_key: None });
    let mut big = table("Big", vec![col("a", "varchar")]);
    big.large_value_types_out_of_row = true;
    let src = db(vec![f.clone(), big.clone()]);
    let s = script(&src, &db(vec![]), &Options::default_set());
    assert!(s.sql.contains("CREATE TABLE [dbo].[Docs] AS FILETABLE\r\nWITH\r\n(\r\n\tFILETABLE_DIRECTORY = N'Docs', FILETABLE_COLLATE_FILENAME = Latin1_General_CI_AS\r\n)"), "{}", s.sql);
    assert!(s.sql.contains("EXEC sp_tableoption N'[dbo].[Big]', N'large value types out of row', 1;"), "{}", s.sql);
    let mut old = db(vec![]);
    old.server = ServerInfo { major_version: 10, product_version: "10.50.6000.34".into(), edition: String::new(), engine_edition: 3 };
    let s = script(&src, &old, &Options::default_set());
    assert!(has_warning(&s, "The filetable [dbo].[Docs] is not supported in the target database version."), "{:?}", s.warnings);
    assert!(has_warning(&s, "will no longer support SQL Server 2008 and SQL Server 2008R2"), "{:?}", s.warnings);
    // The option alone changes.
    let mut off = big.clone();
    off.large_value_types_out_of_row = false;
    let s = script(&db(vec![off]), &db(vec![big]), &Options::default_set());
    assert!(s.sql.contains("EXEC sp_tableoption N'[dbo].[Big]', N'large value types out of row', 0;"), "{}", s.sql);
}

#[test]
fn role_members_follow_the_role() {
    let p = |name: &str, kind: &str, member_of: &[&str]| Principal { name: name.into(), kind: kind.into(), member_of: member_of.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let mut src = db(vec![]);
    src.principals = vec![p("R", "DATABASE_ROLE", &[]), p("u", "SQL_USER", &["R", "db_datareader"])];
    let mut tgt = db(vec![]);
    tgt.principals = vec![p("R", "DATABASE_ROLE", &[]), p("u", "SQL_USER", &[]), p("x", "SQL_USER", &["R"])];
    let s = script(&src, &tgt, &Options::default_set());
    assert!(s.sql.contains("ALTER ROLE [R] ADD MEMBER [u]") && s.sql.contains("ALTER ROLE [R] DROP MEMBER [x]"), "{}", s.sql);
    assert!(s.sql.contains("ALTER ROLE [db_datareader] ADD MEMBER [u]"), "{}", s.sql);
}

#[test]
fn history_rows_get_the_default_too() {
    let mut id = col("Id", "int");
    id.nullable = false;
    let mut t = table("T", vec![id, col("A", "int")]);
    t.indexes.push(pk("PK_T", "Id"));
    t.temporal = Some(Temporal { history_table: ObjectName::new("dbo", "T_History"), ..Default::default() });
    let tgt = db(vec![t.clone()]);
    t.columns[1].nullable = false;
    t.columns[1].default = Some(DefaultConstraint { name: "DF_A".into(), definition: "((0))".into(), ..Default::default() });
    let src = db(vec![t]);
    let s = script(&src, &tgt, &Options::default_set());
    in_order(&s.sql, &["SET (SYSTEM_VERSIONING = OFF)", "UPDATE [dbo].[T_History] SET [A] = ((0)) WHERE [A] IS NULL", "UPDATE [dbo].[T] SET [A]=DEFAULT", "SET (SYSTEM_VERSIONING = ON", "ALTER TABLE [dbo].[T] ALTER COLUMN [A] [int] NOT NULL"]);
    assert!(has_warning(&s, "The column [A] on history table [dbo].[T_History] is being marked as NOT NULL."), "{:?}", s.warnings);
}

#[test]
fn compression_rebuild_keeps_partition_groups() {
    let mut t = table("T", vec![col("a", "int")]);
    t.storage.data_space = Some("PS".into());
    t.storage.partition_column = Some("a".into());
    let tgt = db(vec![t.clone()]);
    t.storage.compression = vec![
        PartitionCompression { partition: 1, compression: "PAGE".into() },
        PartitionCompression { partition: 2, compression: "ROW".into() },
    ];
    let s = script(&db(vec![t]), &tgt, &Options::default_set().without(Opt::IgnoreFileGroups));
    assert!(s.sql.contains("REBUILD PARTITION = ALL\r\nWITH (DATA_COMPRESSION = ROW ON PARTITIONS (2), DATA_COMPRESSION = PAGE ON PARTITIONS (1))"), "{}", s.sql);
}

#[test]
fn non_string_extended_property_values() {
    let p = ExtendedProperty { class: "DATABASE".into(), name: "n".into(), value: Some("5".into()), value_type: Some("int".into()), ..Default::default() };
    let mut src = db(vec![]);
    src.extended_properties.push(p);
    let s = script(&src, &db(vec![]), &Options::default_set());
    assert!(s.sql.contains("DECLARE @xp int\r\nSELECT @xp=5\r\nEXEC sp_addextendedproperty N'n', @xp, NULL"), "{}", s.sql);
}

#[test]
fn nonclustered_columnstore_lists_its_columns() {
    let mut t = table("T", vec![col("Id", "int"), col("A", "int")]);
    t.indexes.push(Index { name: "NCCI_T".into(), kind: IndexKind::NonclusteredColumnstore, included: vec!["A".into()], allow_row_locks: true, allow_page_locks: true, ..Default::default() });
    let s = script(&db(vec![t]), &db(vec![]), &Options::default_set());
    assert!(s.sql.contains("CREATE NONCLUSTERED COLUMNSTORE INDEX [NCCI_T] ON [dbo].[T] ([A])"), "{}", s.sql);
}

#[test]
fn constraints_that_call_a_recreated_function_come_back() {
    let f = |body: &str| Routine { schema: "dbo".into(), name: "f".into(), kind: RoutineKind::ScalarFunction, definition: format!("CREATE FUNCTION dbo.f() RETURNS int AS BEGIN RETURN {body} END"), ..Default::default() };
    let mut t = table("T", vec![col("a", "int"), col("c", "int")]);
    t.check_constraints.push(CheckConstraint { name: "CK".into(), definition: "([a]>[dbo].[f]())".into(), ..Default::default() });
    t.columns[1].computed = Some(Computed { definition: "([dbo].[f]())".into(), persisted: false });
    let mut src = db(vec![t.clone()]);
    let mut tgt = db(vec![t]);
    src.routines.push(f("1"));
    tgt.routines.push(f("2"));
    for d in [&mut src, &mut tgt] {
        d.dependencies.push(dep("T", "f", true));
    }
    let s = script(&src, &tgt, &Options::default_set().with(Opt::DropAndCreateInsteadOfAlter));
    in_order(&s.sql, &["DROP CONSTRAINT [CK]", "ALTER TABLE [dbo].[T] DROP COLUMN [c]", "DROP FUNCTION [dbo].[f]", "CREATE FUNCTION [dbo].[f]", "ALTER TABLE [dbo].[T] ADD [c] AS", "ADD CONSTRAINT [CK] CHECK"]);
    // Without IncludeDependencies the table is only reported.
    let s = script(&src, &tgt, &Options::default_set().with(Opt::DropAndCreateInsteadOfAlter).without(Opt::IncludeDependencies));
    assert!(!s.sql.contains("DROP CONSTRAINT [CK]") && has_warning(&s, "aren't included in the deployment:\r\n\r\n[dbo].[T]"), "{}\n{:?}", s.sql, s.warnings);
}

#[test]
fn roles_on_old_servers_change_before_the_transaction() {
    let p = |name: &str, kind: &str, member_of: &[&str]| Principal { name: name.into(), kind: kind.into(), member_of: member_of.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let mut src = db(vec![]);
    src.principals = vec![p("R", "DATABASE_ROLE", &[]), p("u", "SQL_USER", &["R"])];
    let mut tgt = db(vec![]);
    tgt.server = ServerInfo { major_version: 10, product_version: "10.50.6000.34".into(), edition: String::new(), engine_edition: 3 };
    let s = script(&src, &tgt, &Options::default_set());
    in_order(&s.sql, &["CREATE ROLE [R]", "sp_addrolemember N'R', N'u'", "BEGIN TRANSACTION"]);
    tgt.server = ServerInfo::default();
    let s = script(&src, &tgt, &Options::default_set());
    in_order(&s.sql, &["BEGIN TRANSACTION", "CREATE ROLE [R]", "ALTER ROLE [R] ADD MEMBER [u]"]);
}

#[test]
fn referencing_columns_follow_a_retyped_key() {
    let mut id = col("Id", "bigint");
    id.nullable = false;
    let mut parent = table("P", vec![id]);
    parent.indexes.push(pk("PK_P", "Id"));
    let mut child = table("C", vec![col("PId", "int")]);
    child.foreign_keys.push(ForeignKey { name: "FK_C_P".into(), columns: vec!["PId".into()], referenced_table: ObjectName::new("dbo", "P"), referenced_columns: vec!["Id".into()], on_delete: "NO_ACTION".into(), on_update: "NO_ACTION".into(), ..Default::default() });
    let src = db(vec![parent.clone(), child.clone()]);
    parent.columns[0].data_type.name = "int".into();
    let tgt = db(vec![parent, child]);
    let mut c = super::super::compare::compare(&src, &tgt, &Options::default_set(), &Mappings::default());
    // Only the parent is deployed.
    for d in c.differences.iter_mut() {
        d.selected = d.name.name == "P";
    }
    let s = generate(&c, &src, &tgt, &Options::default_set(), &Mappings::default(), &settings()).unwrap();
    in_order(&s.sql, &["DROP CONSTRAINT [FK_C_P]", "ALTER TABLE [dbo].[P] ALTER COLUMN [Id] [bigint] NOT NULL", "ALTER TABLE [dbo].[C] ALTER COLUMN [PId] [bigint] NULL", "ADD CONSTRAINT [FK_C_P] FOREIGN KEY"]);
}
