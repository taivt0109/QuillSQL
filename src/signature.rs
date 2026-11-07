//! Parameter hints: the signature of the function or procedure whose arguments the caret is in,
//! and which argument it is at.

use serde::Serialize;

use crate::complete::{Schema, bare, eq, is_name, object_matches, utf16_to_byte};
use crate::keywords::is_reserved;
use crate::lexer::{Kind, Token, lex_partial};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Signature {
    /// The name as it is shown: `DATEADD`, `dbo.usp_GetOrders`.
    pub name: String,
    /// Each parameter as it is shown: `datepart`, `@CustomerId int`.
    pub params: Vec<String>,
    /// The parameter the caret is at; past the last one for extra arguments.
    pub active: usize,
    /// What the function returns, if known.
    pub returns: String,
}

/// Built-in functions: name, parameters (`[...]` optional) and what they return.
const BUILT_IN: &[(&str, &[&str], &str)] = &[
    ("ABS", &["numeric_expression"], "same type"),
    ("APPROX_COUNT_DISTINCT", &["expression"], "bigint"),
    ("AVG", &["[ALL | DISTINCT] expression"], "numeric"),
    ("CAST", &["expression AS data_type[(length)]"], "data_type"),
    ("CEILING", &["numeric_expression"], "same type"),
    ("CHARINDEX", &["expression_to_find", "expression_to_search", "[start_location]"], "int"),
    ("CHECKSUM", &["* | expression [, ...]"], "int"),
    ("CHOOSE", &["index", "value_1", "value_2 [, ...]"], "highest precedence type"),
    ("COALESCE", &["expression", "expression [, ...]"], "highest precedence type"),
    ("CONCAT", &["string_value", "string_value [, ...]"], "string"),
    ("CONCAT_WS", &["separator", "argument", "argument [, ...]"], "string"),
    ("CONVERT", &["data_type[(length)]", "expression", "[style]"], "data_type"),
    ("COUNT", &["{ * | [ALL | DISTINCT] expression }"], "int"),
    ("COUNT_BIG", &["{ * | [ALL | DISTINCT] expression }"], "bigint"),
    ("DATALENGTH", &["expression"], "int"),
    ("DATEADD", &["datepart", "number", "date"], "date type of date"),
    ("DATEDIFF", &["datepart", "startdate", "enddate"], "int"),
    ("DATEDIFF_BIG", &["datepart", "startdate", "enddate"], "bigint"),
    ("DATEFROMPARTS", &["year", "month", "day"], "date"),
    ("DATENAME", &["datepart", "date"], "nvarchar"),
    ("DATEPART", &["datepart", "date"], "int"),
    ("DATETIME2FROMPARTS", &["year", "month", "day", "hour", "minute", "seconds", "fractions", "precision"], "datetime2"),
    ("DATETIMEFROMPARTS", &["year", "month", "day", "hour", "minute", "seconds", "milliseconds"], "datetime"),
    ("DATETRUNC", &["datepart", "date"], "date type of date"),
    ("DAY", &["date"], "int"),
    ("DB_ID", &["['database_name']"], "int"),
    ("DB_NAME", &["[database_id]"], "nvarchar(128)"),
    ("DENSE_RANK", &[], "bigint · OVER ([PARTITION BY ...] ORDER BY ...)"),
    ("EOMONTH", &["start_date", "[month_to_add]"], "date"),
    ("FIRST_VALUE", &["scalar_expression"], "same type · OVER (...)"),
    ("FLOOR", &["numeric_expression"], "same type"),
    ("FORMAT", &["value", "format", "[culture]"], "nvarchar"),
    ("GETDATE", &[], "datetime"),
    ("GETUTCDATE", &[], "datetime"),
    ("GREATEST", &["expression1", "expression2 [, ...]"], "highest precedence type"),
    ("HASHBYTES", &["'algorithm'", "input"], "varbinary(8000)"),
    ("IIF", &["boolean_expression", "true_value", "false_value"], "highest precedence type"),
    ("ISDATE", &["expression"], "int"),
    ("ISJSON", &["expression", "[json_type_constraint]"], "bit"),
    ("ISNULL", &["check_expression", "replacement_value"], "type of check_expression"),
    ("ISNUMERIC", &["expression"], "int"),
    ("JSON_MODIFY", &["expression", "path", "newValue"], "nvarchar(max)"),
    ("JSON_QUERY", &["expression", "[path]"], "nvarchar(max)"),
    ("JSON_VALUE", &["expression", "path"], "nvarchar(4000)"),
    ("LAG", &["scalar_expression", "[offset]", "[default]"], "same type · OVER (...)"),
    ("LAST_VALUE", &["scalar_expression"], "same type · OVER (...)"),
    ("LEAD", &["scalar_expression", "[offset]", "[default]"], "same type · OVER (...)"),
    ("LEAST", &["expression1", "expression2 [, ...]"], "highest precedence type"),
    ("LEFT", &["character_expression", "integer_expression"], "varchar or nvarchar"),
    ("LEN", &["string_expression"], "int"),
    ("LOG", &["float_expression", "[base]"], "float"),
    ("LOWER", &["character_expression"], "varchar or nvarchar"),
    ("LTRIM", &["character_expression", "[characters]"], "varchar or nvarchar"),
    ("MAX", &["[ALL | DISTINCT] expression"], "same type"),
    ("MIN", &["[ALL | DISTINCT] expression"], "same type"),
    ("MONTH", &["date"], "int"),
    ("NEWID", &[], "uniqueidentifier"),
    ("NTILE", &["integer_expression"], "bigint · OVER (...)"),
    ("NULLIF", &["expression", "expression"], "type of the first expression"),
    ("OBJECT_ID", &["'object_name'", "['object_type']"], "int"),
    ("OBJECT_NAME", &["object_id", "[database_id]"], "sysname"),
    ("OBJECT_SCHEMA_NAME", &["object_id", "[database_id]"], "sysname"),
    ("OPENJSON", &["jsonExpression", "[path]"], "rows · [WITH (...)]"),
    ("PARSE", &["string_value AS data_type [USING culture]"], "data_type"),
    ("PATINDEX", &["'%pattern%'", "expression"], "int"),
    ("POWER", &["float_expression", "y"], "same type"),
    ("QUOTENAME", &["'character_string'", "['quote_character']"], "nvarchar(258)"),
    ("RANK", &[], "bigint · OVER ([PARTITION BY ...] ORDER BY ...)"),
    ("REPLACE", &["string_expression", "string_pattern", "string_replacement"], "varchar or nvarchar"),
    ("REPLICATE", &["string_expression", "integer_expression"], "same type"),
    ("REVERSE", &["string_expression"], "varchar or nvarchar"),
    ("RIGHT", &["character_expression", "integer_expression"], "varchar or nvarchar"),
    ("ROUND", &["numeric_expression", "length", "[function]"], "same type"),
    ("ROW_NUMBER", &[], "bigint · OVER ([PARTITION BY ...] ORDER BY ...)"),
    ("RTRIM", &["character_expression", "[characters]"], "varchar or nvarchar"),
    ("SCHEMA_NAME", &["[schema_id]"], "sysname"),
    ("SCOPE_IDENTITY", &[], "numeric(38,0)"),
    ("SIGN", &["numeric_expression"], "same type"),
    ("SPACE", &["integer_expression"], "varchar"),
    ("SQRT", &["float_expression"], "float"),
    ("STR", &["float_expression", "[length]", "[decimal]"], "varchar"),
    ("STRING_AGG", &["expression", "separator"], "string · [WITHIN GROUP (ORDER BY ...)]"),
    ("STRING_ESCAPE", &["text", "type"], "nvarchar(max)"),
    ("STRING_SPLIT", &["string", "separator", "[enable_ordinal]"], "rows (value[, ordinal])"),
    ("STUFF", &["character_expression", "start", "length", "replace_with_expression"], "same type"),
    ("SUBSTRING", &["expression", "start", "length"], "same type"),
    ("SUM", &["[ALL | DISTINCT] expression"], "numeric"),
    ("SWITCHOFFSET", &["datetimeoffset_expression", "timezoneoffset_expression"], "datetimeoffset"),
    ("SYSDATETIME", &[], "datetime2(7)"),
    ("SYSDATETIMEOFFSET", &[], "datetimeoffset(7)"),
    ("SYSUTCDATETIME", &[], "datetime2(7)"),
    ("TODATETIMEOFFSET", &["datetime_expression", "timezoneoffset_expression"], "datetimeoffset"),
    ("TRANSLATE", &["inputString", "characters", "translations"], "same type"),
    ("TRIM", &["[characters FROM] string"], "varchar or nvarchar"),
    ("TRY_CAST", &["expression AS data_type[(length)]"], "data_type or NULL"),
    ("TRY_CONVERT", &["data_type[(length)]", "expression", "[style]"], "data_type or NULL"),
    ("TRY_PARSE", &["string_value AS data_type [USING culture]"], "data_type or NULL"),
    ("UNICODE", &["ncharacter_expression"], "int"),
    ("UPPER", &["character_expression"], "varchar or nvarchar"),
    ("YEAR", &["date"], "int"),
];

