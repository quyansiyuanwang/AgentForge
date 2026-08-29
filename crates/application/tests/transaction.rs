use std::{
    collections::BTreeMap,
    fs,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
};

use agentforge_application::{
    ApplicationService, ApplyError, ApplyValidator, Checkpoint, FaultInjector, NoFaults,
    NoopValidator,
};
use agentforge_core::{
    model::Target,
    planning::{
        DesiredArtifact, Manifest, ManifestArtifact, ResolvedPlan, build_plan, content_hash,
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
fn manifest(items: &[(&str, &[u8])]) -> Manifest {
    Manifest::new(
        "agentforge 0.1.0",
        "spec",
        "lock",
        items
            .iter()
            .map(|(path, bytes)| ManifestArtifact {
                path: (*path).into(),
                target: Target::Codex,
                renderer_version: "codex@1".into(),
                sha256: content_hash(bytes),
            })
            .collect(),
    )
    .unwrap()
}
fn write_manifest(root: &Path, value: &Manifest) -> Vec<u8> {
    fs::create_dir_all(root.join(".agentforge")).unwrap();
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(root.join(".agentforge/manifest.json"), &bytes).unwrap();
    bytes
}
fn update_plan(root: &Path) -> (ResolvedPlan, Vec<u8>) {
    let previous = manifest(&[("old.txt", b"old"), ("remove.txt", b"remove")]);
    fs::write(root.join("old.txt"), b"old").unwrap();
    fs::write(root.join("remove.txt"), b"remove").unwrap();
    let manifest_bytes = write_manifest(root, &previous);
    let current = BTreeMap::from([
        ("old.txt".into(), b"old".to_vec()),
        ("remove.txt".into(), b"remove".to_vec()),
    ]);
    let plan = build_plan(
        vec![
            desired("nested/new.txt", b"created"),
            desired("old.txt", b"new"),
        ],
        &current,
        Some(&previous),
        "agentforge 0.1.0",
        "spec2",
        "lock2",
    )
    .unwrap();
    (plan, manifest_bytes)
}
fn assert_restored(root: &Path, manifest_bytes: &[u8]) {
    assert_eq!(fs::read(root.join("old.txt")).unwrap(), b"old");
    assert_eq!(fs::read(root.join("remove.txt")).unwrap(), b"remove");
    assert!(!root.join("nested/new.txt").exists());
    assert!(!root.join("nested").exists());
    assert_eq!(
        fs::read(root.join(".agentforge/manifest.json")).unwrap(),
        manifest_bytes
    );
    assert!(!root.join(".agentforge/transaction.json").exists());
    assert!(
        !fs::read_dir(root.join(".agentforge"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".txn-"))
    );
}

#[test]
fn successful_apply_creates_modifies_deletes_and_writes_manifest_last() {
    let root = tempfile::tempdir().unwrap();
    let (plan, _) = update_plan(root.path());
    ApplicationService::new(root.path()).apply(&plan).unwrap();
    assert_eq!(fs::read(root.path().join("old.txt")).unwrap(), b"new");
    assert_eq!(
        fs::read(root.path().join("nested/new.txt")).unwrap(),
        b"created"
    );
    assert!(!root.path().join("remove.txt").exists());
    let actual: Manifest =
        serde_json::from_slice(&fs::read(root.path().join(".agentforge/manifest.json")).unwrap())
            .unwrap();
    assert_eq!(actual, plan.next_manifest);
    assert!(!root.path().join(".agentforge/transaction.json").exists());
}

#[derive(Clone, Copy)]
struct FailAt(Checkpoint);
impl FaultInjector for FailAt {
    fn check(&self, checkpoint: Checkpoint) -> Result<(), String> {
        if checkpoint == self.0 {
            Err("fixture".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn every_mutation_checkpoint_rolls_back_completely() {
    for checkpoint in [
        Checkpoint::AfterJournal,
        Checkpoint::AfterBackup,
        Checkpoint::AfterArtifact,
        Checkpoint::BeforeManifest,
        Checkpoint::AfterManifest,
        Checkpoint::DuringPostValidation,
    ] {
        let root = tempfile::tempdir().unwrap();
        let (plan, before) = update_plan(root.path());
        let result =
            ApplicationService::with_components(root.path(), NoopValidator, FailAt(checkpoint))
                .apply(&plan);
        assert!(
            matches!(result, Err(ApplyError::Injected { .. })),
            "{checkpoint:?}: {result:?}"
        );
        assert_restored(root.path(), &before);
    }
}

struct RejectStaging;
impl ApplyValidator for RejectStaging {
    fn validate_staging(&self, _: &Path, _: &ResolvedPlan) -> Result<(), String> {
        Err("invalid renderer output".into())
    }
    fn validate_applied(&self, _: &Path, _: &Manifest) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn staging_failure_leaves_no_transaction_or_new_agentforge_directory() {
    let root = tempfile::tempdir().unwrap();
    let plan = build_plan(
        vec![desired("AGENTS.md", b"new")],
        &BTreeMap::new(),
        None,
        "agentforge",
        "spec",
        "lock",
    )
    .unwrap();
    let result =
        ApplicationService::with_components(root.path(), RejectStaging, NoFaults).apply(&plan);
    assert!(matches!(result, Err(ApplyError::StagingValidation(_))));
    assert!(!root.path().join("AGENTS.md").exists());
    assert!(!root.path().join(".agentforge").exists());
}

#[test]
fn stale_preview_and_reserved_internal_paths_are_rejected_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let plan = build_plan(
        vec![desired("AGENTS.md", b"new")],
        &BTreeMap::new(),
        None,
        "agentforge",
        "spec",
        "lock",
    )
    .unwrap();
    fs::write(root.path().join("AGENTS.md"), b"raced").unwrap();
    assert!(matches!(
        ApplicationService::new(root.path()).apply(&plan),
        Err(ApplyError::StalePreview { .. })
    ));
    let internal = build_plan(
        vec![desired(".agentforge/manifest.json", b"bad")],
        &BTreeMap::new(),
        None,
        "agentforge",
        "spec",
        "lock",
    )
    .unwrap();
    assert!(matches!(
        ApplicationService::new(root.path()).apply(&internal),
        Err(ApplyError::UnsafePath(_))
    ));
}

struct PanicAtArtifact;
impl FaultInjector for PanicAtArtifact {
    fn check(&self, checkpoint: Checkpoint) -> Result<(), String> {
        if checkpoint == Checkpoint::AfterArtifact {
            panic!("simulated crash")
        }
        Ok(())
    }
}

#[test]
fn next_run_restores_crash_journal_and_requires_replanning() {
    let root = tempfile::tempdir().unwrap();
    let (plan, before) = update_plan(root.path());
    let crashed = std::panic::catch_unwind(AssertUnwindSafe(|| {
        ApplicationService::with_components(root.path(), NoopValidator, PanicAtArtifact)
            .apply(&plan)
    }));
    assert!(crashed.is_err());
    assert!(root.path().join(".agentforge/transaction.json").exists());
    assert!(matches!(
        ApplicationService::new(root.path()).apply(&plan),
        Err(ApplyError::RecoveredPreviousTransaction)
    ));
    assert_restored(root.path(), &before);
}

#[test]
fn control_files_replace_atomically_and_restore_on_invalid_path() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".agentforge")).unwrap();
    fs::write(root.path().join(".agentforge/project.yaml"), b"old").unwrap();
    let service = ApplicationService::new(root.path());
    service
        .apply_control_files(&[(PathBuf::from(".agentforge/project.yaml"), b"new".to_vec())])
        .unwrap();
    assert_eq!(
        fs::read(root.path().join(".agentforge/project.yaml")).unwrap(),
        b"new"
    );
    let result = service.apply_control_files(&[
        (
            PathBuf::from(".agentforge/project.yaml"),
            b"changed".to_vec(),
        ),
        (PathBuf::from(".git/blocked"), b"bad".to_vec()),
    ]);
    assert!(result.is_err());
    assert_eq!(
        fs::read(root.path().join(".agentforge/project.yaml")).unwrap(),
        b"new"
    );
    let leftovers = fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".control-"))
        .count();
    assert_eq!(leftovers, 0);
    let duplicate = service.apply_control_files(&[
        (PathBuf::from(".agentforge/a"), b"a".to_vec()),
        (PathBuf::from(".agentforge/a"), b"b".to_vec()),
    ]);
    assert!(matches!(duplicate, Err(ApplyError::UnsafePath(_))));
}

#[test]
fn control_files_with_modes_preserve_vendor_executable_bits() {
    let root = tempfile::tempdir().unwrap();
    let service = ApplicationService::new(root.path());
    service
        .apply_control_files_with_modes(&[(
            PathBuf::from(".agentforge/vendor/skills/demo/run.sh"),
            b"#!/bin/sh\n".to_vec(),
            0o755,
        )])
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.path().join(".agentforge/vendor/skills/demo/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
