use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Diagnostic, DiagnosticCode, model::Target};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub generated_by: String,
    pub spec_sha256: String,
    pub lock_sha256: String,
    pub artifacts: Vec<ManifestArtifact>,
}

impl Manifest {
    pub fn new(
        generated_by: impl Into<String>,
        spec_sha256: impl Into<String>,
        lock_sha256: impl Into<String>,
        mut artifacts: Vec<ManifestArtifact>,
    ) -> Result<Self, PlanningError> {
        artifacts.sort_by(|a, b| a.path.cmp(&b.path));
        let mut ownership = BTreeSet::new();
        for artifact in &artifacts {
            if !ownership.insert(ownership_key(&artifact.path)) {
                return Err(PlanningError::DuplicateOwner(artifact.path.clone()));
            }
        }
        Ok(Self {
            schema_version: "1".into(),
            generated_by: generated_by.into(),
            spec_sha256: spec_sha256.into(),
            lock_sha256: lock_sha256.into(),
            artifacts,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestArtifact {
    pub path: String,
    pub target: Target,
    pub renderer_version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredArtifact {
    pub path: String,
    pub target: Target,
    pub renderer_version: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChangeKind {
    Create,
    Modify,
    Delete,
    Conflict,
    Unchanged,
    ManualAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactChange {
    pub path: String,
    pub kind: ChangeKind,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub desired: Option<DesiredArtifact>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlan {
    pub changes: Vec<ArtifactChange>,
    pub next_manifest: Manifest,
    pub diagnostics: Vec<Diagnostic>,
}

impl ResolvedPlan {
    pub fn has_conflicts(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.kind == ChangeKind::Conflict)
    }
    pub fn has_changes(&self) -> bool {
        self.changes.iter().any(|change| {
            matches!(
                change.kind,
                ChangeKind::Create | ChangeKind::Modify | ChangeKind::Delete
            )
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanningError {
    #[error("multiple renderers own artifact path {0}")]
    DuplicateOwner(String),
}

pub fn build_plan(
    desired: Vec<DesiredArtifact>,
    current: &BTreeMap<String, Vec<u8>>,
    previous: Option<&Manifest>,
    generated_by: &str,
    spec_sha256: &str,
    lock_sha256: &str,
) -> Result<ResolvedPlan, PlanningError> {
    let mut desired_by_path = BTreeMap::new();
    let mut ownership = BTreeSet::new();
    for artifact in desired {
        let path = artifact.path.clone();
        if !ownership.insert(ownership_key(&path))
            || desired_by_path.insert(path.clone(), artifact).is_some()
        {
            return Err(PlanningError::DuplicateOwner(path));
        }
    }
    let previous_by_path = previous
        .map(|manifest| {
            manifest
                .artifacts
                .iter()
                .map(|artifact| (artifact.path.clone(), artifact))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let all_paths = desired_by_path
        .keys()
        .cloned()
        .chain(previous_by_path.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut next_artifacts = Vec::new();
    for path in all_paths {
        let desired = desired_by_path.remove(&path);
        let owner = previous_by_path.get(&path).copied();
        let current_hash = current.get(&path).map(|bytes| content_hash(bytes));
        let desired_hash = desired
            .as_ref()
            .map(|artifact| content_hash(&artifact.content));
        let (kind, reason) = classify(owner, current_hash.as_deref(), desired_hash.as_deref());
        if kind == ChangeKind::Conflict {
            let (code, message) = if owner.is_some() {
                (
                    DiagnosticCode::ArtifactDrift,
                    format!("managed artifact '{path}' has drifted"),
                )
            } else {
                (
                    DiagnosticCode::ArtifactConflict,
                    format!("unmanaged artifact '{path}' blocks generation"),
                )
            };
            diagnostics.push(Diagnostic::error(code, message).at_path(&path).with_remediation("move user content to a canonical source and restore or remove the conflicting target file"));
        }
        if let Some(artifact) = &desired {
            next_artifacts.push(ManifestArtifact {
                path: path.clone(),
                target: artifact.target,
                renderer_version: artifact.renderer_version.clone(),
                sha256: desired_hash.clone().expect("desired hash exists"),
            });
        }
        changes.push(ArtifactChange {
            path,
            kind,
            before_sha256: current_hash,
            after_sha256: desired_hash,
            desired,
            reason,
        });
    }
    let next_manifest = Manifest::new(generated_by, spec_sha256, lock_sha256, next_artifacts)?;
    Ok(ResolvedPlan {
        changes,
        next_manifest,
        diagnostics,
    })
}

fn classify(
    owner: Option<&ManifestArtifact>,
    current: Option<&str>,
    desired: Option<&str>,
) -> (ChangeKind, Option<String>) {
    match (owner, current, desired) {
        (None, None, Some(_)) => (ChangeKind::Create, None),
        (None, Some(_), Some(_)) => (
            ChangeKind::Conflict,
            Some("path exists without manifest ownership".into()),
        ),
        (Some(_), None, Some(_)) => (
            ChangeKind::Create,
            Some("managed artifact is missing and will be rebuilt".into()),
        ),
        (Some(owner), Some(current), _) if current != owner.sha256 => (
            ChangeKind::Conflict,
            Some("current hash differs from manifest".into()),
        ),
        (Some(_), Some(current), Some(desired)) if current == desired => {
            (ChangeKind::Unchanged, None)
        }
        (Some(_), Some(_), Some(_)) => (ChangeKind::Modify, None),
        (Some(_), Some(_), None) => (ChangeKind::Delete, None),
        (Some(_), None, None) => (
            ChangeKind::Delete,
            Some("artifact is already absent; manifest ownership will be removed".into()),
        ),
        (None, _, None) => unreachable!("path union excludes unowned undesired paths"),
    }
}

pub fn content_hash(content: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(content)))
}

fn ownership_key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}
