use std::collections::BTreeMap;

use agentforge_core::{
    model::Target,
    planning::{
        ChangeKind, DesiredArtifact, Manifest, ManifestArtifact, PlanningError, build_plan,
        content_hash,
    },
};

fn desired(path: &str, content: &[u8]) -> DesiredArtifact {
    DesiredArtifact {
        path: path.into(),
        target: Target::Codex,
        renderer_version: "codex@1".into(),
        content: content.into(),
    }
}

fn manifest(path: &str, content: &[u8]) -> Manifest {
    Manifest::new(
        "agentforge 0.1.0",
        "sha256:spec",
        "sha256:lock",
        vec![ManifestArtifact {
            path: path.into(),
            target: Target::Codex,
            renderer_version: "codex@1".into(),
            sha256: content_hash(content),
        }],
    )
    .unwrap()
}

fn plan(
    desired_items: Vec<DesiredArtifact>,
    current: BTreeMap<String, Vec<u8>>,
    previous: Option<&Manifest>,
) -> agentforge_core::planning::ResolvedPlan {
    build_plan(
        desired_items,
        &current,
        previous,
        "agentforge 0.1.0",
        "sha256:spec",
        "sha256:lock",
    )
    .unwrap()
}

#[test]
fn fresh_path_is_created_and_unmanaged_path_conflicts_even_if_identical() {
    let fresh = plan(
        vec![desired("AGENTS.md", b"content")],
        BTreeMap::new(),
        None,
    );
    assert_eq!(fresh.changes[0].kind, ChangeKind::Create);
    let current = BTreeMap::from([("AGENTS.md".into(), b"content".to_vec())]);
    let unmanaged = plan(vec![desired("AGENTS.md", b"content")], current, None);
    assert_eq!(unmanaged.changes[0].kind, ChangeKind::Conflict);
    assert!(unmanaged.has_conflicts());
}

#[test]
fn owned_artifact_transitions_are_deterministic() {
    let previous = manifest("AGENTS.md", b"old");
    let old = BTreeMap::from([("AGENTS.md".into(), b"old".to_vec())]);
    assert_eq!(
        plan(
            vec![desired("AGENTS.md", b"old")],
            old.clone(),
            Some(&previous)
        )
        .changes[0]
            .kind,
        ChangeKind::Unchanged
    );
    assert_eq!(
        plan(
            vec![desired("AGENTS.md", b"new")],
            old.clone(),
            Some(&previous)
        )
        .changes[0]
            .kind,
        ChangeKind::Modify
    );
    assert_eq!(
        plan(vec![], old, Some(&previous)).changes[0].kind,
        ChangeKind::Delete
    );
}

#[test]
fn drift_blocks_modify_and_delete() {
    let previous = manifest("AGENTS.md", b"old");
    let drift = BTreeMap::from([("AGENTS.md".into(), b"user edit".to_vec())]);
    for desired_items in [vec![desired("AGENTS.md", b"new")], vec![]] {
        let result = plan(desired_items, drift.clone(), Some(&previous));
        assert_eq!(result.changes[0].kind, ChangeKind::Conflict);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|item| item.code.as_str() == "AF1302")
        );
    }
}

#[test]
fn missing_managed_artifact_is_rebuilt_and_safe_removal_cleans_manifest() {
    let previous = manifest("AGENTS.md", b"old");
    let rebuild = plan(
        vec![desired("AGENTS.md", b"old")],
        BTreeMap::new(),
        Some(&previous),
    );
    assert_eq!(rebuild.changes[0].kind, ChangeKind::Create);
    let cleanup = plan(vec![], BTreeMap::new(), Some(&previous));
    assert_eq!(cleanup.changes[0].kind, ChangeKind::Delete);
    assert!(cleanup.next_manifest.artifacts.is_empty());
}

#[test]
fn duplicate_output_owner_is_rejected_before_diff() {
    let result = build_plan(
        vec![desired("AGENTS.md", b"a"), desired("AGENTS.md", b"b")],
        &BTreeMap::new(),
        None,
        "agentforge 0.1.0",
        "spec",
        "lock",
    );
    assert!(matches!(result, Err(PlanningError::DuplicateOwner(path)) if path == "AGENTS.md"));
}

#[test]
fn case_and_separator_collisions_are_rejected_on_every_platform() {
    for paths in [["Output.md", "output.md"], ["dir\\file.md", "dir/file.md"]] {
        let result = build_plan(
            vec![desired(paths[0], b"a"), desired(paths[1], b"b")],
            &BTreeMap::new(),
            None,
            "agentforge 0.1.0",
            "spec",
            "lock",
        );
        assert!(matches!(result, Err(PlanningError::DuplicateOwner(_))));
    }
}

#[test]
fn manifest_and_changes_are_stably_sorted() {
    let result = plan(
        vec![desired("z", b"z"), desired("a", b"a")],
        BTreeMap::new(),
        None,
    );
    assert_eq!(
        result
            .changes
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(
        result
            .next_manifest
            .artifacts
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
}
