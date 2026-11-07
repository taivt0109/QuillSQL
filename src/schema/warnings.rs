//! Deployment warnings with SQL Compare's severities and wording. `object` is the object the
//! warning belongs to (empty for the deployment as a whole); names are passed as written in the
//! script (`[dbo].[T]`, column names without brackets).

use super::deploy::{Severity, Warning};

fn w(severity: Severity, object: &str, message: String) -> Warning {
    Warning { severity, object: object.to_string(), message }
}

const PRESERVED: &str = "The table must be rebuilt. The data in the table apart from dropped columns will be preserved.";

/// Why a table is rebuilt (Medium).
pub mod rebuild {
    use super::*;

    pub fn filegroup(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The filegroup of table {t} has changed. {PRESERVED}"))
    }

    pub fn text_filegroup(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The text filegroup of table {t} has changed. {PRESERVED}"))
    }

    pub fn filestream_filegroup(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The filestream filegroup of table {t} has changed. {PRESERVED}"))
    }

    pub fn partitioning(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The partitioning of table {t} has changed in a way that requires a table rebuild. The data in the table apart from dropped columns will be preserved."))
    }

    pub fn filestream(t: &str, column: &str) -> Warning {
        w(Severity::Medium, t, format!("Table {t} contains filestream data in column {column}. {PRESERVED}"))
    }

    pub fn column_set(t: &str, column: &str) -> Warning {
        w(Severity::Medium, t, format!("Cannot add column set on field {column} to table {t}. {PRESERVED}"))
    }

    pub fn period_column(t: &str, column_def: &str) -> Warning {
        w(Severity::Medium, t, format!("Cannot alter period column {column_def} on system-versioned temporal table {t}. {PRESERVED}"))
    }

    pub fn column_alter(t: &str, old_def: &str, new_def: &str) -> Warning {
        w(Severity::Medium, t, format!("Cannot alter column {old_def} to {new_def} on table {t}. {PRESERVED}"))
    }

    pub fn identity_alter(t: &str, column: &str) -> Warning {
        w(Severity::Medium, t, format!("Cannot alter IDENTITY property on column [{column}] on table {t}. {PRESERVED}"))
    }

    pub fn identity_change(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The IDENTITY column on table {t} has changed. {PRESERVED}"))
    }

    pub fn add_graph(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The table {t} must be rebuilt to convert it from a normal table into a SQL Graph table."))
    }

    pub fn remove_graph(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The table {t} must be rebuilt to convert it from a SQL Graph table into a normal table."))
    }

    pub fn column_order(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The column order in table {t} has changed. {PRESERVED}"))
    }

    pub fn persisted_reference(t: &str, column: &str) -> Warning {
        w(
            Severity::Medium,
            t,
            format!("The column [{column}] on table {t} is a persisted computed column that references columns that need to be rebuilt. {PRESERVED}"),
        )
    }

    pub fn add_not_null_no_default(t: &str, column: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("This deployment adds the column [{column}] to table {t} as NOT NULL, with no default value specified. The table will be rebuilt. Any data in the table will cause the deployment to fail."),
        )
    }

    pub fn add_not_null_bound_default(t: &str, column: &str, default: &str) -> Warning {
        w(
            Severity::Medium,
            t,
            format!("The column [{column}] on table {t} must be added and does not allow NULL values. The default {default} must be bound to it. The table must be rebuilt. The data in the table will be preserved."),
        )
    }

    pub fn computed_not_preserved(t: &str, column: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("Column [{column}] on table {t} is being changed from a computed to a normal column. The computed column data will not be migrated during the rebuild."),
        )
    }

    pub fn fields_dropped(t: &str, column: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("Couldn't match the column [{column}] on table {t} to a column in the source table. The data in this column will be lost unless additional steps are taken to preserve it."),
        )
    }

    pub fn null_to_default(t: &str, column: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("Column [{column}] on table {t} no longer allows NULL values, but it has a new default set up. During the table rebuild such NULL values will be changed to the new default value."),
        )
    }

    pub fn cannot_cast(t: &str, column: &str, old: &str, new: &str) -> Warning {
        w(Severity::High, t, format!("The column [{column}] on table {t} cannot be cast from {old} to {new}. The script may fail."))
    }

    pub fn cast_to_non_system_type(t: &str, column: &str, new: &str) -> Warning {
        w(Severity::High, t, format!("The column [{column}] on table {t} cannot be cast to {new} because it is not a system type. The script may fail."))
    }
}

