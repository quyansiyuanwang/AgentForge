use agentforge_core::model::Source;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    adapters::{GitFetcher, HttpFetcher},
    lock::{ContentKind, LockEntry, SourceType},
    security::{EntryKind, UntrustedEntry, VendorLimits, VendorTree},
};

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("duplicate lock entry {kind:?}/{id}")]
    DuplicateLockEntry { kind: ContentKind, id: String },
    #[error("failed to serialize lock file: {0}")]
    SerializeLock(serde_yaml::Error),
    #[error("unsafe vendor path: {0}")]
    UnsafePath(String),
    #[error("forbidden vendor entry {path}: {kind:?}")]
    ForbiddenEntry { path: String, kind: EntryKind },
    #[error("vendor paths collide: {first} and {second}")]
    PathCollision { first: String, second: String },
    #[error("vendor file {path} is {size} bytes; limit is {limit}")]
    FileTooLarge { path: String, size: u64, limit: u64 },
    #[error("vendor dependency has {count} files; limit is {limit}")]
    TooManyFiles { count: usize, limit: usize },
    #[error("vendor dependency is {size} bytes; limit is {limit}")]
    DependencyTooLarge { size: u64, limit: u64 },
    #[error("all vendor dependencies total {size} bytes; limit is {limit}")]
    TotalTooLarge { size: u64, limit: u64 },
    #[error("size calculation overflow")]
    SizeOverflow,
    #[error("failed to build HTTP client: {0}")]
    HttpClient(reqwest::Error),
    #[error("HTTP request failed: {0}")]
    Http(reqwest::Error),
    #[error("invalid URL: {0}")]
    InvalidUrl(url::ParseError),
    #[error("remote URL must use HTTPS: {0}")]
    InsecureUrl(String),
    #[error("remote URL must not contain credentials")]
    UrlCredentials,
    #[error("redirect response omitted Location")]
    RedirectWithoutLocation,
    #[error("redirect Location is invalid")]
    InvalidRedirect,
    #[error("source exceeded the limit of {0} redirects")]
    TooManyRedirects(usize),
    #[error("remote server rate limited the request")]
    RateLimited,
    #[error("remote server returned HTTP {0}")]
    HttpStatus(u16),
    #[error("download is {size} bytes; limit is {limit}")]
    DownloadTooLarge { size: u64, limit: u64 },
    #[error("source {0} must specify a version unless unpinned sources are explicitly allowed")]
    Unpinned(String),
    #[error("invalid skills.sh-compatible reference: {0}")]
    InvalidSkillsReference(String),
    #[error("invalid MCP registry response: {0}")]
    InvalidRegistryResponse(String),
    #[error("source checksum mismatch: expected {expected}, actual {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("third-party executable content requires explicit authorization")]
    ExecutableContentNotAllowed,
    #[error("Git source failed: {0}")]
    Git(String),
    #[error("offline vendor verification failed for {path}: {reason}")]
    OfflineVerification { path: String, reason: String },
}

pub struct ResolveRequest<'a> {
    pub kind: ContentKind,
    pub id: &'a str,
    pub source: &'a Source,
    pub allow_unpinned: bool,
    pub allow_executable_content: bool,
    pub installed_vendor_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum ResolvedSource {
    Local {
        path: String,
    },
    Remote {
        lock: Box<LockEntry>,
        vendor: VendorTree,
    },
}

pub struct SourceService<H, G> {
    http: H,
    git: G,
    limits: VendorLimits,
    mcp_registry_base: String,
}

impl<H: HttpFetcher, G: GitFetcher> SourceService<H, G> {
    pub fn new(http: H, git: G) -> Self {
        Self {
            http,
            git,
            limits: VendorLimits::default(),
            mcp_registry_base: "https://registry.modelcontextprotocol.io".into(),
        }
    }

