//! Reads sectioned style files: JSON grouped into sections such as `whitespace`, `lists`
//! and `joinStatements`.
//!
//! A sectioned style stores only the values that differ from the defaults, so it is merged
//! over `quill_defaults.json` before use. Keys and enum values compare case-insensitively.

use serde_json::{Map, Value};

use crate::options::{AndOrAlign, Layout, ListBreak, Options, ParenStyle, SemicolonSpace, WordCase};

const DEFAULTS: &str = include_str!("quill_defaults.json");

const SECTIONS: &[&str] = &[
    "metadata", "whitespace", "lists", "parentheses", "casing", "dml", "ddl", "controlFlow", "cte",
    "variables", "joinStatements", "insertStatements", "restoreStatements", "functionCalls",
    "caseExpressions", "operators",
];

/// Options this formatter applies. Any other value that differs from the default is reported.
const SUPPORTED: &[&str] = &[
    "whitespace.numberOfSpacesInTabs",
    "whitespace.newLines.preserveExistingEmptyLinesBetweenStatements",
    "whitespace.newLines.preserveExistingEmptyLinesAfterBatchSeparator",
    "whitespace.newLines.emptyLinesBetweenStatements",
    "whitespace.newLines.emptyLinesAfterBatchSeparator",
    "whitespace.whiteSpaceBeforeSemiColon",
    "lists.placeFirstItemOnNewLine",
    "lists.placeSubsequentItemsOnNewLines",
    "lists.alignSubsequentItemsWithFirstItem",
    "lists.placeCommasBeforeItems",
    "lists.addSpaceBeforeComma",
    "lists.addSpaceAfterComma",
    "parentheses.parenthesisStyle",
    "casing.reservedKeywords",
    "casing.builtInFunctions",
    "casing.builtInDataTypes",
    "dml.addNewLineAfterDistinctAndTopClauses",
    "dml.collapseShortStatements",
    "dml.collapseStatementsShorterThan",
    "dml.collapseShortSubqueries",
    "dml.collapseSubqueriesShorterThan",
    "dml.clauses.clauseAlignment",
    "dml.listItems.placeFromTableOnNewLine",
    "dml.listItems.placeWhereConditionOnNewLine",
    "dml.listItems.placeGroupByAndOrderByOnNewLine",
    "ddl.parenthesisStyle",
    "ddl.collapseShortStatements",
    "ddl.collapseStatementsShorterThan",
    "controlFlow.indentBeginAndEndKeywords",
    "cte.parenthesisStyle",
    "cte.placeNameOnNewLine",
    "cte.placeAsOnNewLine",
    "joinStatements.join.placeOnNewLine",
    "joinStatements.join.keywordAlignment",
    "joinStatements.on.placeOnNewLine",
    "joinStatements.on.keywordAlignment",
    "joinStatements.on.placeConditionOnNewLine",
    "insertStatements.columns.parenthesisStyle",
    "insertStatements.columns.placeSubsequentColumnsOnNewLines",
    "caseExpressions.placeFirstWhenOnNewLine",
    "caseExpressions.whenAlignment",
    "caseExpressions.placeThenOnNewLine",
    "caseExpressions.thenAlignment",
    "caseExpressions.placeElseOnNewLine",
    "caseExpressions.alignElseToWhen",
    "caseExpressions.placeEndOnNewLine",
    "caseExpressions.endAlignment",
    "caseExpressions.collapseShortCaseExpressions",
    "caseExpressions.collapseCaseExpressionsShorterThan",
    "operators.comparison.addSpacesAround",
    "operators.arithmetic.addSpacesAround",
    "operators.andOr.placeOnNewLine",
    "operators.andOr.alignment",
];

