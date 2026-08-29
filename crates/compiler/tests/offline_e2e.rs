use std::{collections::BTreeMap, fs, path::Path};

use agentforge_application::ApplicationService;
use agentforge_compiler::ProjectCompiler;
use agentforge_core::planning::ChangeKind;
use agentforge_sources::LockFile;

fn write(root: &Path, path: &str, content: &str) {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, content).unwrap();
}

fn setup(root: &Path) {
    write(root, "ai/project.md", "Run tests before committing.\n");
    write(
        root,
        "ai/reviewer.md",
        "Inspect correctness and security.\n",
    );
    write(
        root,
        "ai/skills/testing/SKILL.md",
        "---\nname: testing\n---\nTest carefully.\n",
    );
    write(
        root,
        ".agentforge/project.yaml",
        r#"schemaVersion: "1"
project: { name: fixture }
targets: [codex, claude, copilot]
instructions:
  - id: project
    source: { type: local, path: ai/project.md }
skills:
  - id: testing
    source: { type: local, path: ai/skills/testing }
mcp:
  - id: github
    transport:
      type: stdio
      command: github-mcp-server
      args: [stdio]
      env: { GITHUB_TOKEN: "${GITHUB_TOKEN}" }
    env: [GITHUB_TOKEN]
subagents:
  - id: reviewer
    description: Review correctness and security
    instructions: { type: local, path: ai/reviewer.md }
    skills: [testing]
settings:
  permissions: { shell: ask, network: ask }
  sandbox: workspace-write
extensions:
  codex: { reasoningEffort: high }
  claude: { permissionMode: default }
  copilot: { mcpSurface: cli }
"#,
    );
    let lock = LockFile::new("agentforge 0.1.0", vec![])
        .unwrap()
        .to_yaml()
        .unwrap();
    write(root, ".agentforge/lock.yaml", &lock);
}

fn desired_bytes(compilation: &agentforge_compiler::Compilation) -> BTreeMap<String, Vec<u8>> {
    compilation
        .plan
        .changes
        .iter()
        .filter_map(|change| {
            change
                .desired
                .as_ref()
                .map(|desired| (desired.path.clone(), desired.content.clone()))
        })
        .collect()
}

#[test]
fn tracked_inputs_rebuild_all_targets_offline_and_detect_drift() {
    let repository = tempfile::tempdir().unwrap();
    setup(repository.path());
    let first = ProjectCompiler::new(repository.path()).compile().unwrap();
    assert!(!first.plan.has_conflicts());
    assert!(
        first
            .plan
            .changes
            .iter()
            .all(|change| change.kind == ChangeKind::Create)
    );
    let expected = desired_bytes(&first);
    ApplicationService::new(repository.path())
        .apply(&first.plan)
        .unwrap();

    let clean = ProjectCompiler::new(repository.path()).compile().unwrap();
    assert!(!clean.plan.has_changes());
    assert!(
        clean
            .plan
            .changes
            .iter()
            .all(|change| change.kind == ChangeKind::Unchanged)
    );

    for path in expected.keys() {
        fs::remove_file(repository.path().join(path)).unwrap();
    }
    let rebuild = ProjectCompiler::new(repository.path()).compile().unwrap();
    assert!(
        rebuild
            .plan
            .changes
            .iter()
            .all(|change| change.kind == ChangeKind::Create)
    );
    assert_eq!(desired_bytes(&rebuild), expected);
    ApplicationService::new(repository.path())
        .apply(&rebuild.plan)
        .unwrap();

    let drift_path = expected.keys().next().unwrap();
    fs::write(repository.path().join(drift_path), b"user edit").unwrap();
    let drift = ProjectCompiler::new(repository.path()).compile().unwrap();
    assert!(drift.plan.has_conflicts());
}

#[test]
fn strict_mode_blocks_visible_capability_downgrades() {
    let repository = tempfile::tempdir().unwrap();
    setup(repository.path());
    let compilation = ProjectCompiler::new(repository.path()).compile().unwrap();
    assert!(!compilation.blocks_apply(false));
    assert!(compilation.blocks_apply(true));
    assert!(
        compilation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.target.as_deref() == Some("copilot"))
    );
}

#[test]
fn repeated_offline_compilation_is_byte_stable() {
    let repository = tempfile::tempdir().unwrap();
    setup(repository.path());
    let first = ProjectCompiler::new(repository.path()).compile().unwrap();
    let expected = desired_bytes(&first);
    ApplicationService::new(repository.path())
        .apply(&first.plan)
        .unwrap();
    for _ in 0..100 {
        let compilation = ProjectCompiler::new(repository.path()).compile().unwrap();
        assert!(!compilation.plan.has_changes());
        assert_eq!(desired_bytes(&compilation), expected);
    }
}

#[test]
fn lock_schema_and_entries_are_validated_before_rendering() {
    let repository = tempfile::tempdir().unwrap();
    setup(repository.path());
    let lock_path = repository.path().join(".agentforge/lock.yaml");
    fs::write(
        &lock_path,
        "schemaVersion: '2'\ngeneratedBy: agentforge 0.1.0\nsources: []\n",
    )
    .unwrap();
    let error = ProjectCompiler::new(repository.path())
        .compile()
        .unwrap_err();
    assert!(matches!(
        error,
        agentforge_compiler::CompileError::UnsupportedLockVersion(version) if version == "2"
    ));

    fs::write(
        &lock_path,
        "schemaVersion: '1'\ngeneratedBy: unknown\nsources: []\n",
    )
    .unwrap();
    let error = ProjectCompiler::new(repository.path())
        .compile()
        .unwrap_err();
    assert!(matches!(
        error,
        agentforge_compiler::CompileError::InvalidLockGenerator(generator) if generator == "unknown"
    ));

    fs::write(
        &lock_path,
        "schemaVersion: '1'\ngeneratedBy: agentforge 0.1.0\nsources:\n  - kind: skill\n    id: testing\n    sourceType: git\n    requestedLocator: x\n    resolvedLocator: y\n    vendorPath: .agentforge/vendor/skills/testing\n    sha256: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    executableContent: false\n    fileCount: 0\n    totalBytes: 0\n  - kind: skill\n    id: testing\n    sourceType: git\n    requestedLocator: x\n    resolvedLocator: y\n    vendorPath: .agentforge/vendor/skills/testing\n    sha256: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    executableContent: false\n    fileCount: 0\n    totalBytes: 0\n",
    )
    .unwrap();
    let error = ProjectCompiler::new(repository.path())
        .compile()
        .unwrap_err();
    assert!(matches!(
        error,
        agentforge_compiler::CompileError::DuplicateLockEntry(
            agentforge_sources::ContentKind::Skill,
            id
        ) if id == "testing"
    ));
}