pub fn table_drop(t: &str) -> Warning {
    w(Severity::High, t, format!("This deployment drops the table {t}. You may want to take additional steps to preserve the data in the table."))
}

/// `columns` already bracketed and comma-joined.
pub fn columns_dropped(t: &str, columns: &str) -> Warning {
    w(
        Severity::High,
        t,
        format!("This deployment drops the column(s) {columns} from table {t}. Data in these column(s), will be lost unless additional steps are taken to preserve it."),
    )
}

pub fn not_null_no_default(t: &str, column: &str) -> Warning {
    w(
        Severity::High,
        t,
        format!("This deployment alters the column [{column}] on table {t} to NOT NULL, with no default value specified. Any NULLs in the column will cause the deployment to fail."),
    )
}

pub fn not_null_default(t: &str, column: &str) -> Warning {
    w(Severity::Low, t, format!("The column [{column}] on table {t} is being marked as NOT NULL. The default value will be applied where appropriate."))
}

pub fn truncated(t: &str, column: &str) -> Warning {
    w(Severity::High, t, format!("This deployment truncates the column [{column}] on table {t}. Data may be lost unless additional steps are taken to preserve it."))
}

pub fn unicode_loss(t: &str, column: &str) -> Warning {
    w(Severity::High, t, format!("This deployment will corrupt any Unicode strings in the column [{column}] on table {t}."))
}

pub fn precision_loss(t: &str, column: &str, old: &str, new: &str) -> Warning {
    w(
        Severity::High,
        t,
        format!("This deployment casts the column [{column}] on table {t} from {old} to {new}. Any data that exceeds the new column size might lose precision, or the deployment could fail."),
    )
}

pub fn xml_schema_change(t: &str, column: &str, collection: &str) -> Warning {
    w(
        Severity::High,
        t,
        format!("This deployment alters the XML schema collection {collection} used by column [{column}] on table {t}. Any data that does not conform to the new schema might cause the deployment to fail."),
    )
}

pub fn change_tracking_enable(t: &str) -> Warning {
    w(Severity::Medium, t, format!("Table change tracking is being added to table {t}. Make sure database change tracking is enabled before running this script."))
}

pub fn change_tracking_reenable(t: &str) -> Warning {
    w(Severity::High, t, format!("The change tracking on table {t} needs to be disabled and re-enabled. The current change tracking data will be lost."))
}

pub fn recovered_table(t: &str) -> Warning {
    w(
        Severity::Medium,
        t,
        "The target database includes a table that was recovered from a failed deployment. You can use recovery tables to restore data lost in a failed deployment.".into(),
    )
}

pub fn no_transactions() -> Warning {
    w(
        Severity::High,
        "",
        "This deployment will run without transactions. This could leave your database in an inconsistent state if it fails. We recommend backing up the database as part of the deployment, or enabling transactions.".into(),
    )
}

pub fn memory_optimized_in_transaction() -> Warning {
    w(
        Severity::High,
        "",
        "You're attempting to deploy one or more memory-optimized objects (memory-optimized tables, natively compiled stored procedures or memory-optimized table types) in a transaction, which is not supported by SQL Server.\r\n\r\nIf you want to deploy memory-optimized objects, you need to select the 'Do not use transactions in deployment scripts' project option.".into(),
    )
}