/// The signature at the caret, or None when it is not inside a call's arguments.
pub fn signature_at(text: &str, caret_utf16: usize, schema: Option<&Schema>) -> Option<Signature> {
    let caret = utf16_to_byte(text, caret_utf16);
    let (head, error) = lex_partial(&text[..caret]);
    // Not inside a string or comment.
    if error.is_some() {
        return None;
    }
    if head.last().is_some_and(|t| t.kind == Kind::LineComment) {
        return None;
    }
    let code: Vec<&Token> = head.iter().filter(|t| !t.is_comment()).collect();

    // The innermost `(` still open at the caret, with the commas after it at its own depth.
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut open = None;
    for (i, t) in code.iter().enumerate().rev() {
        match t.kind {
            Kind::RParen => depth += 1,
            Kind::LParen if depth == 0 => {
                open = Some(i);
                break;
            }
            Kind::LParen => depth -= 1,
            Kind::Comma if depth == 0 => commas += 1,
            Kind::Semi | Kind::Go => break,
            _ => {}
        }
    }
    if let Some(o) = open {
        return if o > 0 { call(&code, o, commas, schema) } else { None };
    }
    let touching = text[..caret].chars().last().is_some_and(|c| !c.is_whitespace());
    exec(&code, touching, schema)
}

