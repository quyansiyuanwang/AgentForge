//! Repository fact detection for AgentForge.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::{Path, PathBuf},
};

use agentforge_core::{
    diagnostic::{Diagnostic, DiagnosticCode},
    filesystem::FileSystem,
    model::{Evidence, EvidenceCategory, ProjectProfile},
};
use thiserror::Error;

const MAX_SCAN_DEPTH: usize = 8;
const IGNORED_DIRECTORIES: &[&str] = &[".git", ".agentforge", "node_modules", "target", "vendor"];

#[derive(Debug, Error)]
pub enum DetectorError {
    #[error("filesystem operation failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

#[derive(Debug, Default)]
pub struct DetectorOutput {
    pub evidence: Vec<Evidence>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct DetectionContext<'a> {
    pub root: &'a Path,
    pub filesystem: &'a dyn FileSystem,
}

pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;
    fn detect(&self, context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError>;
}

#[derive(Default)]
pub struct DetectionEngine {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectionEngine {
    pub fn with_builtins() -> Self {
        Self {
            detectors: vec![
                Box::new(RepositoryDetector),
                Box::new(NodeDetector),
                Box::new(PythonDetector),
                Box::new(LanguageDetector),
            ],
        }
    }

    pub fn new(detectors: Vec<Box<dyn Detector>>) -> Self {
        Self { detectors }
    }

    pub fn detect(&self, context: &DetectionContext<'_>) -> DetectionReport {
        let mut evidence = Vec::new();
        let mut diagnostics = Vec::new();
        for detector in &self.detectors {
            match detector.detect(context) {
                Ok(mut output) => {
                    evidence.append(&mut output.evidence);
                    diagnostics.append(&mut output.diagnostics);
                }
                Err(error) => diagnostics.push(
                    Diagnostic::warning(
                        DiagnosticCode::DetectorFailure,
                        format!("detector '{}' failed: {error}", detector.id()),
                    )
                    .with_remediation("fix the unreadable input and run detect again"),
                ),
            }
        }
        DetectionReport {
            profile: aggregate(evidence),
            diagnostics,
        }
    }
}

#[derive(Debug, Default)]
pub struct DetectionReport {
    pub profile: ProjectProfile,
    pub diagnostics: Vec<Diagnostic>,
}

struct RepositoryDetector;

impl Detector for RepositoryDetector {
    fn id(&self) -> &'static str {
        "repository"
    }

    fn detect(&self, context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        let files = scan(context)?;
        let mut output = DetectorOutput::default();
        if metadata_exists(context, ".git")? {
            output.evidence.push(fact(
                self.id(),
                EvidenceCategory::Tool,
                "git",
                1.0,
                ".git",
                "Git metadata directory exists",
            ));
        }
        for file in files {
            let lower = file.to_ascii_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            if name == "dockerfile"
                || name.starts_with("dockerfile.")
                || name.starts_with("compose.")
                || name.starts_with("docker-compose.")
            {
                output.evidence.push(fact(
                    self.id(),
                    EvidenceCategory::Tool,
                    "docker",
                    0.99,
                    &file,
                    "Docker configuration file exists",
                ));
            }
            if lower.starts_with(".github/workflows/")
                && (lower.ends_with(".yml") || lower.ends_with(".yaml"))
            {
                output.evidence.push(fact(
                    self.id(),
                    EvidenceCategory::Ci,
                    "github-actions",
                    1.0,
                    &file,
                    "GitHub Actions workflow exists",
                ));
            }
        }
        Ok(output)
    }
}

struct NodeDetector;

impl Detector for NodeDetector {
    fn id(&self) -> &'static str {
        "node"
    }

