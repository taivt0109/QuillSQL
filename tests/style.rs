use serde_json::{Value, json};
use quillsql::{format, style};

fn with_style(src: &str, style_json: Value) -> String {
    let loaded = style::load(&style_json).unwrap();
    format(src, &loaded.options).unwrap()
}

fn warnings(style_json: Value) -> Vec<String> {
    style::load(&style_json).unwrap().warnings
}

const QUERY: &str = "select a, b from t inner join u on u.id = t.id and u.x = 1 where t.c = 1 and t.d = 2 order by a, b";

#[test]
fn recognises_sectioned_styles_but_not_quillsql_options() {
    assert!(style::is_sectioned_style(&json!({"lists": {}})));
    assert!(style::is_sectioned_style(&json!({"Metadata": {"name": "x"}})));
    assert!(!style::is_sectioned_style(&json!({"indentSize": 2, "applyCasing": true})));
}

#[test]
fn an_empty_style_uses_the_defaults() {
    assert_eq!(
        with_style(QUERY, json!({"metadata": {"name": "empty"}})),
        "select a,\n       b\nfrom t\ninner join u\non u.id = t.id\nand u.x = 1\nwhere t.c = 1\nand t.d = 2\norder by a,\n         b"
    );
}

#[test]
fn first_item_on_new_line_and_conditions_aligned_to_first() {
    let out = with_style(
        QUERY,
        json!({
            "lists": {"placeFirstItemOnNewLine": "always", "placeSubsequentItemsOnNewLines": "never"},
            "dml": {"listItems": {"placeWhereConditionOnNewLine": "always"}},
            "joinStatements": {"on": {"placeOnNewLine": false}},
            "operators": {"andOr": {"alignment": "toFirstListItem"}},
            "casing": {"reservedKeywords": "uppercase"}
        }),
    );
    assert_eq!(
        out,
        "SELECT\n    a, b\nFROM t\nINNER JOIN u ON u.id = t.id\n                AND u.x = 1\nWHERE\n    t.c = 1\n    AND t.d = 2\nORDER BY a, b"
    );
}

#[test]
fn right_aligned_clauses_and_leading_commas() {
    let out = with_style(
        "select a, b from t where c = 1 and d = 2",
        json!({
            "dml": {"clauses": {"clauseAlignment": "rightAligned"}},
            "lists": {"placeCommasBeforeItems": true},
            "operators": {"andOr": {"alignment": "rightAligned"}}
        }),
    );
    assert_eq!(out, "select a\n     , b\n  from t\n where c = 1\n   and d = 2");
}

#[test]
fn collapses_short_statements_and_subqueries() {
    let collapsed = json!({
        "dml": {"collapseShortStatements": true, "collapseStatementsShorterThan": 40,
                "collapseShortSubqueries": true, "collapseSubqueriesShorterThan": 30}
    });
    assert_eq!(with_style("select a, b from t where c = 1", collapsed.clone()), "select a, b from t where c = 1");
    let long = with_style("select a, b from t where c in (select c from u) and d = 1 and e = 2", collapsed);
    assert!(long.contains("(select c from u)"), "{long}");
    assert!(long.starts_with("select a,\n"), "{long}");
}

#[test]
fn casing_respects_where_names_can_be_types() {
    let out = with_style(
        "declare @d date = getdate(); select date, cast(x as varchar(10)), count(*) from t",
        json!({"casing": {"reservedKeywords": "uppercase", "builtInFunctions": "uppercase", "builtInDataTypes": "lowercase"}}),
    );
    // `date` the column keeps its case; `date` the type follows the data type rule.
    assert!(out.contains("DECLARE @d date = GETDATE();"), "{out}");
    assert!(out.contains("SELECT date,"), "{out}");
    assert!(out.contains("CAST(x AS varchar(10))"), "{out}");
    assert!(out.contains("COUNT(*)"), "{out}");
}

#[test]
fn blank_lines_between_statements_follow_the_style() {
    let src = "select 1\n\n\n\nselect 2\nselect 3";
    let none = json!({"whitespace": {"newLines": {"preserveExistingEmptyLinesBetweenStatements": false, "emptyLinesBetweenStatements": 0}}});
    let one = json!({"whitespace": {"newLines": {"preserveExistingEmptyLinesBetweenStatements": false, "emptyLinesBetweenStatements": 1}}});
    assert_eq!(with_style(src, none), "select 1\nselect 2\nselect 3");
    assert_eq!(with_style(src, one), "select 1\n\nselect 2\n\nselect 3");
}

#[test]
fn assignment_keeps_its_spaces_when_comparison_spacing_is_off() {
    let out = with_style(
        "declare @a int = 1; update t set b = 2 where c = 3",
        json!({"operators": {"comparison": {"addSpacesAround": false}}}),
    );
    assert!(out.contains("@a int = 1"), "{out}");
    assert!(out.contains("b = 2"), "{out}");
    assert!(out.contains("c=3"), "{out}");
}

#[test]
fn keys_and_values_are_case_insensitive() {
    let out = with_style("select a from t", json!({"Casing": {"ReservedKeywords": "UPPERCASE"}}));
    assert_eq!(out, "SELECT a\nFROM t");
}

