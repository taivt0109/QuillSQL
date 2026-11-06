//! Existence checks (ObjectExistenceChecks, SchemaExistenceChecks, RoleExistenceChecks): a
//! statement is prefixed with an `IF` line that tests the object is there (drop, alter) or not
//! (create). Statements that must start a batch are run through sp_executesql.

use super::ddl::{qname, sql_string};
use super::options::{Opt, Options};
use crate::catalog::ObjectName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Create,
    Alter,
    Drop,
}

impl Check {
    fn is_create(self) -> bool {
        self == Check::Create
    }

    /// `IS NULL` for a create, `IS NOT NULL` otherwise.
    fn is_null(self) -> &'static str {
        if self.is_create() { "IS NULL" } else { "IS NOT NULL" }
    }

    /// `NOT EXISTS` for a create, `EXISTS` otherwise.
    fn exists(self) -> &'static str {
        if self.is_create() { "NOT EXISTS" } else { "EXISTS" }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Checks {
    objects: bool,
    schemas: bool,
    roles: bool,
    case_sensitive: bool,
}

fn guard(test: String, sql: &str) -> String {
    format!("IF {test}\r\n{sql}")
}

fn exec(sql: &str) -> String {
    format!("EXEC sp_executesql {}", sql_string(sql))
}

fn object_id(name: &ObjectName, code: &str) -> String {
    format!("OBJECT_ID({}, '{code}')", sql_string(&name.quoted()))
}

impl Checks {
    pub fn new(o: &Options) -> Checks {
        Checks {
            objects: o.has(Opt::ObjectExistenceChecks),
            schemas: o.has(Opt::ObjectExistenceChecks) || o.has(Opt::SchemaExistenceChecks),
            roles: o.has(Opt::ObjectExistenceChecks) || o.has(Opt::RoleExistenceChecks),
            case_sensitive: o.has(Opt::CaseSensitiveObjectDefinition),
        }
    }

    pub fn enabled(&self) -> bool {
        self.objects
    }