    fn detect(&self, context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        let files = scan(context)?;
        let mut output = DetectorOutput::default();
        for file in &files {
            let name = file.rsplit('/').next().unwrap_or(file);
            if name == "tsconfig.json" {
                output.evidence.push(fact(
                    self.id(),
                    EvidenceCategory::Language,
                    "typescript",
                    0.99,
                    file,
                    "tsconfig.json exists",
                ));
            }
            let manager = match name {
                "pnpm-lock.yaml" => Some("pnpm"),
                "package-lock.json" => Some("npm"),
                "yarn.lock" => Some("yarn"),
                "bun.lock" | "bun.lockb" => Some("bun"),
                _ => None,
            };
            if let Some(manager) = manager {
                output.evidence.push(fact(
                    self.id(),
                    EvidenceCategory::PackageManager,
                    manager,
                    1.0,
                    file,
                    "package manager lockfile exists",
                ));
            }
        }
        for file in files.iter().filter(|file| file.ends_with("package.json")) {
            let text = read_text(context, file)?;
            let value: serde_json::Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(error) => {
                    output.diagnostics.push(
                        Diagnostic::warning(
                            DiagnosticCode::MalformedDetectionInput,
                            format!("cannot parse package.json: {error}"),
                        )
                        .at_path(file),
                    );
                    continue;
                }
            };
            output.evidence.push(fact(
                self.id(),
                EvidenceCategory::Language,
                "nodejs",
                0.99,
                file,
                "package.json exists",
            ));
            let dependencies = dependency_names(&value);
            for (dependency, category, name) in [
                ("typescript", EvidenceCategory::Language, "typescript"),
                ("react", EvidenceCategory::Framework, "react"),
                ("next", EvidenceCategory::Framework, "nextjs"),
                ("vue", EvidenceCategory::Framework, "vue"),
                ("svelte", EvidenceCategory::Framework, "svelte"),
                ("pg", EvidenceCategory::Database, "postgres"),
                ("postgres", EvidenceCategory::Database, "postgres"),
                ("redis", EvidenceCategory::Database, "redis"),
                ("ioredis", EvidenceCategory::Database, "redis"),
                ("vitest", EvidenceCategory::Test, "vitest"),
                ("jest", EvidenceCategory::Test, "jest"),
                ("@playwright/test", EvidenceCategory::Test, "playwright"),
            ] {
                if dependencies.contains(dependency) {
                    output.evidence.push(fact(
                        self.id(),
                        category,
                        name,
                        0.99,
                        file,
                        &format!("dependency '{dependency}' is declared"),
                    ));
                }
            }
        }
        Ok(output)
    }
}

struct PythonDetector;

impl Detector for PythonDetector {
    fn id(&self) -> &'static str {
        "python"
    }

    fn detect(&self, context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        let files = scan(context)?;
        let mut output = DetectorOutput::default();
        for file in files.iter().filter(|file| {
            file.ends_with("pyproject.toml")
                || file.ends_with("requirements.txt")
                || file.ends_with("requirements-dev.txt")
        }) {
            let text = read_text(context, file)?;
            if file.ends_with("pyproject.toml") && text.parse::<toml::Value>().is_err() {
                output.diagnostics.push(
                    Diagnostic::warning(
                        DiagnosticCode::MalformedDetectionInput,
                        "cannot parse pyproject.toml",
                    )
                    .at_path(file),
                );
                continue;
            }
            output.evidence.push(fact(
                self.id(),
                EvidenceCategory::Language,
                "python",
                0.99,
                file,
                "Python dependency manifest exists",
            ));
            let normalized = text.to_ascii_lowercase();
            for (needle, category, name) in [
                ("django", EvidenceCategory::Framework, "django"),
                ("fastapi", EvidenceCategory::Framework, "fastapi"),
                ("psycopg", EvidenceCategory::Database, "postgres"),
                ("asyncpg", EvidenceCategory::Database, "postgres"),
                ("redis", EvidenceCategory::Database, "redis"),
                ("pytest", EvidenceCategory::Test, "pytest"),
            ] {
                if contains_dependency(&normalized, needle) {
                    output.evidence.push(fact(
                        self.id(),
                        category,
                        name,
                        0.95,
                        file,
                        &format!("dependency '{needle}' is declared"),
                    ));
                }
            }
        }
        Ok(output)
    }
}

struct LanguageDetector;

