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