/// The function called by `name (` with the `(` at `o`.
fn call(code: &[&Token], o: usize, commas: usize, schema: Option<&Schema>) -> Option<Signature> {
    let mut j = o - 1;
    if !matches!(code[j].kind, Kind::Word | Kind::Bracket | Kind::Quoted) {
        return None;
    }
    let mut parts = vec![bare(code[j])];
    while j >= 2 && code[j - 1].kind == Kind::Dot && is_name(code[j - 2]) {
        parts.insert(0, bare(code[j - 2]));
        j -= 2;
    }
    if parts.len() == 1 {
        let upper = parts[0].to_ascii_uppercase();
        if let Some((name, params, returns)) = BUILT_IN.iter().find(|(n, _, _)| *n == upper) {
            return Some(Signature { name: name.to_string(), params: params.iter().map(|p| p.to_string()).collect(), active: commas, returns: returns.to_string() });
        }
        if is_reserved(&upper) {
            return None;
        }
    }
    let schema = schema?;
    let o = schema.objects.iter().find(|o| matches!(o.kind.as_str(), "scalar_function" | "table_function") && object_matches(o, &parts, schema))?;
    Some(Signature {
        name: format!("{}.{}", o.schema, o.name),
        params: o.params.iter().map(|p| format!("{} {}", p.name, p.data_type)).collect(),
        active: commas,
        returns: if o.kind == "table_function" { "table".into() } else { String::new() },
    })
}

/// `EXEC name arguments`: the procedure's parameters, and the one the caret is at — by name when
/// the argument names it (`@Id = `), else by position.
fn exec(code: &[&Token], touching: bool, schema: Option<&Schema>) -> Option<Signature> {
    let schema = schema?;
    let start = code.iter().rposition(|t| t.kind == Kind::Word && matches!(t.text.to_ascii_uppercase().as_str(), "EXEC" | "EXECUTE"))?;
    // Only within that statement: no statement keyword after EXEC.
    let mut j = start + 1;
    // `EXEC @result = name`
    if j + 1 < code.len() && code[j].kind == Kind::Variable && code[j + 1].text == "=" {
        j += 2;
    }
    if j >= code.len() || !is_name(code[j]) {
        return None;
    }
    let mut parts = vec![bare(code[j])];
    j += 1;
    while j + 1 < code.len() && code[j].kind == Kind::Dot && is_name(code[j + 1]) {
        parts.push(bare(code[j + 1]));
        j += 2;
    }
    // The caret must be after the name and a space, not still in it.
    if j > code.len() || (j == code.len() && touching) {
        return None;
    }
    let args = &code[j..];
    if args.iter().any(|t| t.kind == Kind::Word && matches!(t.text.to_ascii_uppercase().as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "SET" | "DECLARE" | "IF" | "BEGIN" | "END")) {
        return None;
    }
    let proc = schema.objects.iter().find(|o| o.kind == "procedure" && object_matches(o, &parts, schema))?;
    let commas = args.iter().filter(|t| t.kind == Kind::Comma).count();
    // The argument being written starts after the last comma.
    let current = args.iter().rposition(|t| t.kind == Kind::Comma).map_or(0, |c| c + 1);
    let named = args.get(current).filter(|t| t.kind == Kind::Variable).and_then(|t| proc.params.iter().position(|p| eq(&p.name, &t.text)));
    Some(Signature {
        name: format!("{}.{}", proc.schema, proc.name),
        params: proc.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect(),
        active: named.unwrap_or(commas),
        returns: String::new(),
    })
}