pub fn memory_optimized_filegroup() -> Warning {
    w(
        Severity::Low,
        "",
        "This deployment includes one or more memory-optimized objects (memory-optimized tables, natively compiled stored procedures or memory-optimized table types). Make sure that your target database has a memory-optimized filegroup.".into(),
    )
}

pub fn filegroups_for_tables(list: &str) -> Warning {
    w(Severity::Medium, "", format!("Tables need to be created using one or more non-standard filegroups. These filegroups may need to be created manually: {list}"))
}

pub fn filegroups_for_objects(list: &str) -> Warning {
    w(Severity::Medium, "", format!("Objects need to be created using one or more non-standard filegroups. These filegroups may need to be created manually: {list}"))
}

pub fn filegroups_unsupported() -> Warning {
    w(Severity::High, "", "Filegroups are not supported in the target database version.".into())
}

pub fn use_unsupported() -> Warning {
    w(Severity::Medium, "", "The use statement is not supported in the target database version.".into())
}

pub fn old_server() -> Warning {
    w(Severity::Low, "", "A future version of QuillSQL Compare will no longer support SQL Server 2008 and SQL Server 2008R2.".into())
}

/// What the target version cannot take (High unless stated).
pub mod target {
    use super::*;

    pub fn memory_optimized_table(t: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("The memory-optimized table {t} is not supported in database versions older than SQL Server 2014 or basic and standard tiers of Microsoft Azure SQL Database."),
        )
    }

    pub fn native_procedure(p: &str) -> Warning {
        w(
            Severity::High,
            p,
            format!("The natively compiled stored procedure {p} is not supported in database versions older than SQL Server 2014 or in Microsoft Azure SQL Databases."),
        )
    }

    pub fn native_function(f: &str) -> Warning {
        w(
            Severity::High,
            f,
            format!("The natively compiled function {f} is not supported in database versions older than SQL Server 2016 or in Microsoft Azure SQL Databases."),
        )
    }

    pub fn memory_optimized_table_type(ty: &str) -> Warning {
        w(Severity::High, ty, format!("The memory-optimized user-defined table type {ty} is not supported in the target database version."))
    }

    pub fn table_type_indexes(ty: &str) -> Warning {
        w(Severity::High, ty, format!("The user type {ty} can't have indexes in database versions older than SQL Server 2014."))
    }

    pub fn temporal(t: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!("Temporal tables are not supported in the target database version. {t} can be deployed but will not be a temporal table in the target database."),
        )
    }

    pub fn retention(t: &str) -> Warning {
        w(Severity::Medium, t, format!("The temporal {t} has a history retention policy, which is a Server 2017 feature. This deployment will omit the retention period."))
    }

    pub fn masking(t: &str, column: &str) -> Warning {
        w(
            Severity::Medium,
            t,
            format!("The masking function for column {column} will be ignored. The dynamic data masking option is not supported in the target database version."),
        )
    }

    pub fn filestream(t: &str, column: &str) -> Warning {
        w(Severity::High, t, format!("The filestream property on column {column} in table {t} is not supported in the target database version."))
    }

    pub fn graph(t: &str) -> Warning {
        w(Severity::High, t, format!("The graph table {t} is not supported in the target database version."))
    }

    pub fn clustered_columnstore(t: &str, ix: &str) -> Warning {
        w(Severity::High, t, format!("The clustered columnstore index {ix} on table {t} is not supported in the target database version."))
    }

    pub fn columnstore(t: &str, ix: &str) -> Warning {
        w(Severity::High, t, format!("The columnstore index {ix} on table {t} is not supported in the target database version."))
    }

    pub fn incremental_statistics(t: &str, ix: &str) -> Warning {
        w(
            Severity::High,
            t,
            format!(
                "The STATISTICS_INCREMENTAL option for indexes and the INCREMENTAL option for statistics aren't supported in previous versions of SQL Server (SQL Server 2012 and earlier) and Microsoft Azure SQL Database. The deployment script will not include the incremental statistics option for {ix} on {t}."
            ),
        )
    }

    pub fn auto_grid(t: &str, ix: &str) -> Warning {
        w(
            Severity::Low,
            t,
            format!("The spatial index {ix} on table {t} uses AUTO_GRID, which is not supported in the target version. Spatial index will be created with default GRID setting."),
        )
    }

    pub fn selective_xml(t: &str, ix: &str) -> Warning {
        w(Severity::High, t, format!("The selective XML index {ix} on {t} is not supported in the target database version."))
    }

    pub fn sequence(q: &str) -> Warning {
        w(Severity::High, q, format!("The sequence object {q} is not supported in the target database version."))
    }

    pub fn multipart_synonym(sn: &str) -> Warning {
        w(Severity::High, sn, format!("The synonym {sn} references a 3 or 4 part name which is not supported in the target database version."))
    }

    pub fn online(object: &str, edition: &str) -> Warning {
        w(
            Severity::Low,
            object,
            format!("Online operations are only supported on SQL Server Enterprise Edition. As the target server is {edition} Edition, ONLINE = ON clauses have not been added to the deployment script."),
        )
    }

    pub fn online_clustered_columnstore(object: &str, version: &str) -> Warning {
        w(
            Severity::Low,
            object,
            format!("Online creation of clustered columnstore indexes are only supported in SQL Server 2019 and later. As the target server is SQL Server {version}, ONLINE = ON clauses for clustered columnstore indexes have not been added to the deployment script."),
        )
    }

    pub fn online_nonclustered_columnstore(object: &str, version: &str) -> Warning {
        w(
            Severity::Low,
            object,
            format!("Online creation of non-clustered columnstore indexes are only supported in SQL Server 2017 and later. As the target server is SQL Server {version}, ONLINE = ON clauses for columnstore indexes have not been added to the deployment script."),
        )
    }
}

