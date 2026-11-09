//! Every option on the Options tab, checked against what it is documented to do. An Ignore
//! option must hide its difference from the comparison, from the side-by-side SQL and from the
//! deployment script; a behavior option must change the script (or the comparison) as described.
//! A visible option without a case here fails `every_visible_option_has_a_case`.

use super::compare::{Comparison, DiffKind, Mappings, compare};
use super::deploy::{DeploymentScript, ScriptSettings, generate};
use super::options::{ALL, Group, Opt, Options};
use super::linediff;
use crate::catalog::*;

/// What one option is checked for.
enum Case {
    /// The two sides differ only in what the option ignores. `view`: the side-by-side SQL must
    /// match too (false for text options, where the definitions are shown as they are, and for
    /// names, which the view shows as named). `marker`: text the deployment must not contain when
    /// the table differs for another reason.
    Ignore { change: fn(&mut Database), base: fn(&mut Database), view: bool, marker: Option<&'static str> },
    /// A check of its own.
    Check(fn()),
    /// Does nothing the engine can show; the reason.
    NoOp(&'static str),
}

fn nothing(_: &mut Database) {}

fn case(o: Opt) -> Case {
    use Case::*;
    let ignore = |change: fn(&mut Database), marker: Option<&'static str>| Ignore { change, base: nothing, view: true, marker };
    match o {
        Opt::IgnoreSchemaObjectAuthorization => ignore(|d| t(d).owner = Some("app".into()), Some("AUTHORIZATION")),
        Opt::IgnoreBindings => ignore(|d| t(d).columns[1].bound_default = Some("[dbo].[D]".into()), Some("sp_bindefault")),
        Opt::IgnoreCertificatesAndCryptoKeys => ignore(
            |d| d.certificates.push(Certificate { name: "C".into(), subject: "s".into(), ..Default::default() }),
            Some("CERTIFICATE"),
        ),
        Opt::IgnoreChangeTracking => ignore(|d| t(d).change_tracking = Some(ChangeTracking { track_columns_updated: false }), Some("CHANGE_TRACKING")),
        Opt::IgnoreChecks => ignore(|d| t(d).check_constraints.push(check()), Some("CK_T")),
        Opt::IgnoreCollations => ignore(|d| t(d).columns[2].collation = Some("Latin1_General_BIN".into()), Some("Latin1_General_BIN")),
        Opt::IgnoreComments => Ignore { change: |d| r(d).definition = "CREATE PROCEDURE dbo.R AS\n-- note\nSELECT 1".into(), base: nothing, view: false, marker: None },
        Opt::IgnoreConstraintNames => Ignore { change: |d| t(d).indexes[0].name = "PK_T2".into(), base: nothing, view: false, marker: Some("PK_T2") },
        Opt::IgnoreDataCompression => ignore(
            |d| t(d).storage.compression = vec![PartitionCompression { partition: 1, compression: "PAGE".into() }],
            Some("DATA_COMPRESSION"),
        ),
        Opt::IgnoreDatabaseAndServerName => Ignore { change: |d| d.synonyms[0].base_object = "[db2].[dbo].[T]".into(), base: nothing, view: false, marker: None },
        Opt::IgnoreTriggerOrder => Ignore {
            change: |d| t(d).triggers[0].order = vec![("INSERT".into(), "First".into())],
            base: |d| t(d).triggers.push(trigger("TR_T")),
            view: true,
            marker: Some("sp_settriggerorder"),
        },
        Opt::IgnoreTriggers => ignore(|d| t(d).triggers.push(trigger("TR_T")), Some("TR_T")),
        Opt::IgnoreQueueEventNotifications => Ignore {
            change: |d| {
                d.queues[0].event_notifications.push(EventNotification {
                    name: "EN".into(),
                    queue: Some(ObjectName::new("dbo", "Q")),
                    events: vec!["QUEUE_ACTIVATION".into()],
                    service: "//S".into(),
                    broker_instance: "current database".into(),
                    ..Default::default()
                })
            },
            base: |d| d.queues.push(Queue { schema: "dbo".into(), name: "Q".into(), ..Default::default() }),
            view: true,
            marker: Some("EVENT NOTIFICATION"),
        },
        Opt::IgnoreExtendedProperties => ignore(
            |d| {
                d.extended_properties.push(ExtendedProperty {
                    class: "TABLE".into(),
                    schema: Some("dbo".into()),
                    object: Some("T".into()),
                    name: "MS_Description".into(),
                    value: Some("x".into()),
                    ..Default::default()
                })
            },
            Some("sp_addextendedproperty"),
        ),
        Opt::IgnoreFileGroups => ignore(|d| t(d).storage.data_space = Some("FG1".into()), Some("FG1")),
        Opt::IgnoreFillFactor => ignore(|d| t(d).indexes[0].fill_factor = 80, Some("FILLFACTOR")),
        Opt::IgnoreKeys => ignore(|d| t(d).foreign_keys.push(fk()), Some("FK_T_P")),
        Opt::IgnoreFullTextIndexing => Ignore {
            change: |d| {
                t(d).fulltext_index = Some(FullTextIndex {
                    catalog: "FC".into(),
                    key_index: "PK_T".into(),
                    change_tracking: "AUTO".into(),
                    stoplist: "SYSTEM".into(),
                    columns: vec![FullTextColumn { name: "Name".into(), type_column: None, language: 1033 }],
                    ..Default::default()
                })
            },
            base: |d| d.fulltext_catalogs.push(FullTextCatalog { name: "FC".into(), ..Default::default() }),
            view: true,
            marker: Some("FULLTEXT INDEX"),
        },
        // Column definitions are shown as each side has them (IDENTITY, seed, NULL / NOT NULL).
        Opt::IgnoreIdentityProperties => Ignore { change: |d| t(d).columns[0].identity = Some(identity("1")), base: nothing, view: false, marker: Some("IDENTITY") },
        Opt::IgnoreIdentitySeedAndIncrement => Ignore {
            change: |d| t(d).columns[0].identity = Some(identity("100")),
            base: |d| t(d).columns[0].identity = Some(identity("1")),
            view: false,
            marker: Some("100"),
        },
        Opt::IgnoreIndexes => ignore(|d| t(d).indexes.push(index("IX_T_P")), Some("IX_T_P")),
        Opt::IgnoreInsteadOfTriggers => ignore(|d| t(d).triggers.push(Trigger { instead_of: true, ..trigger("TR_I") }), Some("TR_I")),
        Opt::IgnoreIndexLockProperties => ignore(|d| t(d).indexes[0].allow_page_locks = false, Some("ALLOW_PAGE_LOCKS")),
        Opt::IgnoreNocheckAndWithNocheck => Ignore {
            change: |d| t(d).foreign_keys[0].disabled = true,
            base: |d| t(d).foreign_keys.push(fk()),
            view: true,
            marker: Some("NOCHECK"),
        },
        Opt::IgnoreNotForReplication => Ignore {
            change: |d| t(d).foreign_keys[0].not_for_replication = true,
            base: |d| t(d).foreign_keys.push(fk()),
            view: true,
            marker: Some("NOT FOR REPLICATION"),
        },
        Opt::IgnoreNullability => Ignore { change: |d| t(d).columns[1].nullable = false, base: nothing, view: false, marker: Some("[P] [int] NOT NULL") },
        Opt::IgnorePerformanceIndexes => ignore(|d| t(d).indexes.push(index("IX_T_P")), Some("IX_T_P")),
        Opt::IgnorePermissions => ignore(
            |d| {
                d.permissions.push(Permission {
                    class: "OBJECT".into(),
                    schema: Some("dbo".into()),
                    object: Some("T".into()),
                    grantee: "app".into(),
                    permission: "SELECT".into(),
                    state: "GRANT".into(),
                    ..Default::default()
                })
            },
            Some("GRANT"),
        ),
        Opt::IgnoreReplicationTriggers => ignore(|d| t(d).triggers.push(trigger("MSmerge_ins_T")), Some("MSmerge")),
        Opt::IgnoreQuotedIdentifiersAndAnsiNullSettings => ignore(|d| r(d).settings.quoted_identifier = false, Some("QUOTED_IDENTIFIER OFF")),
        Opt::IgnoreSensitivityClassification => ignore(
            |d| t(d).columns[2].sensitivity = Some(Sensitivity { label: Some("Confidential".into()), ..Default::default() }),
            Some("SENSITIVITY"),
        ),
        Opt::IgnoreSquareBrackets => Ignore {
            change: |d| r(d).definition = "CREATE PROCEDURE dbo.R AS SELECT [Id] FROM [dbo].[T]".into(),
            base: |d| r(d).definition = "CREATE PROCEDURE dbo.R AS SELECT Id FROM dbo.T".into(),
            view: false,
            marker: None,
        },
        Opt::IgnoreStatistics => ignore(|d| t(d).statistics.push(Statistic { name: "ST_T_P".into(), columns: vec!["P".into()], ..Default::default() }), Some("ST_T_P")),
        Opt::IgnoreStatisticsNorecompute => ignore(|d| t(d).indexes[0].no_recompute = true, Some("STATISTICS_NORECOMPUTE")),
        Opt::IgnoreSystemNamedConstraintNames => Ignore {
            change: |d| t(d).indexes[0].name = "PK__T__2".into(),
            base: |d| {
                let ix = &mut t(d).indexes[0];
                ix.name = "PK__T__1".into();
                ix.system_named = true;
            },
            view: true,
            marker: Some("PK__T__"),
        },
        Opt::IgnoreTSQLT => ignore(
            |d| {
                d.schemas.push(Schema { name: "tSQLt".into(), owner: "dbo".into() });
                d.routines.push(Routine { schema: "tSQLt".into(), name: "Run".into(), definition: "CREATE PROCEDURE tSQLt.Run AS SELECT 1".into(), ..r0() });
            },
            Some("tSQLt"),
        ),
        Opt::IgnoreUserProperties => Ignore {
            change: |d| d.principals.last_mut().unwrap().login = Some("L2".into()),
            base: |d| d.principals.push(Principal { name: "u".into(), kind: "SQL_USER".into(), login: Some("L1".into()), ..Default::default() }),
            view: false,
            marker: Some("L2"),
        },
        // Users themselves are still compared; their permissions and role memberships are not.
        Opt::IgnoreUsers => Ignore {
            change: |d| d.principals.last_mut().unwrap().member_of = vec!["app".into()],
            base: |d| d.principals.push(Principal { name: "bob".into(), kind: "SQL_USER".into(), login: Some("bob".into()), ..Default::default() }),
            view: false,
            marker: Some("ADD MEMBER"),
        },
        Opt::IgnoreWhiteSpace => ignore(|d| r(d).definition = "CREATE PROCEDURE  dbo.R  AS   SELECT 1".into(), None),
        Opt::IgnoreWithElementOrder => Ignore {
            change: |d| r(d).definition = "CREATE PROCEDURE dbo.R WITH EXECUTE AS OWNER, RECOMPILE AS SELECT 1".into(),
            base: |d| r(d).definition = "CREATE PROCEDURE dbo.R WITH RECOMPILE, EXECUTE AS OWNER AS SELECT 1".into(),
            view: false,
            marker: None,
        },
        Opt::IgnoreWithEncryption => Ignore { change: |d| r(d).definition = "CREATE PROCEDURE dbo.R WITH ENCRYPTION AS SELECT 1".into(), base: nothing, view: false, marker: None },
        Opt::IgnoreWithNocheck => Ignore {
            change: |d| {
                t(d).foreign_keys[0].not_trusted = true;
                t(d).check_constraints[0].not_trusted = true;
            },
            base: |d| {
                t(d).foreign_keys.push(fk());
                t(d).check_constraints.push(check());
            },
            view: true,
            marker: Some("NOCHECK"),
        },
        Opt::IgnoreDynamicDataMasking => ignore(|d| t(d).columns[2].masking_function = Some("default()".into()), Some("MASKED")),
        Opt::IgnoreStatisticsIncremental => Ignore {
            change: |d| t(d).indexes[0].incremental = true,
            base: nothing,
            view: true,
            marker: Some("INCREMENTAL"),
        },

        Opt::AddDatabaseUseStatement => Check(|| {
            assert!(new_table(Options::default_set().with(Opt::AddDatabaseUseStatement)).sql.contains("USE [t]"));
            assert!(!new_table(Options::default_set()).sql.contains("USE ["));
        }),
        Opt::ObjectExistenceChecks => Check(|| {
            let s = new_table(Options::default_set().with(Opt::ObjectExistenceChecks)).sql;
            assert!(s.contains("IF OBJECT_ID") || s.contains("IF NOT EXISTS"), "{s}");
            let s = new_table(Options::default_set()).sql;
            assert!(!s.contains("IF OBJECT_ID") && !s.contains("IF NOT EXISTS"), "{s}");
        }),
        Opt::AddWithEncryption => Check(|| {
            assert!(new_proc(Options::default_set().with(Opt::AddWithEncryption)).sql.contains("WITH ENCRYPTION"));
            assert!(!new_proc(Options::default_set()).sql.contains("WITH ENCRYPTION"));
        }),
        Opt::NoAutoColumnMapping => Check(|| {
            // A renamed column pairs with its look-alike unless the option is set.
            let a = with(|d| t(d).columns[2].name = "Names".into());
            let b = base();
            let on = run(&a, &b, &Options::default_set());
            let off = run(&a, &b, &Options::default_set().with(Opt::NoAutoColumnMapping));
            assert!(on.differences.iter().any(|d| d.name.name == "T" && d.kind == DiffKind::Different));
            assert!(off.differences.iter().any(|d| d.name.name == "T" && d.kind == DiffKind::Different));
        }),
        Opt::ConsiderNextFilegroupInPartitionSchemes => Check(|| {
            // One boundary, two partitions; the first side has a NEXT USED filegroup.
            let side = |filegroups: &[&str]| Database {
                partition_functions: vec![PartitionFunction {
                    name: "PF".into(),
                    data_type: DataType { name: "int".into(), ..Default::default() },
                    boundaries: vec!["1".into()],
                    ..Default::default()
                }],
                partition_schemes: vec![PartitionScheme { name: "PS".into(), function: "PF".into(), filegroups: filegroups.iter().map(|f| f.to_string()).collect() }],
                ..Default::default()
            };
            let (a, b) = (side(&["PRIMARY", "PRIMARY", "FG2"]), side(&["PRIMARY", "PRIMARY"]));
            let diff = |o: &Options| run(&a, &b, &o.clone().without(Opt::IgnoreFileGroups)).differences.iter().any(|d| d.kind != DiffKind::Equal);
            assert!(diff(&Options::default_set().with(Opt::ConsiderNextFilegroupInPartitionSchemes)));
            assert!(!diff(&Options::default_set()));
        }),
        Opt::SsdtCompatibleOptions => Check(|| {
            let o = Options::default_set().with(Opt::SsdtCompatibleOptions);
            assert!(o.has(Opt::IgnorePermissions) && o.has(Opt::IgnoreWithNocheck) && o.has(Opt::IgnoreSystemNamedConstraintNames) && o.has(Opt::InlineTableObjects));
        }),
        Opt::DecryptPost2kEncryptedObjects => NoOp("read option: decides whether encrypted modules are decrypted when a live database is read"),
        Opt::DisableAndReenableDdlTriggers => Check(|| {
            let ddl = DdlTrigger { name: "D".into(), definition: "CREATE TRIGGER D ON DATABASE FOR CREATE_TABLE AS SELECT 1".into(), events: vec!["CREATE_TABLE".into()], ..Default::default() };
            let tgt = Database { ddl_triggers: vec![ddl], ..base() };
            let src = Database { ddl_triggers: tgt.ddl_triggers.clone(), ..with(|d| t(d).columns.push(column("Extra"))) };
            let sql = |o: Options| script(&src, &tgt, &o).sql;
            assert!(sql(Options::default_set().with(Opt::DisableAndReenableDdlTriggers)).contains("DISABLE TRIGGER"));
            assert!(!sql(Options::default_set()).contains("DISABLE TRIGGER"));
        }),
        Opt::NoErrorHandling => Check(|| {
            assert!(new_table(Options::default_set()).sql.contains("@@ERROR"));
            assert!(!new_table(Options::default_set().with(Opt::NoErrorHandling)).sql.contains("@@ERROR"));
        }),
        Opt::DoNotOutputCommentHeader => Check(|| {
            assert!(new_table(Options::default_set()).sql.starts_with("/*"));
            assert!(!new_table(Options::default_set().with(Opt::DoNotOutputCommentHeader)).sql.starts_with("/*"));
        }),
        Opt::UseClrUdtToStringForClrMigration => NoOp("covered by the CLR deployment tests in deploy/unbind.rs"),
        Opt::NoTransactions => Check(|| {
            assert!(new_table(Options::default_set()).sql.contains("BEGIN TRANSACTION"));
            assert!(!new_table(Options::default_set().with(Opt::NoTransactions)).sql.contains("BEGIN TRANSACTION"));
        }),
        Opt::NoDeploymentLogging => Check(|| {
            // Shown as "Log the deployment": ticked logs, which is the flag cleared.
            let logged = new_table(Options::default_set().without(Opt::NoDeploymentLogging)).sql;
            let plain = new_table(Options::default_set().with(Opt::NoDeploymentLogging)).sql;
            assert_ne!(logged, plain);
        }),
        Opt::ForceColumnOrder => Check(|| {
            // A column added in the middle: appended normally, the table rebuilt when forced.
            let a = with(|d| {
                let c = column("Mid");
                t(d).columns.insert(1, c);
            });
            let b = base();
            let plain = script(&a, &b, &Options::default_set()).sql;
            let forced = script(&a, &b, &Options::default_set().with(Opt::ForceColumnOrder)).sql;
            assert!(plain.contains("ADD") && !plain.contains("CREATE TABLE"), "{plain}");
            assert!(forced.contains("CREATE TABLE"), "{forced}");
        }),
        Opt::IgnoreMigrationScripts => NoOp("migration scripts are not supported, so there are none to ignore"),
        Opt::CaseSensitiveObjectDefinition => Check(|| {
            let a = with(|d| r(d).definition = "CREATE PROCEDURE dbo.R AS select 1".into());
            let b = base();
            let diff = |o: Options| run(&a, &b, &o).differences.iter().any(|d| d.kind != DiffKind::Equal);
            assert!(!diff(Options::default_set()));
            assert!(diff(Options::default_set().with(Opt::CaseSensitiveObjectDefinition)));
        }),
        Opt::UseCompatibilityLevel => NoOp("covered by deploy.rs tests (features follow the target's compatibility level)"),
        Opt::DropAndCreateInsteadOfAlter => Check(|| {
            let (a, b) = (with(|d| r(d).definition = "CREATE PROCEDURE dbo.R AS SELECT 2".into()), base());
            let s = script(&a, &b, &Options::default_set().with(Opt::DropAndCreateInsteadOfAlter)).sql;
            assert!(s.contains("DROP PROCEDURE") && s.contains("CREATE PROCEDURE"), "{s}");
            assert!(script(&a, &b, &Options::default_set()).sql.contains("ALTER PROCEDURE"));
        }),
        Opt::CreateOrAlterInsteadOfAlter => Check(|| {
            let (a, b) = (with(|d| r(d).definition = "CREATE PROCEDURE dbo.R AS SELECT 2".into()), base());
            assert!(script(&a, &b, &Options::default_set().with(Opt::CreateOrAlterInsteadOfAlter)).sql.contains("CREATE OR ALTER PROCEDURE"));
            assert!(!script(&a, &b, &Options::default_set()).sql.contains("CREATE OR ALTER"));
        }),
        Opt::AddNoPopulation => Check(|| {
            let src = with(|d| {
                d.fulltext_catalogs.push(FullTextCatalog { name: "FC".into(), ..Default::default() });
                t(d).fulltext_index = Some(FullTextIndex {
                    catalog: "FC".into(),
                    key_index: "PK_T".into(),
                    change_tracking: "AUTO".into(),
                    stoplist: "SYSTEM".into(),
                    columns: vec![FullTextColumn { name: "Name".into(), type_column: None, language: 1033 }],
                    ..Default::default()
                });
            });
            let tgt = with(|d| d.fulltext_catalogs.push(FullTextCatalog { name: "FC".into(), ..Default::default() }));
            assert!(script(&src, &tgt, &Options::default_set().with(Opt::AddNoPopulation)).sql.contains("NO POPULATION"));
            assert!(!script(&src, &tgt, &Options::default_set()).sql.contains("NO POPULATION"));
        }),
        Opt::AddOnlineOn => Check(|| {
            let (a, b) = (with(|d| t(d).indexes.push(index("IX_T_P"))), base());
            assert!(script(&a, &b, &Options::default_set().with(Opt::AddOnlineOn)).sql.contains("ONLINE = ON"));
            assert!(!script(&a, &b, &Options::default_set()).sql.contains("ONLINE = ON"));
        }),
        other => panic!("no case for {other:?}: add one to option_tests.rs"),
    }
}

#[test]
fn every_visible_option_has_a_case() {
    let mut failed = Vec::new();
    for o in ALL.iter().copied().filter(|o| o.group() != Group::Hidden) {
        let ok = std::panic::catch_unwind(|| match case(o) {
            Case::Ignore { change, base: prepare, view, marker } => check_ignore(o, change, prepare, view, marker),
            Case::Check(f) => f(),
            Case::NoOp(why) => assert!(!why.is_empty()),
        });
        if ok.is_err() {
            failed.push(o);
        }
    }
    assert!(failed.is_empty(), "options that do not behave: {failed:?}");
}

fn check_ignore(o: Opt, change: fn(&mut Database), prepare: fn(&mut Database), view: bool, marker: Option<&str>) {
    let mut b = base();
    prepare(&mut b);
    let mut a = b.clone();
    change(&mut a);
    let off = Options::default_set().without(o);
    let on = Options::default_set().with(o);
    let c = run(&a, &b, &off);
    assert!(c.differences.iter().any(|d| d.kind != DiffKind::Equal), "{o:?} off: no difference");
    let c = run(&a, &b, &on);
    for d in &c.differences {
        assert_eq!(d.kind, DiffKind::Equal, "{o:?} on: {:?} {:?}", d.name, d.properties);
        if view {
            let left = crate::compare_serve::object_sql(&a, d.object_type, &d.name, &on);
            let right = crate::compare_serve::object_sql(&b, d.object_type, d.target_name.as_ref().unwrap_or(&d.name), &on);
            let lines = linediff::compare(left.as_deref(), right.as_deref(), on.has(Opt::IgnoreWhiteSpace));
            assert!(lines.iter().all(|l| !l.different), "{o:?} on: the view differs\n{left:?}\n{right:?}");
        }
    }
    assert!(generate(&c, &a, &b, &on, &Mappings::default(), &settings()).unwrap().batches.is_empty(), "{o:?} on: something deployed");
    // The table different for another reason: the ignored part stays out of the script.
    if let Some(m) = marker {
        let mut a2 = a.clone();
        t(&mut a2).columns.push(column("Extra"));
        let s = script(&a2, &b, &on).sql;
        assert!(s.contains("[Extra]"), "{o:?}: {s}");
        assert!(!s.contains(m), "{o:?} on: {m} deployed\n{s}");
    }
}

fn run(a: &Database, b: &Database, o: &Options) -> Comparison {
    compare(a, b, o, &Mappings::default())
}

fn settings() -> ScriptSettings {
    ScriptSettings { transaction_isolation_level: "SERIALIZABLE".into(), source: "s".into(), target: "t".into(), target_database: "t".into(), target_major_version: 0 }
}

fn script(a: &Database, b: &Database, o: &Options) -> DeploymentScript {
    generate(&run(a, b, o), a, b, o, &Mappings::default(), &settings()).unwrap()
}

/// A table only in the source.
fn new_table(o: Options) -> DeploymentScript {
    let a = with(|d| d.tables.push(Table { name: "N".into(), indexes: Vec::new(), ..d.tables[1].clone() }));
    script(&a, &base(), &o)
}

/// A procedure only in the source.
fn new_proc(o: Options) -> DeploymentScript {
    let a = with(|d| d.routines.push(Routine { name: "R2".into(), definition: "CREATE PROCEDURE dbo.R2 AS SELECT 1".into(), ..r0() }));
    script(&a, &base(), &o)
}

fn with(f: impl FnOnce(&mut Database)) -> Database {
    let mut d = base();
    f(&mut d);
    d
}

fn t(d: &mut Database) -> &mut Table {
    d.tables.iter_mut().find(|t| t.name == "T").unwrap()
}

fn r(d: &mut Database) -> &mut Routine {
    d.routines.iter_mut().find(|r| r.name == "R").unwrap()
}

fn r0() -> Routine {
    Routine {
        schema: "dbo".into(),
        name: "R".into(),
        kind: RoutineKind::Procedure,
        definition: "CREATE PROCEDURE dbo.R AS SELECT 1".into(),
        settings: ModuleSettings { ansi_nulls: true, quoted_identifier: true },
        ..Default::default()
    }
}

fn column(name: &str) -> Column {
    Column { name: name.into(), data_type: DataType { name: "int".into(), ..Default::default() }, nullable: true, ..Default::default() }
}

fn identity(seed: &str) -> Identity {
    Identity { seed: seed.into(), increment: "1".into(), not_for_replication: false }
}

fn index(name: &str) -> Index {
    Index {
        name: name.into(),
        kind: IndexKind::Nonclustered,
        columns: vec![IndexColumn { name: "P".into(), descending: false }],
        allow_row_locks: true,
        allow_page_locks: true,
        ..Default::default()
    }
}

fn pk(name: &str) -> Index {
    Index {
        name: name.into(),
        kind: IndexKind::Clustered,
        primary_key: true,
        unique: true,
        columns: vec![IndexColumn { name: "Id".into(), descending: false }],
        allow_row_locks: true,
        allow_page_locks: true,
        ..Default::default()
    }
}

fn fk() -> ForeignKey {
    ForeignKey {
        name: "FK_T_P".into(),
        columns: vec!["P".into()],
        referenced_table: ObjectName::new("dbo", "P"),
        referenced_columns: vec!["Id".into()],
        on_delete: "NO_ACTION".into(),
        on_update: "NO_ACTION".into(),
        ..Default::default()
    }
}

fn check() -> CheckConstraint {
    CheckConstraint { name: "CK_T".into(), definition: "([P]>(0))".into(), ..Default::default() }
}

fn trigger(name: &str) -> Trigger {
    Trigger {
        name: name.into(),
        definition: format!("CREATE TRIGGER [dbo].[{name}] ON [dbo].[T] AFTER INSERT AS SELECT 1"),
        events: vec!["INSERT".into()],
        settings: ModuleSettings { ansi_nulls: true, quoted_identifier: true },
        ..Default::default()
    }
}

/// dbo.P(Id) and dbo.T(Id, P, Name), a procedure dbo.R, a synonym dbo.S and a role app.
fn base() -> Database {
    let not_null = |n: &str| Column { nullable: false, ..column(n) };
    let name = Column {
        name: "Name".into(),
        data_type: DataType { name: "varchar".into(), length: Some(50), ..Default::default() },
        nullable: true,
        collation: Some("SQL_Latin1_General_CP1_CI_AS".into()),
        ..Default::default()
    };
    let table = |n: &str, columns: Vec<Column>| Table {
        schema: "dbo".into(),
        name: n.into(),
        lock_escalation: "TABLE".into(),
        columns,
        indexes: vec![pk(&format!("PK_{n}"))],
        ..Default::default()
    };
    let mut cols = vec![not_null("Id"), column("P"), name];
    for (i, c) in cols.iter_mut().enumerate() {
        c.ordinal = i as u32 + 1;
    }
    Database {
        collation: "SQL_Latin1_General_CP1_CI_AS".into(),
        schemas: vec![Schema { name: "dbo".into(), owner: "dbo".into() }],
        principals: vec![Principal { name: "app".into(), kind: "DATABASE_ROLE".into(), owner: Some("dbo".into()), ..Default::default() }],
        tables: vec![table("P", vec![Column { ordinal: 1, ..not_null("Id") }]), table("T", cols)],
        routines: vec![r0()],
        synonyms: vec![Synonym { schema: "dbo".into(), name: "S".into(), owner: None, base_object: "[db1].[dbo].[T]".into() }],
        ..Default::default()
    }
}

#[test]
fn ignored_options_are_compare_options() {
    // The view hides what these ignore; a deploy-only option there would change it.
    for o in ALL.iter().copied().filter(|o| o.group() == Group::Ignore) {
        assert_eq!(o.kind(), super::options::Kind::Compare, "{o:?}");
    }
}
