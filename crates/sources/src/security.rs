use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

use sha2::{Digest, Sha256};

use crate::SourceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Regular,
    Directory,
    Symlink,
    HardLink,
    Device,
    Fifo,
    Socket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedEntry {
    pub path: String,
    pub kind: EntryKind,
    pub mode: u32,
    pub content: Vec<u8>,
}

impl UntrustedEntry {
    pub fn file(path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Regular,
            mode: 0o644,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VendorLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_dependency_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for VendorLimits {
    fn default() -> Self {
        Self {
            max_files: 1_000,
            max_file_bytes: 5 * 1024 * 1024,
            max_dependency_bytes: 20 * 1024 * 1024,
            max_total_bytes: 100 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorFile {
    pub path: String,
    pub mode: u32,
    pub content: Vec<u8>,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorTree {
    pub files: Vec<VendorFile>,
    pub sha256: String,
    pub total_bytes: u64,
    pub executable_content: bool,
}

impl VendorTree {
    pub fn validate(
        entries: Vec<UntrustedEntry>,
        limits: VendorLimits,
        installed_bytes: u64,
    ) -> Result<Self, SourceError> {
        let mut paths = BTreeMap::<String, String>::new();
        let mut files = Vec::new();
        let mut total_bytes = 0u64;
        for entry in entries {
            if entry.kind == EntryKind::Directory {
                continue;
            }
            if entry.kind != EntryKind::Regular {
                return Err(SourceError::ForbiddenEntry {
                    path: entry.path,
                    kind: entry.kind,
                });
            }
            let normalized = normalize_path(&entry.path)?;
            let collision_key = normalized.to_ascii_lowercase();
            if let Some(previous) = paths.insert(collision_key, normalized.clone()) {
                return Err(SourceError::PathCollision {
                    first: previous,
                    second: normalized,
                });
            }
            let size = u64::try_from(entry.content.len()).unwrap_or(u64::MAX);
            if size > limits.max_file_bytes {
                return Err(SourceError::FileTooLarge {
                    path: normalized,
                    size,
                    limit: limits.max_file_bytes,
                });
            }
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or(SourceError::SizeOverflow)?;
            let executable =
                entry.mode & 0o111 != 0 || looks_executable(&normalized, &entry.content);
            files.push(VendorFile {
                path: normalized,
                mode: if executable { 0o755 } else { 0o644 },
                content: entry.content,
                executable,
            });
        }
        if files.len() > limits.max_files {
            return Err(SourceError::TooManyFiles {
                count: files.len(),
                limit: limits.max_files,
            });
        }
        if total_bytes > limits.max_dependency_bytes {
            return Err(SourceError::DependencyTooLarge {
                size: total_bytes,
                limit: limits.max_dependency_bytes,
            });
        }
        if installed_bytes
            .checked_add(total_bytes)
            .ok_or(SourceError::SizeOverflow)?
            > limits.max_total_bytes
        {
            return Err(SourceError::TotalTooLarge {
                size: installed_bytes + total_bytes,
                limit: limits.max_total_bytes,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let sha256 = tree_hash(&files);
        let executable_content = files.iter().any(|file| file.executable);
        Ok(Self {
            files,
            sha256,
            total_bytes,
            executable_content,
        })
    }
}

fn normalize_path(value: &str) -> Result<String, SourceError> {
    if value.is_empty() || value.contains('\0') {
        return Err(SourceError::UnsafePath(value.into()));
    }
    let replaced = value.replace('\\', "/");
    if replaced.starts_with('/')
        || replaced
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || reserved(part))
    {
        return Err(SourceError::UnsafePath(value.into()));
    }
    if Path::new(value).components().any(|part| {
        matches!(
            part,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(SourceError::UnsafePath(value.into()));
    }
    Ok(replaced)
}

fn reserved(component: &str) -> bool {
    let stem = component
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn looks_executable(path: &str, content: &[u8]) -> bool {
    let lower = path.to_ascii_lowercase();
    content.starts_with(b"#!")
        || [
            ".sh", ".bash", ".zsh", ".fish", ".ps1", ".bat", ".cmd", ".exe", ".com", ".dll", ".so",
            ".dylib", ".jar",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn tree_hash(files: &[VendorFile]) -> String {
    let mut digest = Sha256::new();
    for file in files {
        update_length_prefixed(&mut digest, file.path.as_bytes());
        digest.update(file.mode.to_be_bytes());
        update_length_prefixed(&mut digest, &file.content);
    }
    format!("sha256:{}", hex::encode(digest.finalize()))
}

fn update_length_prefixed(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}