pub fn incremental_with_ignored_filegroups(t: &str, ix: &str) -> Warning {
    w(
        Severity::High,
        t,
        format!(
            "Indexes using the STATISTICS_INCREMENTAL option and statistics using the INCREMENTAL option are only supported on partitioned tables.\r\n\r\nThe 'Ignore filegroups, partition schemes and partition functions' option is selected, so partition schemes will be ignored.This may cause the deployment of {ix} on {t} to fail.\r\n\r\nTo stop ignoring partition schemes, clear the 'Ignore filegroups, partition schemes and partition functions' option."
        ),
    )
}

pub fn indexes_auto_enabled(t: &str) -> Warning {
    w(
        Severity::Low,
        t,
        "Primary key is being dropped on a table with disabled non-clustered indexes. SQL Server will automatically re-enable the disabled non-clustered indexes.".into(),
    )
}

pub fn sequence_restarted(q: &str) -> Warning {
    w(
        Severity::Low,
        q,
        format!(
            "The deployment will restart the sequence {q} in the target database with the starting value of the sequence in the source database.\r\n\r\n\r\nIf you want to preserve the value of the sequence in the target database, you'll need to modify the script to run the following:\r\n\r\nALTER SEQUENCE {q} RESTART WITH <value from the source database>"
        ),
    )
}

pub fn sequence_restarting(q: &str) -> Warning {
    w(Severity::Medium, q, format!("The sequence {q} needs to be restarted as the current start value falls outside of the new range. "))
}

pub fn function_type_change(f: &str) -> Warning {
    w(Severity::Low, f, format!("The function {f} has changed its type. It must be dropped and recreated."))
}

/// `kind`: view, function, procedure, trigger or DDL trigger.
pub fn nondecryptable(kind: &str, x: &str) -> Warning {
    w(
        Severity::High,
        x,
        format!("The {kind} {x} cannot be decrypted in one or more of the datasources. Its contents cannot be compared and it cannot be synchronized."),
    )
}

