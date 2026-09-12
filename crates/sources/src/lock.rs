use serde::{Deserialize, Serialize};

use crate::SourceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentKind {
    Instruction,
    Skill,
    Mcp,
    Subagent,
}

impl ContentKind {
    pub const fn vendor_segment(self) -> &'static str {
        match self {
            Self::Instruction => "instructions",
            Self::Skill => "skills",
            Self::Mcp => "mcp",
            Self::Subagent => "subagents",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceType {
    Url,
    Git,
    SkillsSh,
    McpRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockFile {
    pub schema_version: String,
    pub generated_by: String,
    pub sources: Vec<LockEntry>,
}

impl LockFile {
    pub fn new(
        generated_by: impl Into<String>,
        mut sources: Vec<LockEntry>,
    ) -> Result<Self, SourceError> {
        sources.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));
        for pair in sources.windows(2) {
            if pair[0].kind == pair[1].kind && pair[0].id == pair[1].id {
                return Err(SourceError::DuplicateLockEntry {
                    kind: pair[0].kind,
                    id: pair[0].id.clone(),
                });
            }
        }
        Ok(Self {
            schema_version: "1".into(),
            generated_by: generated_by.into(),
            sources,
        })
    }

    pub fn to_yaml(&self) -> Result<String, SourceError> {
        serde_yaml::to_string(self).map_err(SourceError::SerializeLock)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockEntry {
    pub kind: ContentKind,
    pub id: String,
    pub source_type: SourceType,
    pub requested_locator: String,
    pub resolved_locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    pub vendor_path: String,
    pub sha256: String,
    pub executable_content: bool,
    /// Vendor-relative paths that the platform-independent classification
    /// marked executable. Unix permission bits cannot be recovered on every
    /// filesystem, so the lock carries the authoritative list and offline
    /// verification replays it before hashing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_paths: Vec<String>,
    pub file_count: u64,
    pub total_bytes: u64,
}