#[test]
fn reports_unsupported_and_unknown_options_only_when_they_matter() {
    let w = warnings(json!({
        "variables": {"placeEqualsSignOnNewLine": true},
        "whitespace": {"wrapLongLines": false},
        "controlFlow": {"collapseStatementsShorterThan": 70},
        "lists": {"noSuchOption": 1}
    }));
    assert_eq!(
        w,
        vec![
            "lists.noSuchOption: unknown option, ignored".to_string(),
            "variables.placeEqualsSignOnNewLine: not supported, ignored".to_string(),
        ]
    );
}

#[test]
fn rejects_invalid_values() {
    assert!(style::load(&json!({"lists": {"placeFirstItemOnNewLine": "sometimes"}})).is_err());
    assert!(style::load(&json!({"whitespace": {"numberOfSpacesInTabs": "four"}})).is_err());
}

/// Every supported option, flipped one at a time, must keep the formatter's token check passing.
#[test]
fn every_option_value_keeps_scripts_intact() {
    let corpus = [
        QUERY,
        "select distinct top (5) with ties a, case x when 1 then 'a' else 'b' end k from (select a, x from t) s order by a",
        "with c as (select 1 x), d (y) as (select 2) select * from c cross join d;",
        "insert into t (a, b) values (1, 2); update t set a = a + 1 where b between 1 and 2;",
        "create table t (id int primary key, n varchar(5)); if 1 = 1 begin select 1; end else select 2;",
        "-- c\nselect a -- d\nfrom t\nGO\nselect 1",
    ];
    let variants: Vec<Value> = vec![
        json!({"lists": {"placeFirstItemOnNewLine": "always"}}),
        json!({"lists": {"placeFirstItemOnNewLine": "ifSubsequentItems"}}),
        json!({"lists": {"placeSubsequentItemsOnNewLines": "never"}}),
        json!({"lists": {"alignSubsequentItemsWithFirstItem": false}}),
        json!({"lists": {"placeCommasBeforeItems": true}}),
        json!({"lists": {"addSpaceAfterComma": false, "addSpaceBeforeComma": true}}),
        json!({"parentheses": {"parenthesisStyle": "compactSimple"}}),
        json!({"parentheses": {"parenthesisStyle": "expandedIndented"}}),
        json!({"dml": {"clauses": {"clauseAlignment": "rightAligned"}}}),
        json!({"dml": {"addNewLineAfterDistinctAndTopClauses": true}}),
        json!({"dml": {"collapseShortStatements": true, "collapseStatementsShorterThan": 60}}),
        json!({"dml": {"collapseShortSubqueries": true}}),
        json!({"dml": {"listItems": {"placeFromTableOnNewLine": "always", "placeWhereConditionOnNewLine": "ifMultiple", "placeGroupByAndOrderByOnNewLine": "always"}}}),
        json!({"ddl": {"parenthesisStyle": "expandedIndented"}}),
        json!({"ddl": {"collapseShortStatements": true}}),
        json!({"controlFlow": {"indentBeginAndEndKeywords": true}}),
        json!({"cte": {"placeNameOnNewLine": true, "placeAsOnNewLine": false, "parenthesisStyle": "expandedIndented"}}),
        json!({"joinStatements": {"join": {"keywordAlignment": "indented", "placeOnNewLine": false}, "on": {"placeOnNewLine": false, "placeConditionOnNewLine": true}}}),
        json!({"insertStatements": {"columns": {"placeSubsequentColumnsOnNewLines": "never"}}}),
        json!({"insertStatements": {"columns": {"parenthesisStyle": "compactSimple"}}}),
        json!({"caseExpressions": {"placeFirstWhenOnNewLine": "never", "whenAlignment": "toCase", "placeThenOnNewLine": true, "endAlignment": "toWhen"}}),
        json!({"caseExpressions": {"collapseShortCaseExpressions": true}}),
        json!({"operators": {"andOr": {"alignment": "beforeFirstListItem"}}}),
        json!({"operators": {"andOr": {"placeOnNewLine": "never"}}}),
        json!({"operators": {"comparison": {"addSpacesAround": false}, "arithmetic": {"addSpacesAround": false}}}),
        json!({"whitespace": {"numberOfSpacesInTabs": 2, "whiteSpaceBeforeSemiColon": "newLineBefore"}}),
        json!({"whitespace": {"whiteSpaceBeforeSemiColon": "spaceBefore", "newLines": {"preserveExistingEmptyLinesBetweenStatements": false, "emptyLinesBetweenStatements": 2}}}),
        json!({"casing": {"reservedKeywords": "lowercase", "builtInFunctions": "uppercase", "builtInDataTypes": "uppercase"}}),
    ];
    for variant in &variants {
        let loaded = style::load(variant).unwrap();
        for src in corpus {
            let once = format(src, &loaded.options).unwrap_or_else(|e| panic!("{variant}\n{src}\n{e}"));
            let twice = format(&once, &loaded.options).unwrap();
            assert_eq!(once, twice, "not idempotent with {variant}:\n{src}");
        }
    }
}
