use super::*;

#[test]
fn category_cycles_and_metadata_query_is_bounded() {
    let mut category = Category::All;
    for expected in [
        Category::Lifecycle,
        Category::Output,
        Category::Tool,
        Category::Request,
        Category::Waiting,
        Category::All,
    ] {
        category = category.next();
        assert_eq!(category, expected);
    }
    assert!(Query {
        text: "x".repeat(FIELD_BYTES + 1),
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(Query {
        thread: "root".into(),
        ..Default::default()
    }
    .validate()
    .is_ok());
}
