use super::*;
use crate::task::{
    TaskSource, durable_task_identity, task_content_for_edit, task_display_text,
    task_entry_from_text, task_full_display_text,
};

#[test]
fn thinking_accepts_every_level_anywhere_in_multiline_task_content() {
    for level in &LEVELS[1..] {
        for content in [
            format!("/think {level} Fix the parser."),
            format!("Fix /think {level} the parser."),
            format!("Fix the parser. /think {level}"),
            format!("Fix the parser.\n\nPreserve Unicode.\n/think {level}\nMore details."),
            format!("Fix the parser.\n\n/think\t{level}\ncodex:session clt:stopped"),
        ] {
            assert_eq!(read(&content), Some(*level), "{content}");
            assert!(!strip(&content).contains("/think"));
            assert_eq!(read(&strip(&content)), None);
        }
    }
    assert_eq!(read("Task /think HIGH"), Some("high"));
    assert_eq!(read("/think low Task /think max"), Some("max"));
    assert_eq!(read("Task /think high /think default"), None);
    assert_eq!(strip("Task /think high /think default"), "Task");
}

#[test]
fn thinking_only_consumes_complete_directives_and_preserves_other_text() {
    for content in [
        "Task /thinking high",
        "Task /think",
        "Task /think unknown",
        "Task /thinkhigh",
        "Document `/think high` examples",
        "Task <!-- clt-coordination: {\"intent\":\"use /think high later\"} -->",
    ] {
        assert_eq!(read(content), None, "{content}");
        assert_eq!(strip(content), content);
    }
    assert_eq!(
        strip("修复 /think max café.\n\nKeep details."),
        "修复 café.\n\nKeep details."
    );
    assert_eq!(strip("/think high /goal Ship it."), "/goal Ship it.");
    assert_eq!(strip("/goal Ship /think high it."), "/goal Ship it.");
}

#[test]
fn thinking_labels_use_full_content_and_do_not_leak_into_the_editor() {
    let content = "Fix parser.\n\nLong details. /think max codex:session clt:stopped";
    let task = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        content,
        content,
        false,
    );
    assert_eq!(task_display_text(&task), "[think: max] Fix parser.");
    assert_eq!(
        task_full_display_text(&task),
        "[think: max] Fix parser. Long details."
    );
    assert_eq!(
        task_content_for_edit(content),
        "Fix parser.\n\nLong details. /think max"
    );
    let plain = "Fix parser.\n\nLong details.";
    assert_eq!(
        durable_task_identity(plain),
        durable_task_identity("Fix parser.\n\nLong /think high details.")
    );
    assert_eq!(
        durable_task_identity(plain),
        durable_task_identity("Fix parser.\n\nLong details.\n/think max")
    );
}

#[test]
fn thinking_cycle_replaces_all_directives_and_restores_default() {
    let body = "/goal Fix parser.\n\nKeep all details.";
    let mut content = body.to_string();
    for level in LEVELS[1..].iter().copied().map(Some).chain([None]) {
        let (updated, selected) = cycle(&content);
        assert_eq!(selected, level);
        assert_eq!(read(&updated), level);
        assert_eq!(strip(&updated), body);
        assert!(updated.starts_with("/goal"));
        content = updated;
    }
    assert_eq!(content, body);
    let (updated, level) = cycle("/think low Task /think high");
    assert_eq!(level, Some("xhigh"));
    assert_eq!(updated, "Task /think xhigh");
}
