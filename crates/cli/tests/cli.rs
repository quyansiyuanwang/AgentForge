use std::{fs, path::Path};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

fn write(root: &Path, path: &str, content: &str) {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, content).unwrap();
}

fn setup(root: &Path) {
    write(root, "ai/project.md", "Run tests.\n");
    write(root, "ai/reviewer.md", "Review carefully.\n");
    write(
        root,
        "ai/skills/testing/SKILL.md",
        "---\nname: testing\n---\nTest carefully.\n",
    );
    write(
        root,
        ".agentforge/lock.yaml",
        "schemaVersion: '1'\ngeneratedBy: agentforge 0.1.0\nsources: []\n",
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
    transport: { type: stdio, command: github-mcp-server, env: { GITHUB_TOKEN: "${GITHUB_TOKEN}" } }
    env: [GITHUB_TOKEN]
subagents:
  - id: reviewer
    description: Review correctness
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
}

#[test]
fn help_exposes_the_stable_command_surface_and_usage_errors_exit_two() {
    cargo_bin_cmd!("agentforge")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("detect"))
        .stdout(predicate::str::contains("subagent"))
        .stdout(predicate::str::contains("config"));
    cargo_bin_cmd!("agentforge").arg("unknown").assert().code(2);
}

#[test]
fn detect_json_uses_a_clean_stable_envelope() {
    let root = tempfile::tempdir().unwrap();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["detect", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], "1");
    assert_eq!(value["command"], "detect");
    assert_eq!(value["status"], "success");
}

#[test]
fn config_validate_reports_business_failure_as_exit_one() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "bad.yaml", "targets: []\n");
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["config", "validate", "bad.yaml", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "failure");
    assert!(
        value["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["code"].as_str().unwrap().starts_with("AF"))
    );
}

#[test]
fn sync_is_idempotent_diff_detects_drift_and_strict_blocks_warnings() {
    let root = tempfile::tempdir().unwrap();
    setup(root.path());
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["sync", "--strict"])
        .assert()
        .code(1);
    assert!(!root.path().join("AGENTS.md").exists());
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied changes"));
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes"));
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("diff")
        .assert()
        .success()
        .stdout("Clean\n");
    fs::write(root.path().join("AGENTS.md"), b"user edit").unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("diff")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Conflict AGENTS.md"));
}

#[test]
fn doctor_reports_names_not_secret_values() {
    let root = tempfile::tempdir().unwrap();
    setup(root.path());
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .success();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["doctor", "--json"])
        .env("GITHUB_TOKEN", "must-not-leak")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("GITHUB_TOKEN"));
    assert!(!text.contains("must-not-leak"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        value["data"]["health"]
            .as_str()
            .unwrap()
            .starts_with("Healthy")
    );
}

#[test]
fn init_non_interactive_generates_spec_and_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["init", "--non-interactive", "--target", "generic"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.path().join(".agentforge/project.yaml").exists());
    assert!(root.path().join(".agentforge/lock.yaml").exists());
    assert!(root.path().join("AGENTS.md").exists());
    assert!(root.path().join(".agentforge/manifest.json").exists());
}

#[test]
fn init_non_interactive_requires_explicit_target() {
    let root = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["init", "--non-interactive"])
        .assert()
        .code(2);
}

#[test]
fn init_validation_failure_does_not_leave_control_files() {
    let root = tempfile::tempdir().unwrap();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args([
            "init",
            "--non-interactive",
            "--target",
            "codex",
            "--skill",
            "missing/skill",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.path().join(".agentforge/project.yaml").exists());
    assert!(!root.path().join(".agentforge/lock.yaml").exists());
}

#[test]
fn init_dry_run_reports_planned_artifacts_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args([
            "init",
            "--non-interactive",
            "--target",
            "codex",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!root.path().join(".agentforge/project.yaml").exists());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["data"]["plannedArtifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "AGENTS.md")
    );
    assert!(
        value["data"]["plannedArtifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == ".codex/config.toml")
    );
}

#[test]
fn init_three_vendor_targets_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args([
            "init",
            "--non-interactive",
            "--target",
            "codex",
            "--target",
            "claude",
            "--target",
            "copilot",
        ])
        .assert()
        .success();
    assert!(root.path().join("AGENTS.md").exists());
    assert!(root.path().join("CLAUDE.md").exists());
    assert!(root.path().join(".github/copilot-instructions.md").exists());
    assert!(root.path().join(".agentforge/lock.yaml").exists());
    assert!(root.path().join(".agentforge/manifest.json").exists());
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes"));
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("diff")
        .assert()
        .success()
        .stdout("Clean\n");
}

#[test]
fn nextjs_project_full_offline_rebuild_e2e() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"name":"next-fixture","scripts":{"build":"next build","test":"vitest"},"dependencies":{"next":"15.0.0","react":"19.0.0","react-dom":"19.0.0"},"devDependencies":{"vitest":"2.0.0"}}"#,
    );
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args([
            "init",
            "--non-interactive",
            "--target",
            "codex",
            "--target",
            "claude",
            "--target",
            "copilot",
        ])
        .assert()
        .success();
    assert!(root.path().join(".agentforge/project.yaml").exists());
    assert!(root.path().join(".agentforge/lock.yaml").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(".agentforge/manifest.json")).unwrap())
            .unwrap();
    let artifacts = manifest["artifacts"].as_array().unwrap().to_owned();
    assert!(artifacts.iter().any(|item| item["path"] == "AGENTS.md"));
    let doctor = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("Healthy"));
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes"));
    for artifact in &artifacts {
        fs::remove_file(root.path().join(artifact["path"].as_str().unwrap())).unwrap();
    }
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .env("AGENTFORGE_OFFLINE", "1")
        .arg("sync")
        .assert()
        .success();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("diff")
        .assert()
        .success()
        .stdout("Clean\n");
    fs::write(root.path().join("AGENTS.md"), b"user edit").unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .arg("sync")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Conflict AGENTS.md"));
}

#[test]
fn doctor_reports_missing_lockfile_as_warning() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "ai/project.md", "instructions\n");
    write(
        root.path(),
        ".agentforge/project.yaml",
        "schemaVersion: '1'\nproject: { name: fixture }\ntargets: [generic]\ninstructions: [{ id: project, source: { type: local, path: ai/project.md } }]\n",
    );
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "warning");
    assert!(value["diagnostics"].as_array().unwrap().iter().any(|item| {
        item["message"]
            .as_str()
            .unwrap()
            .contains("lock.yaml is missing")
    }));
}

#[test]
fn cli_sync_rebuilds_targets_from_tracked_state() {
    let root = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["init", "--non-interactive", "--target", "generic"])
        .assert()
        .success();
    let generated = root.path().join("AGENTS.md");
    let original = fs::read(&generated).unwrap();
    fs::remove_file(&generated).unwrap();
    cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .env("AGENTFORGE_OFFLINE", "1")
        .arg("sync")
        .assert()
        .success();
    assert_eq!(fs::read(generated).unwrap(), original);
}

#[test]
fn sync_json_returns_envelope() {
    let root = tempfile::tempdir().unwrap();
    setup(root.path());
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["sync", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "sync");
    assert_eq!(value["schemaVersion"], "1");
}

#[test]
fn resource_remove_edits_canonical_spec_without_vendor_access() {
    let root = tempfile::tempdir().unwrap();
    setup(root.path());
    fs::write(root.path().join(".agentforge/lock.yaml"), b"not valid lock").unwrap();
    let output = cargo_bin_cmd!("agentforge")
        .current_dir(root.path())
        .args(["skill", "remove", "testing", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("AF1103"));
}