/// Values that need no support because QuillSQL already lays code out that way.
const ALREADY_MATCHES: &[(&str, &str)] = &[
    ("whitespace.wrapLongLines", "false"),
    ("functionCalls.placeArgumentsOnNewLines", "never"),
    ("operators.in.placeFirstValueOnNewLine", "never"),
    ("operators.in.placeSubsequentValuesOnNewLines", "never"),
    ("operators.between.placeOnNewLine", "false"),
    ("variables.alignDataTypesAndValues", "false"),
    ("variables.placeAssignedValueOnNewLineIfLongerThanMaxLineLength", "false"),
    ("caseExpressions.placeExpressionOnNewLine", "false"),
    ("ddl.placeFirstProcedureParameterOnNewLine", "never"),
    ("joinStatements.join.indentJoinTable", "false"),
    ("lists.alignItemsAcrossClauses", "false"),
    ("insertStatements.values.parenthesisStyle", "compactSimple"),
    ("cte.indentContents", "true"),
    ("parentheses.indentParenthesesContents", "true"),
    ("ddl.indentParenthesesContents", "true"),
];

/// Options that only matter when another option is set to the given value.
const ONLY_WHEN: &[(&str, &str, &str)] = &[
    ("controlFlow.collapseStatementsShorterThan", "controlFlow.collapseShortStatements", "true"),
    ("parentheses.collapseParenthesesShorterThan", "parentheses.collapseShortParenthesisContents", "true"),
    ("dml.collapseStatementsShorterThan", "dml.collapseShortStatements", "true"),
    ("dml.collapseSubqueriesShorterThan", "dml.collapseShortSubqueries", "true"),
    ("ddl.collapseStatementsShorterThan", "ddl.collapseShortStatements", "true"),
    ("caseExpressions.collapseCaseExpressionsShorterThan", "caseExpressions.collapseShortCaseExpressions", "true"),
    ("operators.between.andAlignment", "operators.between.placeAndKeywordOnNewLine", "true"),
    ("operators.in.alignment", "operators.in.placeSubsequentValuesOnNewLines", "always"),
    ("insertStatements.columns.indentContents", "insertStatements.columns.placeSubsequentColumnsOnNewLines", "always"),
];

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_ascii_lowercase(),
        other => other.to_string(),
    }
}

/// True when a changed option has no effect on QuillSQL's output.
fn has_no_effect(path: &str, merged: &Value) -> bool {
    let at = |p: &str| value_text(p.split('.').fold(merged, |v, k| &v[k]));
    ALREADY_MATCHES
        .iter()
        .any(|(p, v)| p.eq_ignore_ascii_case(path) && at(p) == v.to_ascii_lowercase())
        || ONLY_WHEN
            .iter()
            .any(|(p, other, v)| p.eq_ignore_ascii_case(path) && at(other) != v.to_ascii_lowercase())
}

/// True when the JSON is a sectioned style rather than a flat QuillSQL options file.
pub fn is_sectioned_style(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.keys().any(|k| SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(k))))
}

/// Merges `user` into `base`, matching keys case-insensitively, and records every
/// user value that differs from the default along with its dotted path.
fn merge(base: &mut Map<String, Value>, user: &Map<String, Value>, path: &str, changed: &mut Vec<String>, unknown: &mut Vec<String>) {
    for (key, value) in user {
        let canonical = base.keys().find(|k| k.eq_ignore_ascii_case(key)).cloned();
        let full = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
        let Some(canonical) = canonical else {
            if !(path.is_empty() && key.eq_ignore_ascii_case("metadata")) {
                unknown.push(full);
            }
            continue;
        };
        let full = if path.is_empty() { canonical.clone() } else { format!("{path}.{canonical}") };
        match (base.get_mut(&canonical), value) {
            (Some(Value::Object(b)), Value::Object(u)) => merge(b, u, &full, changed, unknown),
            (Some(slot), v) => {
                let differs = match (&*slot, v) {
                    (Value::String(a), Value::String(b)) => !a.eq_ignore_ascii_case(b),
                    (a, b) => a != b,
                };
                if differs {
                    changed.push(full);
                }
                *slot = v.clone();
            }
            (None, _) => unreachable!("key was found above"),
        }
    }
}

struct Reader<'a> {
    root: &'a Value,
    errors: Vec<String>,
}

