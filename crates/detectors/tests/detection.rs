use std::{fs, io, path::Path};

use agentforge_core::{
    DiagnosticCode,
    filesystem::RealFileSystem,
    model::{Evidence, EvidenceCategory},
};
use agentforge_detectors::{
    DetectionContext, DetectionEngine, Detector, DetectorError, DetectorOutput,
};

fn write(root: &Path, path: &str, content: &str) {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().unwrap()).expect("create fixture parent");
    fs::write(destination, content).expect("write fixture");
}

fn detect(root: &Path) -> agentforge_detectors::DetectionReport {
    DetectionEngine::with_builtins().detect(&DetectionContext {
        root,
        filesystem: &RealFileSystem,
    })
}

#[test]
fn detects_the_complete_v01_technology_matrix() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    write(repository.path(), "Dockerfile", "FROM scratch");
    write(repository.path(), ".github/workflows/ci.yml", "jobs: {}");
    write(repository.path(), "pnpm-lock.yaml", "lockfileVersion: 9");
    write(repository.path(), "package-lock.json", "{}");
    write(repository.path(), "yarn.lock", "");
    write(repository.path(), "bun.lock", "");
    write(repository.path(), "tsconfig.json", "{}");
    write(
        repository.path(),
        "package.json",
        r#"{"dependencies":{"typescript":"1","react":"1","next":"1","vue":"1","svelte":"1","pg":"1","redis":"1","vitest":"1","jest":"1","@playwright/test":"1"}}"#,
    );
    write(
        repository.path(),
        "requirements.txt",
        "django==5\nfastapi>=1\npsycopg==3\nredis==5\npytest==8\n",
    );
    write(repository.path(), "go.mod", "module example.com/test");
    write(repository.path(), "Cargo.toml", "[package]\nname='fixture'");
    write(repository.path(), "pom.xml", "<project/>");

    let report = detect(repository.path());
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(
        report.profile.languages,
        ["go", "java", "nodejs", "python", "rust", "typescript"]
    );
    assert_eq!(
        report.profile.frameworks,
        ["django", "fastapi", "nextjs", "react", "svelte", "vue"]
    );
    assert_eq!(report.profile.databases, ["postgres", "redis"]);
    assert_eq!(
        report.profile.package_managers,
        ["bun", "npm", "pnpm", "yarn"]
    );
    assert_eq!(
        report.profile.tests,
        ["jest", "playwright", "pytest", "vitest"]
    );
    assert_eq!(report.profile.ci, ["github-actions"]);
    assert_eq!(report.profile.tools, ["docker", "git"]);
    assert!(report.profile.evidence.iter().all(|item| {
        (0.0..=1.0).contains(&item.confidence)
            && !item.paths.is_empty()
            && item.paths.iter().all(|path| !Path::new(path).is_absolute())
    }));
}

#[test]
fn scans_monorepo_manifests_but_ignores_generated_directories() {
    let repository = tempfile::tempdir().unwrap();
    write(
        repository.path(),
        "apps/web/package.json",
        r#"{"dependencies":{"next":"1"}}"#,
    );
    write(
        repository.path(),
        "node_modules/ignored/package.json",
        r#"{"dependencies":{"vue":"1"}}"#,
    );

    let report = detect(repository.path());
    assert_eq!(report.profile.frameworks, ["nextjs"]);
    assert!(
        report
            .profile
            .evidence
            .iter()
            .any(|item| item.paths == ["apps/web/package.json"])
    );
    assert!(
        !report
            .profile
            .evidence
            .iter()
            .any(|item| item.paths[0].contains("node_modules"))
    );
}

#[test]
fn empty_repository_has_an_empty_profile() {
    let repository = tempfile::tempdir().unwrap();
    let report = detect(repository.path());
    assert!(report.profile.evidence.is_empty());
    assert!(report.diagnostics.is_empty());
}

#[test]
fn malformed_manifest_warns_without_discarding_other_facts() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    write(repository.path(), "package.json", "{");
    write(repository.path(), "go.mod", "module example.com/test");

    let report = detect(repository.path());
    assert!(report.profile.tools.contains(&"git".into()));
    assert!(report.profile.languages.contains(&"go".into()));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.code == DiagnosticCode::MalformedDetectionInput)
    );
}

struct FailingDetector;

impl Detector for FailingDetector {
    fn id(&self) -> &'static str {
        "failing"
    }

    fn detect(&self, _context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        Err(DetectorError::Io {
            path: "broken".into(),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "fixture failure"),
        })
    }
}

struct WorkingDetector;

impl Detector for WorkingDetector {
    fn id(&self) -> &'static str {
        "working"
    }

    fn detect(&self, _context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        Ok(DetectorOutput {
            evidence: vec![Evidence {
                detector: self.id().into(),
                category: EvidenceCategory::Language,
                name: "rust".into(),
                confidence: 1.0,
                paths: vec!["Cargo.toml".into()],
                reason: "fixture".into(),
            }],
            diagnostics: vec![],
        })
    }
}

#[test]
fn one_detector_failure_does_not_discard_other_results() {
    let repository = tempfile::tempdir().unwrap();
    let engine = DetectionEngine::new(vec![Box::new(FailingDetector), Box::new(WorkingDetector)]);
    let report = engine.detect(&DetectionContext {
        root: repository.path(),
        filesystem: &RealFileSystem,
    });
    assert_eq!(report.profile.languages, ["rust"]);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.code == DiagnosticCode::DetectorFailure)
    );
}