impl Detector for LanguageDetector {
    fn id(&self) -> &'static str {
        "languages"
    }

    fn detect(&self, context: &DetectionContext<'_>) -> Result<DetectorOutput, DetectorError> {
        let files = scan(context)?;
        let mut output = DetectorOutput::default();
        for file in files {
            let name = file.rsplit('/').next().unwrap_or(&file);
            let detected = match name {
                "go.mod" => Some(("go", "go.mod exists")),
                "Cargo.toml" => Some(("rust", "Cargo.toml exists")),
                "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                    Some(("java", "Java build manifest exists"))
                }
                _ => None,
            };
            if let Some((language, reason)) = detected {
                output.evidence.push(fact(
                    self.id(),
                    EvidenceCategory::Language,
                    language,
                    0.99,
                    &file,
                    reason,
                ));
            }
        }
        Ok(output)
    }
}

fn scan(context: &DetectionContext<'_>) -> Result<Vec<String>, DetectorError> {
    let mut queue = VecDeque::from([(context.root.to_path_buf(), 0usize)]);
    let mut files = Vec::new();
    while let Some((directory, depth)) = queue.pop_front() {
        let entries =
            context
                .filesystem
                .read_dir(&directory)
                .map_err(|source| DetectorError::Io {
                    path: directory.clone(),
                    source,
                })?;
        for entry in entries {
            let metadata =
                context
                    .filesystem
                    .metadata(&entry)
                    .map_err(|source| DetectorError::Io {
                        path: entry.clone(),
                        source,
                    })?;
            let relative = entry.strip_prefix(context.root).unwrap_or(&entry);
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                let name = entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if depth < MAX_SCAN_DEPTH && !IGNORED_DIRECTORIES.contains(&name) {
                    queue.push_back((entry, depth + 1));
                }
            } else if metadata.is_file() {
                files.push(to_slash(relative));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn metadata_exists(context: &DetectionContext<'_>, path: &str) -> Result<bool, DetectorError> {
    let absolute = context.root.join(path);
    match context.filesystem.metadata(&absolute) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(DetectorError::Io {
            path: absolute,
            source,
        }),
    }
}

fn read_text(context: &DetectionContext<'_>, relative: &str) -> Result<String, DetectorError> {
    let path = context
        .root
        .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    let bytes = context
        .filesystem
        .read(&path)
        .map_err(|source| DetectorError::Io { path, source })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn dependency_names(value: &serde_json::Value) -> BTreeSet<&str> {
    [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ]
    .into_iter()
    .filter_map(|key| value.get(key)?.as_object())
    .flat_map(|object| object.keys().map(String::as_str))
    .collect()
}

fn contains_dependency(text: &str, dependency: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start().trim_matches(['"', '\'']);
        line == dependency
            || line.starts_with(&format!("{dependency}="))
            || line.starts_with(&format!("{dependency}>"))
            || line.starts_with(&format!("{dependency}<"))
            || line.starts_with(&format!("{dependency} "))
            || line.starts_with(&format!("{dependency}\""))
    })
}

fn fact(
    detector: &str,
    category: EvidenceCategory,
    name: &str,
    confidence: f64,
    path: &str,
    reason: &str,
) -> Evidence {
    Evidence {
        detector: detector.into(),
        category,
        name: name.into(),
        confidence,
        paths: vec![path.into()],
        reason: reason.into(),
    }
}

fn aggregate(mut evidence: Vec<Evidence>) -> ProjectProfile {
    evidence.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.paths.cmp(&b.paths))
            .then_with(|| a.detector.cmp(&b.detector))
    });
    let mut grouped = BTreeMap::<EvidenceCategory, BTreeSet<String>>::new();
    for item in &evidence {
        grouped
            .entry(item.category)
            .or_default()
            .insert(item.name.clone());
    }
    let values = |category| {
        grouped
            .get(&category)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    };
    ProjectProfile {
        languages: values(EvidenceCategory::Language),
        frameworks: values(EvidenceCategory::Framework),
        databases: values(EvidenceCategory::Database),
        package_managers: values(EvidenceCategory::PackageManager),
        tests: values(EvidenceCategory::Test),
        ci: values(EvidenceCategory::Ci),
        tools: values(EvidenceCategory::Tool),
        evidence,
    }
}

fn to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