    /// Tables, synonyms, sequences, rules, defaults: `OBJECT_ID(…, 'U') IS NULL`.
    pub fn object(&self, name: &ObjectName, code: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("{} {}", object_id(name, code), check.is_null()), sql)
    }

    /// Views, procedures and DML triggers (`code` V, P, PC, TR, TA); creates and alters of
    /// views and procedures, and trigger creates, run through sp_executesql.
    pub fn module(&self, name: &ObjectName, code: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        let wrap = match code {
            "TR" | "TA" => check == Check::Create,
            _ => check != Check::Drop,
        };
        guard(format!("{} {}", object_id(name, code), check.is_null()), &if wrap { exec(sql) } else { sql.to_string() })
    }

    /// Functions: any of the function types; aggregates by their own code.
    pub fn function(&self, name: &ObjectName, aggregate: bool, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        let body = if check == Check::Drop { sql.to_string() } else { exec(sql) };
        if aggregate {
            return guard(format!("{} {}", object_id(name, "AF"), check.is_null()), &body);
        }
        guard(
            format!(
                "{} (SELECT 1 FROM sys.objects WHERE object_id = OBJECT_ID({}) AND (type = 'IF' OR type = 'FN' OR type = 'TF' OR type = 'FT' OR type = 'FS'))",
                check.exists(),
                sql_string(&name.quoted())
            ),
            &body,
        )
    }

    /// Indexes and statistics other than keys.
    pub fn index(&self, table: &ObjectName, index: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(
            format!("{} (SELECT 1 FROM sys.indexes WHERE name = {} AND object_id = OBJECT_ID({}))", check.exists(), sql_string(index), sql_string(&table.quoted())),
            sql,
        )
    }

    /// Primary keys and unique constraints.
    pub fn key(&self, table: &ObjectName, name: &str, primary: bool, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(
            format!(
                "{} (SELECT 1 FROM sys.objects WHERE object_id = OBJECT_ID({}, '{}') AND parent_object_id = {})",
                check.exists(),
                sql_string(&qname(&table.schema, name)),
                if primary { "PK" } else { "UQ" },
                object_id(table, "U")
            ),
            sql,
        )
    }

    pub fn check_constraint(&self, table: &ObjectName, name: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(
            format!(
                "{} (SELECT 1 FROM sys.check_constraints WHERE object_id = OBJECT_ID({}, 'C') AND parent_object_id = {})",
                check.exists(),
                sql_string(&qname(&table.schema, name)),
                object_id(table, "U")
            ),
            sql,
        )
    }

    pub fn default_constraint(&self, table: &ObjectName, column: &str, name: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(
            format!(
                "{} (SELECT 1 FROM sys.columns WHERE name = {} AND object_id = {} AND default_object_id = OBJECT_ID({}, 'D'))",
                check.exists(),
                sql_string(column),
                object_id(table, "U"),
                sql_string(&qname(&table.schema, name))
            ),
            sql,
        )
    }

    pub fn foreign_key(&self, table: &ObjectName, name: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(
            format!(
                "{} (SELECT 1 FROM sys.foreign_keys WHERE object_id = OBJECT_ID({},'F') AND parent_object_id = {})",
                check.exists(),
                sql_string(&qname(&table.schema, name)),
                object_id(table, "U")
            ),
            sql,
        )
    }

    /// The test for one column (without IF).
    pub fn column_test(&self, table: &ObjectName, column: &str, check: Check) -> String {
        if self.case_sensitive {
            format!(
                "({} (SELECT COLUMN_NAME FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} AND COLUMN_NAME = {} COLLATE Latin1_General_CS_AS))",
                check.exists(),
                sql_string(&table.schema),
                sql_string(&table.name),
                sql_string(column)
            )
        } else {
            format!("COL_LENGTH({}, {}) {}", sql_string(&table.quoted()), sql_string(column), check.is_null())
        }
    }

    /// ADD or DROP of one or more columns guarded together.
    pub fn columns(&self, table: &ObjectName, columns: &[&str], check: Check, sql: &str) -> String {
        if !self.objects || columns.is_empty() {
            return sql.to_string();
        }
        let tests: Vec<String> = columns.iter().map(|c| self.column_test(table, c, check)).collect();
        guard(tests.join(" AND "), sql)
    }

    /// sp_rename of a column: the old name exists and the new one does not.
    pub fn rename_column(&self, table: &ObjectName, old: &str, new: &str, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("({}) AND ({})", self.column_test(table, old, Check::Drop), self.column_test(table, new, Check::Create)), sql)
    }

    /// sp_rename or ALTER SCHEMA TRANSFER of a table.
    pub fn rename_table(&self, old: &ObjectName, new: &ObjectName, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("({} IS NOT NULL) AND ({} IS NULL)", object_id(old, "U"), object_id(new, "U")), sql)
    }

    /// Binding a default or rule object to a column.
    pub fn binding(&self, table: &ObjectName, column: &str, object: &str, rule: bool, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        let (col, code) = if rule { ("rule_object_id", "R") } else { ("default_object_id", "D") };
        guard(
            format!(
                "{} (SELECT 1 FROM sys.columns WHERE name = {} AND object_id = {} AND {col} = OBJECT_ID({}, '{code}'))",
                check.exists(),
                sql_string(column),
                object_id(table, "U"),
                sql_string(object)
            ),
            sql,
        )
    }

    pub fn user_type(&self, name: &ObjectName, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("TYPE_ID({}) {}", sql_string(&name.quoted()), check.is_null()), sql)
    }

    /// Schemas; a create runs through sp_executesql (CREATE SCHEMA must start a batch).
    pub fn schema(&self, name: &str, check: Check, sql: &str) -> String {
        if !self.schemas {
            return sql.to_string();
        }
        let body = if check.is_create() { exec(sql) } else { sql.to_string() };
        guard(format!("SCHEMA_ID({}) {}", sql_string(name), check.is_null()), &body)
    }

    pub fn principal(&self, name: &str, role: bool, check: Check, sql: &str) -> String {
        if !(if role { self.roles } else { self.objects }) {
            return sql.to_string();
        }
        guard(format!("DATABASE_PRINCIPAL_ID({}) {}", sql_string(name), check.is_null()), sql)
    }

    pub fn extended_property(&self, name: &str, levels: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("{} (SELECT 1 FROM fn_listextendedproperty({}, {levels}))", check.exists(), sql_string(name)), sql)
    }

    pub fn change_tracking(&self, table: &ObjectName, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        guard(format!("{} (SELECT 1 FROM sys.change_tracking_tables WHERE object_id = {})", check.exists(), object_id(table, "U")), sql)
    }

    /// DDL triggers.
    pub fn ddl_trigger(&self, name: &str, check: Check, sql: &str) -> String {
        if !self.objects {
            return sql.to_string();
        }
        let body = if check == Check::Drop { sql.to_string() } else { exec(sql) };
        guard(format!("{} (SELECT 1 FROM sys.triggers WHERE name = {} AND parent_class = 0)", check.exists(), sql_string(name)), &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_follow_the_options() {
        let t = ObjectName::new("dbo", "T");
        assert_eq!(Checks::new(&Options::none()).object(&t, "U", Check::Create, "CREATE TABLE x"), "CREATE TABLE x");
        let c = Checks::new(&Options::none().with(Opt::ObjectExistenceChecks));
        assert_eq!(c.object(&t, "U", Check::Drop, "DROP TABLE [dbo].[T]"), "IF OBJECT_ID(N'[dbo].[T]', 'U') IS NOT NULL\r\nDROP TABLE [dbo].[T]");
        assert_eq!(
            c.module(&ObjectName::new("dbo", "v"), "V", Check::Create, "CREATE VIEW [dbo].[v] AS SELECT 'a'"),
            "IF OBJECT_ID(N'[dbo].[v]', 'V') IS NULL\r\nEXEC sp_executesql N'CREATE VIEW [dbo].[v] AS SELECT ''a'''"
        );
        assert_eq!(
            c.foreign_key(&t, "FK", Check::Create, "ALTER TABLE x"),
            "IF NOT EXISTS (SELECT 1 FROM sys.foreign_keys WHERE object_id = OBJECT_ID(N'[dbo].[FK]','F') AND parent_object_id = OBJECT_ID(N'[dbo].[T]', 'U'))\r\nALTER TABLE x"
        );
        assert_eq!(c.columns(&t, &["a", "b"], Check::Create, "ALTER"), "IF COL_LENGTH(N'[dbo].[T]', N'a') IS NULL AND COL_LENGTH(N'[dbo].[T]', N'b') IS NULL\r\nALTER");
        // Schema and role checks have their own switches.
        let s = Checks::new(&Options::none().with(Opt::SchemaExistenceChecks).with(Opt::RoleExistenceChecks));
        assert_eq!(s.schema("Sales", Check::Create, "CREATE SCHEMA [Sales]"), "IF SCHEMA_ID(N'Sales') IS NULL\r\nEXEC sp_executesql N'CREATE SCHEMA [Sales]'");
        assert_eq!(s.principal("r", true, Check::Drop, "DROP ROLE [r]"), "IF DATABASE_PRINCIPAL_ID(N'r') IS NOT NULL\r\nDROP ROLE [r]");
        assert_eq!(s.principal("u", false, Check::Drop, "DROP USER [u]"), "DROP USER [u]");
        assert_eq!(s.object(&t, "U", Check::Drop, "DROP TABLE x"), "DROP TABLE x");
    }
}