pub fn nondecryptable_dependencies(kind: &str, x: &str) -> Warning {
    w(
        Severity::Medium,
        x,
        format!("The {kind} {x} cannot be decrypted in one or more of the datasources. Its dependencies cannot be exactly identified. The script may fail."),
    )
}

pub fn rebind_might_fail(x: &str) -> Warning {
    w(Severity::Medium, x, format!("{x} will have its schemabinding dropped and reapplied. This operation could fail if objects it consumes are no longer compatible."))
}

pub fn schema_drop(s: &str) -> Warning {
    w(
        Severity::Low,
        s,
        format!("Dropping a schema is a potentially dangerous operation. Dependencies for the {s} schema are not selected automatically. If you are sure you want to drop this schema please select the objects that belong to this schema manually."),
    )
}

pub fn user_drop(u: &str) -> Warning {
    w(
        Severity::Low,
        u,
        format!("Dropping a user is a potentially dangerous operation. Dependencies for the {u} user are not selected automatically. If you are sure you want to drop this user please select the objects that belong to this user manually."),
    )
}

pub fn type_drop(ty: &str) -> Warning {
    w(
        Severity::Low,
        ty,
        format!("Dropping a user defined type is a potentially dangerous operation. Dependencies for the {ty} type are not selected automatically. If you are sure you want to drop this user defined type please select the objects that belong to this user defined type manually."),
    )
}

pub fn user_without_login(u: &str) -> Warning {
    w(
        Severity::Medium,
        u,
        format!("The user {u} will be created without an associated login. If this user should be a container user or a user with an assoicated login, edit the deployment script before running, or cancel and create the user manually. You can also associate a user with a login after running the deployment script using sp_change_users_login."),
    )
}

pub fn login_may_not_exist(u: &str) -> Warning {
    w(Severity::Low, u, format!("The login for user {u} may not exist in the target database."))
}

pub fn strong_dependencies(owner: &str, list: &str) -> Warning {
    w(
        Severity::High,
        owner,
        format!("The following objects that reference, or are referenced by, the objects you want to deploy aren't included in the deployment:\r\n\r\n{list}\r\n\r\nIf you don't include these referenced objects, the deployment script may fail or produce unexpected results."),
    )
}

/// Warnings of the objects beyond tables and modules.
pub mod objects {
    use super::*;

    /// `kind` as in the message: message type, contract, queue, service, route, event
    /// notification, service binding, security policy...
    pub fn unsupported(kind: &str, x: &str) -> Warning {
        w(Severity::High, x, format!("The {kind} {x} is not supported in the target database version."))
    }

    pub fn polybase_unsupported(what: &str, x: &str) -> Warning {
        w(Severity::High, x, format!("PolyBase external {what} are not supported in database versions older than SQL Server 2016."))
    }

    pub fn polybase_platform(x: &str) -> Warning {
        w(
            Severity::High,
            x,
            "The PolyBase syntax for on-premises SQL Server is different to that for an Azure SQL Database, and will not be compatible.".into(),
        )
    }

    pub fn missing_credential(x: &str, credential: &str) -> Warning {
        w(
            Severity::High,
            x,
            format!("The target database does not have the required database scoped credential {credential}. The deployment will fail unless this is present."),
        )
    }

    /// `what`: external file formats, external data sources or external tables.
    pub fn polybase_isolation(what: &str, level: &str) -> Warning {
        w(
            Severity::High,
            "",
            format!("SQL Server only supports creating {what} in transactions when the transaction isolation level is READ UNCOMMITTED or READ COMMITTED.\r\n\r\nThe deployment uses {level}."),
        )
    }

    pub fn cannot_be_recreated(kind: &str, x: &str) -> Warning {
        w(Severity::High, x, format!("The {kind} {x} cannot be recreated. The script may fail."))
    }

