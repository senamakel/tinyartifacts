//! The human-edit diff: churn, leftover lines, and the fallback for huge bodies.

// Unwrapping is fine in tests (AGENTS.md); the workspace lints keep it out of
// library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiny_artifacts::{ArtifactAuthor, ArtifactDiff, ArtifactVersion, DiffOp, MAX_DIFF_LINES};

fn version(n: u32, body: &str) -> ArtifactVersion {
    ArtifactVersion::new(n, body, ArtifactAuthor::Agent, "agent", 0)
}

fn ops(diff: &ArtifactDiff) -> Vec<(DiffOp, String)> {
    diff.lines.iter().map(|l| (l.op, l.text.clone())).collect()
}

#[test]
fn writing_into_an_empty_version_is_total_churn_and_no_change_is_none() {
    let added = ArtifactDiff::between(&version(1, ""), &version(2, "a\nb"));
    assert_eq!(added.added, 2);
    assert!((added.churn - 1.0).abs() < f64::EPSILON);

    let same = ArtifactDiff::between(&version(1, ""), &version(2, ""));
    assert!(same.churn.abs() < f64::EPSILON);
    assert_eq!(same.lines.len(), 0);
}

#[test]
fn trailing_removals_and_additions_are_kept_in_order() {
    let cut = ArtifactDiff::between(&version(1, "a\nb\nc"), &version(2, "a"));
    assert_eq!(
        ops(&cut),
        [
            (DiffOp::Equal, "a".into()),
            (DiffOp::Delete, "b".into()),
            (DiffOp::Delete, "c".into()),
        ]
    );
    assert_eq!(cut.removed, 2);

    let grown = ArtifactDiff::between(&version(1, "a"), &version(2, "a\nb\nc"));
    assert_eq!(
        ops(&grown),
        [
            (DiffOp::Equal, "a".into()),
            (DiffOp::Insert, "b".into()),
            (DiffOp::Insert, "c".into()),
        ]
    );
}

#[test]
fn a_huge_body_degrades_to_a_whole_replace() {
    let big = (0..=MAX_DIFF_LINES)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let diff = ArtifactDiff::between(&version(1, "old"), &version(2, &big));
    assert_eq!(diff.removed, 1);
    assert_eq!(diff.added, MAX_DIFF_LINES + 1);
    assert_eq!(diff.lines[0].op, DiffOp::Delete);
    assert!(diff.lines[1..].iter().all(|l| l.op == DiffOp::Insert));
}
