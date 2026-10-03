use super::*;

#[test]
fn category_cycles_and_metadata_query_is_bounded() {
    let mut category = Category::All;
    for expected in [
        Category::Lifecycle,
        Category::Output,
        Category::Tool,
        Category::Compaction,
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

#[test]
fn compaction_category_matches_only_compaction_lifecycle_evidence() {
    use crate::observation::EvidenceKind;
    use crate::protocol::ToolCategory;

    assert!(
        Category::Compaction.accepts(EvidenceKind::ToolCompleted, Some(ToolCategory::Compaction))
    );
    assert!(Category::Compaction.accepts(EvidenceKind::ToolStarted, Some(ToolCategory::Compaction)));
    assert!(Category::Compaction.accepts(
        EvidenceKind::ExecutionUnknown,
        Some(ToolCategory::Compaction)
    ));
    assert!(!Category::Compaction.accepts(EvidenceKind::ToolCompleted, Some(ToolCategory::Shell)));
    assert!(
        !Category::Compaction.accepts(EvidenceKind::ExecutionUnknown, Some(ToolCategory::Shell))
    );
    assert!(
        !Category::Compaction.accepts(EvidenceKind::TurnCompleted, Some(ToolCategory::Compaction))
    );
}