    pub fn with_limits(mut self, limits: VendorLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn with_mcp_registry_base(mut self, base: impl Into<String>) -> Self {
        self.mcp_registry_base = base.into();
        self
    }

    pub async fn resolve(
        &self,
        request: ResolveRequest<'_>,
    ) -> Result<ResolvedSource, SourceError> {
        match request.source {
            Source::Local { path } => Ok(ResolvedSource::Local { path: path.clone() }),
            Source::Url { url, sha256 } => self.resolve_url(request, url, sha256.as_deref()).await,
            Source::Git {
                repository,
                rev,
                subpath,
            } => {
                self.resolve_git(
                    request,
                    SourceType::Git,
                    repository,
                    rev,
                    subpath.as_deref(),
                    format!("{repository}@{rev}"),
                )
                .await
            }
            Source::SkillsSh { r#ref, version } => {
                self.resolve_skill(request, r#ref, version.as_deref()).await
            }
            Source::McpRegistry { r#ref, version } => {
                self.resolve_mcp(request, r#ref, version.as_deref()).await
            }
        }
    }

    async fn resolve_url(
        &self,
        request: ResolveRequest<'_>,
        url: &str,
        expected: Option<&str>,
    ) -> Result<ResolvedSource, SourceError> {
        if expected.is_none() && !request.allow_unpinned {
            return Err(SourceError::Unpinned(url.into()));
        }
        let response = self.http.get(url, self.limits.max_file_bytes).await?;
        let content_hash = bytes_hash(&response.body);
        if let Some(expected) = expected.filter(|expected| *expected != content_hash) {
            return Err(SourceError::ChecksumMismatch {
                expected: expected.into(),
                actual: content_hash,
            });
        }
        let vendor = VendorTree::validate(
            vec![UntrustedEntry::file("content", response.body)],
            self.limits,
            request.installed_vendor_bytes,
        )?;
        self.finish(
            request,
            SourceType::Url,
            url.into(),
            response.final_url,
            None,
            None,
            None,
            response.etag,
            response.last_modified,
            vendor,
        )
    }

    async fn resolve_git(
        &self,
        request: ResolveRequest<'_>,
        source_type: SourceType,
        repository: &str,
        rev: &str,
        subpath: Option<&str>,
        requested: String,
    ) -> Result<ResolvedSource, SourceError> {
        if matches!(rev.to_ascii_lowercase().as_str(), "head" | "latest") && !request.allow_unpinned
        {
            return Err(SourceError::Unpinned(requested));
        }
        let result = self
            .git
            .fetch(repository, rev, subpath, self.limits)
            .await?;
        let vendor =
            VendorTree::validate(result.entries, self.limits, request.installed_vendor_bytes)?;
        self.finish(
            request,
            source_type,
            requested,
            format!("{repository}@{}", result.commit),
            Some(rev.into()),
            Some(result.commit),
            Some(result.tree_hash),
            None,
            None,
            vendor,
        )
    }

    async fn resolve_skill(
        &self,
        request: ResolveRequest<'_>,
        reference: &str,
        version: Option<&str>,
    ) -> Result<ResolvedSource, SourceError> {
        let rev = version
            .or(request.allow_unpinned.then_some("HEAD"))
            .ok_or_else(|| SourceError::Unpinned(reference.into()))?;
        let parts = reference.split('/').collect::<Vec<_>>();
        if parts.len() < 2
            || parts
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(SourceError::InvalidSkillsReference(reference.into()));
        }
        let repository = format!("https://github.com/{}/{}.git", parts[0], parts[1]);
        let subpath = (parts.len() > 2).then(|| parts[2..].join("/"));
        self.resolve_git(
            request,
            SourceType::SkillsSh,
            &repository,
            rev,
            subpath.as_deref(),
            format!("skills-sh:{reference}@{rev}"),
        )
        .await
    }

    async fn resolve_mcp(
        &self,
        request: ResolveRequest<'_>,
        reference: &str,
        version: Option<&str>,
    ) -> Result<ResolvedSource, SourceError> {
        let requested_version = version
            .or(request.allow_unpinned.then_some("latest"))
            .ok_or_else(|| SourceError::Unpinned(reference.into()))?;
        let mut url = url::Url::parse(&self.mcp_registry_base).map_err(SourceError::InvalidUrl)?;
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                SourceError::InvalidRegistryResponse("registry base cannot be a base URL".into())
            })?;
            segments.extend(["v0.1", "servers", reference, "versions", requested_version]);
        }
        let response = self
            .http
            .get(url.as_str(), self.limits.max_file_bytes)
            .await?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::InvalidRegistryResponse(error.to_string()))?;
        let resolved_version = value
            .pointer("/server/version")
            .and_then(Value::as_str)
            .ok_or_else(|| SourceError::InvalidRegistryResponse("missing server.version".into()))?
            .to_owned();
        let normalized = serde_json::to_vec_pretty(&value)
            .map_err(|error| SourceError::InvalidRegistryResponse(error.to_string()))?;
        let vendor = VendorTree::validate(
            vec![UntrustedEntry {
                path: "server.json".into(),
                kind: EntryKind::Regular,
                mode: 0o644,
                content: normalized,
            }],
            self.limits,
            request.installed_vendor_bytes,
        )?;
        self.finish(
            request,
            SourceType::McpRegistry,
            format!("mcp-registry:{reference}@{requested_version}"),
            response.final_url,
            Some(resolved_version),
            None,
            None,
            response.etag,
            response.last_modified,
            vendor,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        request: ResolveRequest<'_>,
        source_type: SourceType,
        requested_locator: String,
        resolved_locator: String,
        resolved_version: Option<String>,
        git_commit: Option<String>,
        tree_hash: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
        vendor: VendorTree,
    ) -> Result<ResolvedSource, SourceError> {
        if vendor.executable_content && !request.allow_executable_content {
            return Err(SourceError::ExecutableContentNotAllowed);
        }
        let lock = LockEntry {
            kind: request.kind,
            id: request.id.into(),
            source_type,
            requested_locator,
            resolved_locator,
            resolved_version,
            git_commit,
            tree_hash,
            etag,
            last_modified,
            vendor_path: format!(
                ".agentforge/vendor/{}/{}",
                request.kind.vendor_segment(),
                request.id
            ),
            sha256: vendor.sha256.clone(),
            executable_content: vendor.executable_content,
            file_count: u64::try_from(vendor.files.len()).unwrap_or(u64::MAX),
            total_bytes: vendor.total_bytes,
        };
        Ok(ResolvedSource::Remote {
            lock: Box::new(lock),
            vendor,
        })
    }
}

fn bytes_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