    pub fn symmetric_key_contents(x: &str) -> Warning {
        w(
            Severity::Medium,
            x,
            format!("The contents of symmetric keys cannot be compared. QuillSQL Compare will only compare the owner and the permissions if required for {x}."),
        )
    }

    pub fn application_role_password(r: &str) -> Warning {
        w(Severity::Low, r, format!("The application role {r} will be created with a default password of 'p@ssw0rd'"))
    }

    pub fn non_local_route(r: &str) -> Warning {
        w(Severity::High, r, format!("The route {r} contains a non-local address which is not supported in Microsoft Azure managed instances."))
    }

    pub fn statistics_sample(t: &str, s: &str) -> Warning {
        w(
            Severity::Low,
            t,
            format!("Creating statistics [{s}] on {t}. When deploying statistics, the SAMPLE option isn\u{2019}t supported. If you're deploying statistics that use SAMPLE, you'll need to manually modify the deployment script for those objects."),
        )
    }

    pub fn edge_constraint_unsupported(t: &str, e: &str) -> Warning {
        w(Severity::High, t, format!("The edge constraint {e} on table {t} is not supported in the target database version."))
    }

    pub fn xml_collection_rebuild(x: &str, column: &str, t: &str) -> Warning {
        w(
            Severity::Low,
            x,
            format!("The XML schema collection {x} needs to be rebuilt. It will be unbound from the {column} column of the {t} table. After the rebuild the XML schema collection will be rebound to the table. The data in the table will be preserved."),
        )
    }

    pub fn clr_type_unbind(ty: &str) -> Warning {
        w(
            Severity::High,
            ty,
            format!("The CLR type {ty} needs to be unbound from dependent tables, but this is not possible without data loss. If the tables in question are not selected explicitly the script may fail."),
        )
    }

    pub fn fulltext_in_transaction() -> Warning {
        w(
            Severity::High,
            "",
            "Full-text indexes can't be deployed in transactions, so QuillSQL Compare will deploy changes to full-text indexes after transactions are complete. If other objects rely on changes to full-text indexes (using CONTAINS, FULLTEXT, CONTAINSTABLE or FREETEXTTABLE), the script may fail.\r\n\r\nWe recommend you select the 'Do not use transactions in deployment scripts' project option when deploying full-text indexes.".into(),
        )
    }

    pub fn label_without_name(column: &str, information_type: bool) -> Warning {
        let (what, missing) = if information_type { ("information type id", "information type") } else { ("label id", "label") };
        w(
            Severity::Medium,
            column,
            format!("The sensitivity classification on column {column} contains a {what} without a corresponding {missing}. This deployment will add a {missing} with the same value as the {what}."),
        )
    }

    pub fn partitioning_in_use(x: &str, users: &str) -> Warning {
        w(
            Severity::High,
            x,
            format!("{x} has to be dropped and created again, but {users} still use it. Rebuild those tables and indexes on another filegroup first, or the script will fail."),
        )
    }

    pub fn retention_shortened(t: &str) -> Warning {
        w(Severity::High, t, format!("This deployment shortens the history retention period for table {t}. This may cause a loss of history data for this table."))
    }
}

/// `kind` as SQL Compare names the user kind (MicrosoftEntraUser…).
pub fn user_type_unsupported(u: &str, kind: &str) -> Warning {
    w(Severity::High, u, format!("The user type {kind} is not supported in the target database version."))
}

pub fn entra_login_unsupported(u: &str) -> Warning {
    w(Severity::High, u, format!("The Microsoft Entra user {u} is mapped to a server-level login, which is not supported in the target database version."))
}

pub fn user_unsyncable(u: &str) -> Warning {
    w(Severity::Medium, u, format!("The user {u} has properties that cannot be synchronized."))
}

pub fn history_not_null_default(column: &str, history: &str) -> Warning {
    w(Severity::Low, history, format!("The column [{column}] on history table {history} is being marked as NOT NULL. The default value will be applied where appropriate."))
}
