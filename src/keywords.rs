//! T-SQL reserved words. These can never be unquoted identifiers, which is what
//! makes upper-casing them safe under any collation.

const RESERVED: &[&str] = &[
    "ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "AUTHORIZATION", "BACKUP", "BEGIN", "BETWEEN",
    "BREAK", "BROWSE", "BULK", "BY", "CASCADE", "CASE", "CHECK", "CHECKPOINT", "CLOSE", "CLUSTERED",
    "COALESCE", "COLLATE", "COLUMN", "COMMIT", "COMPUTE", "CONSTRAINT", "CONTAINS", "CONTAINSTABLE",
    "CONTINUE", "CONVERT", "CREATE", "CROSS", "CURRENT", "CURRENT_DATE", "CURRENT_TIME",
    "CURRENT_TIMESTAMP", "CURRENT_USER", "CURSOR", "DATABASE", "DBCC", "DEALLOCATE", "DECLARE",
    "DEFAULT", "DELETE", "DENY", "DESC", "DISK", "DISTINCT", "DISTRIBUTED", "DOUBLE", "DROP", "DUMP",
    "ELSE", "END", "ERRLVL", "ESCAPE", "EXCEPT", "EXEC", "EXECUTE", "EXISTS", "EXIT", "EXTERNAL",
    "FETCH", "FILE", "FILLFACTOR", "FOR", "FOREIGN", "FREETEXT", "FREETEXTTABLE", "FROM", "FULL",
    "FUNCTION", "GOTO", "GRANT", "GROUP", "HAVING", "HOLDLOCK", "IDENTITY", "IDENTITYCOL",
    "IDENTITY_INSERT", "IF", "IN", "INDEX", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN",
    "KEY", "KILL", "LEFT", "LIKE", "LINENO", "LOAD", "MERGE", "NATIONAL", "NOCHECK", "NONCLUSTERED",
    "NOT", "NULL", "NULLIF", "OF", "OFF", "OFFSETS", "ON", "OPEN", "OPENDATASOURCE", "OPENQUERY",
    "OPENROWSET", "OPENXML", "OPTION", "OR", "ORDER", "OUTER", "OVER", "PERCENT", "PIVOT", "PLAN",
    "PRECISION", "PRIMARY", "PRINT", "PROC", "PROCEDURE", "PUBLIC", "RAISERROR", "READ", "READTEXT",
    "RECONFIGURE", "REFERENCES", "REPLICATION", "RESTORE", "RESTRICT", "RETURN", "REVERT", "REVOKE",
    "RIGHT", "ROLLBACK", "ROWCOUNT", "ROWGUIDCOL", "RULE", "SAVE", "SCHEMA", "SECURITYAUDIT",
    "SELECT", "SEMANTICKEYPHRASETABLE", "SEMANTICSIMILARITYDETAILSTABLE",
    "SEMANTICSIMILARITYTABLE", "SESSION_USER", "SET", "SETUSER", "SHUTDOWN", "SOME", "STATISTICS",
    "SYSTEM_USER", "TABLE", "TABLESAMPLE", "TEXTSIZE", "THEN", "TO", "TOP", "TRAN", "TRANSACTION",
    "TRIGGER", "TRUNCATE", "TRY_CONVERT", "TSEQUAL", "UNION", "UNIQUE", "UNPIVOT", "UPDATE",
    "UPDATETEXT", "USE", "USER", "VALUES", "VARYING", "VIEW", "WAITFOR", "WHEN", "WHERE", "WHILE",
    "WITH", "WITHIN", "WRITETEXT",
];

/// Reserved words written like function calls: no space before their `(`.
const CALL_LIKE: &[&str] = &[
    "COALESCE", "CONTAINS", "CONTAINSTABLE", "CONVERT", "FREETEXT", "FREETEXTTABLE", "IDENTITY",
    "LEFT", "NULLIF", "OPENDATASOURCE", "OPENQUERY", "OPENROWSET", "OPENXML", "RIGHT",
    "TRY_CONVERT",
];

/// Unreserved words that must keep their brackets, or must not be read as an
/// alias, because the parser gives them meaning in some position.
const CONTEXTUAL: &[&str] = &[
    "APPLY", "DELETED", "DISABLE", "ENABLE", "GET", "GO", "INSERTED", "MOVE", "RANGE", "RECEIVE",
    "ROW", "ROWS", "SEND", "THROW", "WINDOW",
];

pub(crate) const DATA_TYPES: &[&str] = &[
    "BIGINT", "BINARY", "BIT", "CHAR", "CURSOR", "DATE", "DATETIME", "DATETIME2", "DATETIMEOFFSET",
    "DECIMAL", "FLOAT", "GEOGRAPHY", "GEOMETRY", "HIERARCHYID", "IMAGE", "INT", "MONEY", "NCHAR",
    "NTEXT", "NUMERIC", "NVARCHAR", "REAL", "ROWVERSION", "SMALLDATETIME", "SMALLINT",
    "SMALLMONEY", "SQL_VARIANT", "SYSNAME", "TEXT", "TIME", "TIMESTAMP", "TINYINT",
    "UNIQUEIDENTIFIER", "VARBINARY", "VARCHAR", "XML",
];

/// A built-in function whose name is not reserved (those are keywords). Matched only when
/// called, so a column with the same name is left alone.
pub fn is_function(upper: &str) -> bool {
    (crate::functions::find(upper).is_some() && !is_reserved(upper)) || is_call_like(upper)
}

pub fn is_data_type(upper: &str) -> bool {
    DATA_TYPES.binary_search(&upper).is_ok()
}

pub fn is_reserved(upper: &str) -> bool {
    RESERVED.binary_search(&upper).is_ok()
}

pub fn is_call_like(upper: &str) -> bool {
    CALL_LIKE.contains(&upper)
}

pub fn is_contextual(upper: &str) -> bool {
    CONTEXTUAL.contains(&upper)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_are_sorted_for_binary_search() {
        for list in [super::RESERVED, super::DATA_TYPES] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "{:?}", list.windows(2).find(|w| w[0] >= w[1]));
        }
    }
}