impl Reader<'_> {
    fn get(&self, path: &str) -> &Value {
        path.split('.').fold(self.root, |v, k| &v[k])
    }

    fn bool(&mut self, path: &str) -> bool {
        match self.get(path).as_bool() {
            Some(b) => b,
            None => {
                self.errors.push(format!("{path}: expected true or false"));
                false
            }
        }
    }

    fn uint(&mut self, path: &str) -> usize {
        match self.get(path).as_u64() {
            Some(n) => n as usize,
            None => {
                self.errors.push(format!("{path}: expected a non-negative number"));
                0
            }
        }
    }

    /// Returns the lower-cased enum value if it is one of `allowed`.
    fn choice(&mut self, path: &str, allowed: &[&str]) -> String {
        let raw = self.get(path).as_str().unwrap_or_default().to_ascii_lowercase();
        if allowed.iter().any(|a| a.eq_ignore_ascii_case(&raw)) {
            raw
        } else {
            self.errors.push(format!("{path}: unsupported value {:?}", self.get(path)));
            allowed[0].to_ascii_lowercase()
        }
    }

    fn list_break(&mut self, path: &str, multiple: &str) -> ListBreak {
        let allowed = ["never", "always", multiple, "ifLongerThanMaxLineLength"];
        match self.choice(path, &allowed).as_str() {
            "always" => ListBreak::Always,
            "never" | "iflongerthanmaxlinelength" => ListBreak::Never,
            _ => ListBreak::IfMultiple,
        }
    }

    fn paren(&mut self, path: &str) -> ParenStyle {
        let allowed = [
            "compactSimple", "compactToStatement", "compactIndented", "compactRightAligned",
            "expandedSimple", "expandedToStatement", "expandedIndented", "expandedRightAligned",
            "expandedSplit",
        ];
        let value = self.choice(path, &allowed);
        if value.starts_with("compact") {
            ParenStyle::Compact
        } else if value == "expandedindented" {
            ParenStyle::ExpandedIndented
        } else {
            ParenStyle::ExpandedToStatement
        }
    }

    fn case(&mut self, path: &str) -> Option<WordCase> {
        let allowed = ["leaveAsIs", "lowercase", "uppercase", "lowerCamelCase", "upperCamelCase"];
        match self.choice(path, &allowed).as_str() {
            "uppercase" => Some(WordCase::Upper),
            "lowercase" => Some(WordCase::Lower),
            _ => None,
        }
    }

    fn and_or(&mut self, path: &str) -> AndOrAlign {
        let allowed = ["leftAligned", "indented", "rightAligned", "beforeFirstListItem", "toFirstListItem"];
        match self.choice(path, &allowed).as_str() {
            "indented" => AndOrAlign::Indented,
            "rightaligned" => AndOrAlign::Right,
            "beforefirstlistitem" => AndOrAlign::BeforeFirstItem,
            "tofirstlistitem" => AndOrAlign::ToFirstItem,
            _ => AndOrAlign::Left,
        }
    }

    fn collapse(&mut self, flag: &str, width: &str) -> Option<usize> {
        let on = self.bool(flag);
        let width = self.uint(width);
        on.then_some(width)
    }
}

/// Values the formatter only approximates; reported so the difference is not a surprise.
fn approximated(merged: &Value) -> Vec<String> {
    let mut notes = Vec::new();
    let s = |path: &str| path.split('.').fold(merged, |v, k| &v[k]).as_str().unwrap_or_default().to_ascii_lowercase();
    for path in [
        "lists.placeSubsequentItemsOnNewLines",
        "operators.andOr.placeOnNewLine",
        "insertStatements.columns.placeSubsequentColumnsOnNewLines",
    ] {
        if s(path) == "iflongerthanmaxlinelength" {
            notes.push(format!("{path}: ifLongerThanMaxLineLength is treated as always"));
        }
    }
    for path in ["parentheses.parenthesisStyle", "ddl.parenthesisStyle", "cte.parenthesisStyle", "insertStatements.columns.parenthesisStyle"] {
        let v = s(path);
        if !matches!(v.as_str(), "compactsimple" | "expandedtostatement" | "expandedindented") {
            notes.push(format!("{path}: {v} is approximated"));
        }
    }
    for path in ["casing.reservedKeywords", "casing.builtInFunctions", "casing.builtInDataTypes"] {
        if s(path).ends_with("camelcase") {
            notes.push(format!("{path}: camel case is not supported; casing is left as is"));
        }
    }
    notes
}

