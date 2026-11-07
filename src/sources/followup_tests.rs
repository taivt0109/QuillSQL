//! The model fields the container cannot produce (FileTables, Stretch, CLR triggers, external
//! tables) and every typed extended property value, through snapshots and scripts folders.

use super::{folder, snp, snp_write};
use crate::catalog::*;
use crate::schema::Options;

fn column(name: &str, ty: &str) -> Column {
    Column { name: name.into(), ordinal: 1, data_type: DataType { name: ty.into(), ..Default::default() }, ..Default::default() }
}

fn table(name: &str) -> Table {
    Table {
        schema: "dbo".into(),
        name: name.into(),
        lock_escalation: "TABLE".into(),
        storage: Storage { data_space: Some("PRIMARY".into()), ..Default::default() },
        columns: vec![column("Id", "int")],
        ..Default::default()
    }
}

fn model() -> Database {
    let mut docs = table("Docs");
    docs.file_table = Some(FileTable { directory: "Docs".into(), filename_collation: "Latin1_General_CI_AS".into(), primary_key: Some("PK_Docs".into()) });
    docs.filestream_filegroup = Some("FS".into());
    docs.owner = Some("Bob".into());
    let mut cold = table("Cold");
    cold.remote_data_archive = true;
    cold.large_value_types_out_of_row = true;
    cold.owner = Some("Bob".into());
    cold.triggers = vec![
        Trigger { name: "T_Clr".into(), events: vec!["INSERT".into()], clr_method: Some("[Asm].[Cls].[M]".into()), execute_as: Some("OWNER".into()), ..Default::default() },
        Trigger {
            name: "T_Off".into(),
            events: vec!["DELETE".into()],
            definition: "CREATE TRIGGER [dbo].[T_Off] ON [dbo].[Cold] AFTER DELETE AS SELECT 1".into(),
            settings: ModuleSettings { ansi_nulls: false, quoted_identifier: false },
            ..Default::default()
        },
    ];
    let prop = |name: &str, value: &str, ty: &str| ExtendedProperty { class: "DATABASE".into(), name: name.into(), value: Some(value.into()), value_type: Some(ty.into()), ..Default::default() };
    Database {
        name: "Followup".into(),
        collation: "SQL_Latin1_General_CP1_CI_AS".into(),
        compatibility_level: 160,
        schemas: vec![Schema { name: "dbo".into(), owner: "dbo".into() }],
        tables: vec![cold, docs],
        principals: vec![Principal { name: "Bob".into(), kind: "SQL_USER".into(), default_schema: Some("dbo".into()), ..Default::default() }],
        external_data_sources: vec![ExternalDataSource { name: "Src".into(), location: "sqlserver://h".into(), ..Default::default() }],
        external_tables: vec![ExternalTable { schema: "dbo".into(), name: "Ext".into(), owner: Some("Bob".into()), columns: vec![column("Id", "int")], data_source: "Src".into(), ..Default::default() }],
        extended_properties: vec![
            prop("a_int", "-5", "int"),
            prop("b_bigint", "9000000000", "bigint"),
            prop("c_smallint", "7", "smallint"),
            prop("d_tinyint", "255", "tinyint"),
            prop("e_bit", "1", "bit"),
            prop("f_float", "1.5", "float"),
            prop("g_varbinary", "0x0A0B", "varbinary"),
            prop("h_decimal", "12.340", "decimal"),
            prop("i_money", "3.50", "money"),
            prop("j_guid", "0E984725-C51C-4BF4-9960-E1C80E27ABA0", "uniqueidentifier"),
            prop("k_datetime", "2024-01-02 03:04:05.000", "datetime"),
            prop("l_nvarchar", "x", "nvarchar"),
            prop("m_varchar", "y", "varchar"),
        ],
        ..Default::default()
    }
}

fn find<'a>(db: &'a Database, name: &str) -> &'a Table {
    db.tables.iter().find(|t| t.name == name).unwrap()
}

fn props(db: &Database) -> Vec<(String, Option<String>, Option<String>)> {
    let mut v: Vec<_> = db.extended_properties.iter().map(|p| (p.name.clone(), p.value.clone(), p.value_type.clone())).collect();
    v.sort();
    v
}

#[test]
fn snapshots_carry_filetables_stretch_clr_triggers_and_typed_values() {
    let db = model();
    let back = snp::read(&snp_write::write(&db)).unwrap();
    let (docs, cold) = (find(&back, "Docs"), find(&back, "Cold"));
    assert_eq!(docs.file_table, db.tables[1].file_table);
    assert_eq!(docs.owner.as_deref(), Some("Bob"));
    assert!(cold.remote_data_archive && cold.large_value_types_out_of_row);
    let clr = cold.triggers.iter().find(|t| t.name == "T_Clr").unwrap();
    assert_eq!((clr.clr_method.as_deref(), clr.execute_as.as_deref()), (Some("[Asm].[Cls].[M]"), Some("OWNER")));
    let off = cold.triggers.iter().find(|t| t.name == "T_Off").unwrap();
    assert_eq!(off.settings, ModuleSettings { ansi_nulls: false, quoted_identifier: false });
    assert_eq!(back.external_tables[0].owner.as_deref(), Some("Bob"));
    assert_eq!(props(&back), props(&db));
}

#[test]
fn scripts_folders_carry_filetables_stretch_and_typed_values() {
    let mut db = model();
    db.external_tables.clear();
    db.external_data_sources.clear();
    db.tables[0].triggers.retain(|t| t.clr_method.is_none());
    let dir = std::env::temp_dir().join(format!("quill-followup-unit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    folder::write_all(&db, &dir, &Options::default_set()).unwrap();
    let text = std::fs::read_to_string(dir.join("Tables").join("dbo.Docs.sql")).unwrap();
    assert!(text.starts_with("CREATE TABLE [dbo].[Docs] AS FILETABLE ON [PRIMARY] FILESTREAM_ON [FS]\r\nWITH\r\n(\r\n\tFILETABLE_DIRECTORY = N'Docs', FILETABLE_COLLATE_FILENAME = Latin1_General_CI_AS, FILETABLE_PRIMARY_KEY_CONSTRAINT_NAME = [PK_Docs]\r\n)\r\nGO\r\nALTER AUTHORIZATION ON [dbo].[Docs] TO [Bob]\r\n"), "{text}");
    let cold = std::fs::read_to_string(dir.join("Tables").join("dbo.Cold.sql")).unwrap();
    assert!(cold.contains("ALTER TABLE [dbo].[Cold] SET (REMOTE_DATA_ARCHIVE = ON (MIGRATION_STATE = PAUSED))\r\nGO\r\nALTER AUTHORIZATION ON [dbo].[Cold] TO [Bob]"), "{cold}");
    assert!(cold.ends_with("EXEC sp_tableoption N'[dbo].[Cold]', N'large value types out of row', 1;\r\nGO\r\n"), "{cold}");
    let f = folder::read(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(f.messages.is_empty(), "{:?}", f.messages);
    let (docs, cold) = (find(&f.database, "Docs"), find(&f.database, "Cold"));
    assert_eq!(docs.file_table, db.tables[1].file_table);
    assert_eq!(docs.owner.as_deref(), Some("Bob"));
    assert!(cold.remote_data_archive && cold.large_value_types_out_of_row);
    assert_eq!(cold.triggers[0].settings, ModuleSettings { ansi_nulls: false, quoted_identifier: false });
    assert_eq!(props(&f.database), props(&db));
}
