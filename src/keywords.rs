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

/// Built-in functions whose name is not reserved. Matched only when called, so a column
/// with the same name is left alone.
pub const FUNCTIONS: &[&str] = &[
    "ABS", "ACOS", "APPROX_COUNT_DISTINCT", "ASCII", "ASIN", "ATAN", "ATN2", "AVG", "CAST",
    "CEILING", "CHAR", "CHARINDEX", "CHECKSUM", "CHECKSUM_AGG", "CHOOSE", "CONCAT", "CONCAT_WS",
    "COS", "COT", "COUNT", "COUNT_BIG", "CUME_DIST", "CURSOR_STATUS", "DATALENGTH", "DATEADD",
    "DATEDIFF", "DATEDIFF_BIG", "DATEFROMPARTS", "DATENAME", "DATEPART", "DATETIME2FROMPARTS",
    "DATETIMEFROMPARTS", "DATETRUNC", "DATE_BUCKET", "DAY", "DB_ID", "DB_NAME", "DEGREES",
    "DENSE_RANK", "DIFFERENCE", "EOMONTH", "ERROR_LINE", "ERROR_MESSAGE", "ERROR_NUMBER",
    "ERROR_PROCEDURE", "ERROR_SEVERITY", "ERROR_STATE", "EXP", "FIRST_VALUE", "FLOOR", "FORMAT",
    "FORMATMESSAGE", "GETDATE", "GETUTCDATE", "GREATEST", "GROUPING", "GROUPING_ID", "HASHBYTES",
    "IIF", "ISDATE", "ISJSON", "ISNULL", "ISNUMERIC", "JSON_ARRAY", "JSON_MODIFY", "JSON_OBJECT",
    "JSON_QUERY", "JSON_VALUE", "LAG", "LAST_VALUE", "LEAD", "LEAST", "LEN", "LOG", "LOG10",
    "LOWER", "LTRIM", "MAX", "MIN", "MONTH", "NCHAR", "NEWID", "NEWSEQUENTIALID", "NTILE",
    "OBJECT_ID", "OBJECT_NAME", "OBJECT_SCHEMA_NAME", "PARSE", "PATINDEX", "PERCENTILE_CONT",
    "PERCENTILE_DISC", "PERCENT_RANK", "PI", "POWER", "QUOTENAME", "RADIANS", "RAND", "RANK",
    "REPLACE", "REPLICATE", "REVERSE", "ROUND", "ROW_NUMBER", "RTRIM", "SCHEMA_ID", "SCHEMA_NAME",
    "SCOPE_IDENTITY", "SIGN", "SIN", "SOUNDEX", "SPACE", "SQRT", "SQUARE", "STDEV", "STDEVP",
    "STR", "STRING_AGG", "STRING_ESCAPE", "STRING_SPLIT", "STUFF", "SUBSTRING", "SUM",
    "SUSER_NAME", "SUSER_SNAME", "SWITCHOFFSET", "SYSDATETIME", "SYSDATETIMEOFFSET",
    "SYSUTCDATETIME", "TAN", "TIMEFROMPARTS", "TODATETIMEOFFSET", "TRANSLATE", "TRIM",
    "TRY_CAST", "TRY_PARSE", "TYPE_ID", "TYPE_NAME", "UNICODE", "UPPER", "USER_NAME", "VAR",
    "VARP", "XACT_STATE", "YEAR",
];

const DATA_TYPES: &[&str] = &[
    "BIGINT", "BINARY", "BIT", "CHAR", "CURSOR", "DATE", "DATETIME", "DATETIME2", "DATETIMEOFFSET",
    "DECIMAL", "FLOAT", "GEOGRAPHY", "GEOMETRY", "HIERARCHYID", "IMAGE", "INT", "MONEY", "NCHAR",
    "NTEXT", "NUMERIC", "NVARCHAR", "REAL", "ROWVERSION", "SMALLDATETIME", "SMALLINT",
    "SMALLMONEY", "SQL_VARIANT", "SYSNAME", "TEXT", "TIME", "TIMESTAMP", "TINYINT",
    "UNIQUEIDENTIFIER", "VARBINARY", "VARCHAR", "XML",
];

pub fn is_function(upper: &str) -> bool {
    FUNCTIONS.binary_search(&upper).is_ok() || is_call_like(upper)
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
        for list in [super::RESERVED, super::FUNCTIONS, super::DATA_TYPES] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "{:?}", list.windows(2).find(|w| w[0] >= w[1]));
        }
    }
}
