use quillsql::plan::{batches, summarize, write_reason};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn estimated_plan_summary() {
    let s = summarize(&[fixture("plan-estimated.xml")], 100);
    println!("{s}");
    assert!(s.starts_with("Statement 1: SELECT TOP (5) c.Name"), "{s}");
    assert!(s.contains("Orders"), "{s}");
    assert!(s.contains("est rows"), "{s}");
    assert!(s.contains("cost ") && s.contains('%'), "{s}");
    assert!(!s.contains("actual rows"), "{s}");
    // CONVERT on the column makes SQL Server warn about the conversion.
    assert!(s.contains("WARNING") || s.contains("CONVERT"), "{s}");
}

#[test]
fn actual_plan_has_runtime_counts() {
    let s = summarize(&[fixture("plan-actual.xml")], 100);
    println!("{s}");
    assert!(s.contains("actual rows"), "{s}");
}

#[test]
fn summaries_are_bounded_and_tolerate_bad_xml() {
    let s = summarize(&[fixture("plan-estimated.xml")], 3);
    assert_eq!(s.lines().count(), 4, "{s}");
    assert!(s.ends_with("(plan truncated)"));
    assert!(summarize(&["<not xml".into()], 10).starts_with("(unreadable plan"));
    assert_eq!(summarize(&[], 10), "(no plan)");
}

#[test]
fn only_reading_scripts_may_run() {
    assert_eq!(write_reason("declare @n int = 5; set nocount on; select * from t where id = @n"), None);
    assert_eq!(write_reason("with x as (select 1 a) select * from x -- delete later"), None);
    assert_eq!(write_reason("select 'insert' as s"), None);
    assert!(write_reason("select * into #t from t").unwrap().contains("INTO"));
    assert!(write_reason("update t set a = 1").unwrap().contains("UPDATE"));
    assert!(write_reason("exec dbo.p").is_some());
    assert!(write_reason("set identity_insert t on").is_some());
    assert!(write_reason("select next value for dbo.seq").is_some());
}

#[test]
fn splits_batches_at_go() {
    assert_eq!(batches("select 1\nGO\n\nselect 2 -- go\ngo 5\n"), vec!["select 1", "\n\nselect 2 -- go"]);
}
