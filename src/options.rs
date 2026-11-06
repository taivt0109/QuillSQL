use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, clap::ValueEnum)]
pub enum Enclose {
    WithBrackets,
    WithoutBrackets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, clap::ValueEnum)]
pub enum TableAlias {
    AddAs,
    RemoveAs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordCase {
    Upper,
    Lower,
}

impl WordCase {
    pub fn apply(self, text: &str) -> String {
        match self {
            WordCase::Upper => text.to_ascii_uppercase(),
            WordCase::Lower => text.to_ascii_lowercase(),
        }
    }
}

/// When to start a list's first item (or a clause's content) on its own line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListBreak {
    Never,
    Always,
    /// Only when more than one item (or condition) follows.
    IfMultiple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndOrAlign {
    /// Under the clause keyword (`WHERE`, `ON`).
    Left,
    /// One indent in from the clause keyword.
    Indented,
    /// Right-aligned with the end of the clause keyword.
    Right,
    /// So the condition after the keyword lines up with the first condition.
    BeforeFirstItem,
    /// The keyword itself lines up with the first condition.
    ToFirstItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParenStyle {
    /// Contents start right after `(` and `)` follows the last item.
    Compact,
    /// Contents indented on their own lines, `)` back at the statement's column.
    ExpandedToStatement,
    /// Contents indented on their own lines, `)` at the contents' column.
    ExpandedIndented,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemicolonSpace {
    None,
    SpaceBefore,
    NewLineBefore,
}

/// Layout rules. `Layout::native` is QuillSQL's own style; `style::load` builds one
/// from a sectioned style file.
#[derive(Clone, Debug)]
pub struct Layout {
    pub indent: usize,
    pub list_first_item: ListBreak,
    pub list_items_on_new_lines: bool,
    pub align_items_with_first: bool,
    pub break_group_order_lists: bool,
    pub leading_commas: bool,
    pub space_before_comma: bool,
    pub space_after_comma: bool,
    pub newline_after_distinct_top: bool,
    pub right_align_clauses: bool,
    pub from_table: ListBreak,
    pub where_condition: ListBreak,
    pub group_order_items: ListBreak,
    pub join_on_new_line: bool,
    pub indent_join: bool,
    pub on_on_new_line: bool,
    pub indent_on: bool,
    pub on_condition_on_new_line: bool,
    pub and_or_on_new_line: bool,
    pub and_or_align: AndOrAlign,
    pub on_and_or_align: AndOrAlign,
    pub case_first_when: ListBreak,
    pub case_when_indented: bool,
    pub case_then_on_new_line: bool,
    pub case_then_indented: bool,
    pub case_else_on_new_line: bool,
    pub case_else_to_when: bool,
    pub case_end_on_new_line: bool,
    pub case_end_to_when: bool,
    pub collapse_case: Option<usize>,
    pub subquery_parens: ParenStyle,
    pub collapse_subqueries: Option<usize>,
    pub collapse_statements: Option<usize>,
    pub ddl_parens: ParenStyle,
    pub collapse_ddl: Option<usize>,
    /// `None` keeps an INSERT column list on one line.
    pub insert_columns: Option<ParenStyle>,
    pub cte_parens: ParenStyle,
    pub cte_name_on_new_line: bool,
    pub cte_as_on_new_line: bool,
    pub preserve_blank_lines: bool,
    pub blank_lines_between_statements: usize,
    pub preserve_blank_lines_after_go: bool,
    pub blank_lines_after_go: usize,
    pub semicolon_space: SemicolonSpace,
    pub spaces_around_comparison: bool,
    pub spaces_around_arithmetic: bool,
    pub indent_begin_end: bool,
}

impl Layout {
    pub fn native(opts: &Options) -> Self {
        Layout {
            indent: opts.indent_size,
            list_first_item: ListBreak::Never,
            list_items_on_new_lines: true,
            align_items_with_first: true,
            break_group_order_lists: false,
            leading_commas: opts.leading_commas,
            space_before_comma: false,
            space_after_comma: true,
            newline_after_distinct_top: false,
            right_align_clauses: false,
            from_table: ListBreak::Never,
            where_condition: ListBreak::Never,
            group_order_items: ListBreak::Never,
            join_on_new_line: true,
            indent_join: false,
            on_on_new_line: true,
            indent_on: true,
            on_condition_on_new_line: false,
            and_or_on_new_line: true,
            and_or_align: AndOrAlign::Indented,
            on_and_or_align: AndOrAlign::Left,
            case_first_when: ListBreak::Always,
            case_when_indented: true,
            case_then_on_new_line: false,
            case_then_indented: true,
            case_else_on_new_line: true,
            case_else_to_when: true,
            case_end_on_new_line: true,
            case_end_to_when: false,
            collapse_case: None,
            subquery_parens: ParenStyle::ExpandedToStatement,
            collapse_subqueries: None,
            collapse_statements: None,
            ddl_parens: ParenStyle::ExpandedToStatement,
            collapse_ddl: None,
            insert_columns: None,
            cte_parens: ParenStyle::ExpandedToStatement,
            cte_name_on_new_line: false,
            cte_as_on_new_line: false,
            preserve_blank_lines: true,
            blank_lines_between_statements: 1,
            preserve_blank_lines_after_go: true,
            blank_lines_after_go: 1,
            semicolon_space: SemicolonSpace::None,
            spaces_around_comparison: true,
            spaces_around_arithmetic: true,
            indent_begin_end: false,
        }
    }
}

/// Formatting options. A QuillSQL `--style` JSON file uses these field names in camelCase.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub layout: bool,
    pub indent_size: usize,
    pub leading_commas: bool,
    pub apply_casing: bool,
    pub prefix_unicode: bool,
    pub insert_semicolons: bool,
    pub enclose_identifiers: Option<Enclose>,
    pub table_alias_style: Option<TableAlias>,
    /// Set by a sectioned style file; overrides the layout fields above.
    #[serde(skip)]
    pub layout_style: Option<Layout>,
    #[serde(skip)]
    pub keyword_case: Option<WordCase>,
    #[serde(skip)]
    pub function_case: Option<WordCase>,
    #[serde(skip)]
    pub data_type_case: Option<WordCase>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            layout: true,
            indent_size: 4,
            leading_commas: false,
            apply_casing: false,
            prefix_unicode: false,
            insert_semicolons: false,
            enclose_identifiers: None,
            table_alias_style: None,
            layout_style: None,
            keyword_case: None,
            function_case: None,
            data_type_case: None,
        }
    }
}

impl Options {
    pub fn effective_layout(&self) -> Layout {
        self.layout_style.clone().unwrap_or_else(|| Layout::native(self))
    }

    pub fn effective_keyword_case(&self) -> Option<WordCase> {
        self.keyword_case.or(self.apply_casing.then_some(WordCase::Upper))
    }
}