pub struct LoadedStyle {
    pub options: Options,
    /// Options the file sets that this formatter ignores or approximates.
    pub warnings: Vec<String>,
}

pub fn load(user: &Value) -> Result<LoadedStyle, String> {
    let mut merged: Value = serde_json::from_str(DEFAULTS).expect("bundled defaults are valid JSON");
    let user_map = user.as_object().ok_or("a sectioned style must be a JSON object")?;
    let (mut changed, mut unknown) = (Vec::new(), Vec::new());
    merge(merged.as_object_mut().expect("defaults are an object"), user_map, "", &mut changed, &mut unknown);

    let mut r = Reader { root: &merged, errors: Vec::new() };
    let and_or = r.and_or("operators.andOr.alignment");
    let layout = Layout {
        indent: r.uint("whitespace.numberOfSpacesInTabs"),
        list_first_item: r.list_break("lists.placeFirstItemOnNewLine", "ifSubsequentItems"),
        list_items_on_new_lines: r.choice("lists.placeSubsequentItemsOnNewLines", &["always", "never", "ifLongerThanMaxLineLength"]) != "never",
        align_items_with_first: r.bool("lists.alignSubsequentItemsWithFirstItem"),
        break_group_order_lists: true,
        leading_commas: r.bool("lists.placeCommasBeforeItems"),
        space_before_comma: r.bool("lists.addSpaceBeforeComma"),
        space_after_comma: r.bool("lists.addSpaceAfterComma"),
        newline_after_distinct_top: r.bool("dml.addNewLineAfterDistinctAndTopClauses"),
        right_align_clauses: r.choice("dml.clauses.clauseAlignment", &["leftAligned", "rightAligned", "toFirstListItem"]) == "rightaligned",
        from_table: r.list_break("dml.listItems.placeFromTableOnNewLine", "ifMultiple"),
        where_condition: r.list_break("dml.listItems.placeWhereConditionOnNewLine", "ifMultiple"),
        group_order_items: r.list_break("dml.listItems.placeGroupByAndOrderByOnNewLine", "ifMultiple"),
        join_on_new_line: r.bool("joinStatements.join.placeOnNewLine"),
        indent_join: r.choice("joinStatements.join.keywordAlignment", &["toFrom", "indented", "toTable", "rightAlignedToFrom"]) != "tofrom",
        on_on_new_line: r.bool("joinStatements.on.placeOnNewLine"),
        indent_on: r.choice("joinStatements.on.keywordAlignment", &["toJoin", "indented", "rightAlignedToJoin", "rightAlignedToInner", "toTable"]) != "tojoin",
        on_condition_on_new_line: r.bool("joinStatements.on.placeConditionOnNewLine"),
        and_or_on_new_line: r.choice("operators.andOr.placeOnNewLine", &["always", "never", "ifLongerThanMaxLineLength"]) != "never",
        and_or_align: and_or,
        on_and_or_align: and_or,
        case_first_when: r.list_break("caseExpressions.placeFirstWhenOnNewLine", "ifInputExpression"),
        case_when_indented: r.choice("caseExpressions.whenAlignment", &["indentedFromCase", "toCase", "toFirstItem"]) != "tocase",
        case_then_on_new_line: r.bool("caseExpressions.placeThenOnNewLine"),
        case_then_indented: r.choice("caseExpressions.thenAlignment", &["indentedFromWhen", "toWhen", "toWhenExpression"]) != "towhen",
        case_else_on_new_line: r.bool("caseExpressions.placeElseOnNewLine"),
        case_else_to_when: r.bool("caseExpressions.alignElseToWhen"),
        case_end_on_new_line: r.bool("caseExpressions.placeEndOnNewLine"),
        case_end_to_when: r.choice("caseExpressions.endAlignment", &["toCase", "toWhen", "rightAlignedToWhen"]) != "tocase",
        collapse_case: r.collapse("caseExpressions.collapseShortCaseExpressions", "caseExpressions.collapseCaseExpressionsShorterThan"),
        subquery_parens: r.paren("parentheses.parenthesisStyle"),
        collapse_subqueries: r.collapse("dml.collapseShortSubqueries", "dml.collapseSubqueriesShorterThan"),
        collapse_statements: r.collapse("dml.collapseShortStatements", "dml.collapseStatementsShorterThan"),
        ddl_parens: r.paren("ddl.parenthesisStyle"),
        collapse_ddl: r.collapse("ddl.collapseShortStatements", "ddl.collapseStatementsShorterThan"),
        insert_columns: {
            let style = r.paren("insertStatements.columns.parenthesisStyle");
            let each = r.choice("insertStatements.columns.placeSubsequentColumnsOnNewLines", &["always", "never", "ifLongerThanMaxLineLength"]);
            (each != "never").then_some(style)
        },
        cte_parens: r.paren("cte.parenthesisStyle"),
        cte_name_on_new_line: r.bool("cte.placeNameOnNewLine"),
        cte_as_on_new_line: r.bool("cte.placeAsOnNewLine"),
        preserve_blank_lines: r.bool("whitespace.newLines.preserveExistingEmptyLinesBetweenStatements"),
        blank_lines_between_statements: r.uint("whitespace.newLines.emptyLinesBetweenStatements"),
        preserve_blank_lines_after_go: r.bool("whitespace.newLines.preserveExistingEmptyLinesAfterBatchSeparator"),
        blank_lines_after_go: r.uint("whitespace.newLines.emptyLinesAfterBatchSeparator"),
        semicolon_space: match r.choice("whitespace.whiteSpaceBeforeSemiColon", &["none", "spaceBefore", "newLineBefore"]).as_str() {
            "spacebefore" => SemicolonSpace::SpaceBefore,
            "newlinebefore" => SemicolonSpace::NewLineBefore,
            _ => SemicolonSpace::None,
        },
        spaces_around_comparison: r.bool("operators.comparison.addSpacesAround"),
        spaces_around_arithmetic: r.bool("operators.arithmetic.addSpacesAround"),
        indent_begin_end: r.bool("controlFlow.indentBeginAndEndKeywords"),
    };
    let keyword_case = r.case("casing.reservedKeywords");
    let function_case = r.case("casing.builtInFunctions");
    let data_type_case = r.case("casing.builtInDataTypes");
    if !r.errors.is_empty() {
        return Err(r.errors.join("; "));
    }

    let mut warnings: Vec<String> = changed
        .into_iter()
        .filter(|p| !SUPPORTED.iter().any(|s| s.eq_ignore_ascii_case(p)) && !has_no_effect(p, &merged))
        .map(|p| format!("{p}: not supported, ignored"))
        .collect();
    warnings.extend(unknown.into_iter().map(|p| format!("{p}: unknown option, ignored")));
    warnings.extend(approximated(&merged));
    warnings.sort();
    warnings.dedup();

    let options = Options {
        indent_size: layout.indent,
        leading_commas: layout.leading_commas,
        layout_style: Some(layout),
        keyword_case,
        function_case,
        data_type_case,
        ..Options::default()
    };
    Ok(LoadedStyle { options, warnings })
}

/// Options from a style file's JSON: a flat QuillSQL options object or a sectioned style.
/// Returns the warnings a sectioned style produces for settings that are not applied.
pub fn options_from_value(value: &Value) -> Result<(Options, Vec<String>), String> {
    if !is_sectioned_style(value) {
        return serde_json::from_value(value.clone()).map(|o| (o, Vec::new())).map_err(|e| e.to_string());
    }
    let loaded = load(value)?;
    Ok((loaded.options, loaded.warnings))
}
