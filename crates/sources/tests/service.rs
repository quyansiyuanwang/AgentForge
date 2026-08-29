use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use agentforge_core::model::Source;
use agentforge_sources::{
    ContentKind, ResolveRequest, ResolvedSource, SourceError, SourceService, SourceType,
    UntrustedEntry, VendorLimits,
    adapters::{GitFetcher, GitResolution, HttpFetcher, HttpResponse},
};
use async_trait::async_trait;

#[derive(Clone)]
struct Http {
    body: Vec<u8>,
    final_url: String,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl HttpFetcher for Http {
    async fn get(&self, _: &str, _: u64) -> Result<HttpResponse, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(HttpResponse {
            final_url: self.final_url.clone(),
            etag: Some("etag".into()),
            last_modified: None,
            body: self.body.clone(),
        })
    }
}
struct Git;
#[async_trait]
impl GitFetcher for Git {
    async fn fetch(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: VendorLimits,
    ) -> Result<GitResolution, SourceError> {
        Ok(GitResolution {
            commit: "1".repeat(40),
            tree_hash: "tree".into(),
            entries: vec![UntrustedEntry::file("SKILL.md", b"content")],
        })
    }
}
fn http(body: &[u8], url: &str) -> (Http, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Http {
            body: body.into(),
            final_url: url.into(),
            calls: calls.clone(),
        },
        calls,
    )
}
fn request<'a>(source: &'a Source) -> ResolveRequest<'a> {
    ResolveRequest {
        kind: ContentKind::Skill,
        id: "testing",
        source,
        allow_unpinned: false,
        allow_executable_content: false,
        installed_vendor_bytes: 0,
    }
}

#[tokio::test]
async fn local_is_network_free_and_url_locks_final_location() {
    let local = Source::Local {
        path: "ai/test.md".into(),
    };
    let (client, calls) = http(b"unused", "https://example.com");
    assert!(matches!(
        SourceService::new(client, Git)
            .resolve(request(&local))
            .await
            .unwrap(),
        ResolvedSource::Local { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let remote = Source::Url {
        url: "https://example.com/start".into(),
        sha256: Some(
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into(),
        ),
    };
    let (client, _) = http(b"hello", "https://cdn.example.com/final");
    let ResolvedSource::Remote { lock, .. } = SourceService::new(client, Git)
        .resolve(request(&remote))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(lock.resolved_locator, "https://cdn.example.com/final");
    assert_eq!(lock.etag.as_deref(), Some("etag"));
}

#[tokio::test]
async fn checksum_mismatch_is_rejected() {
    let source = Source::Url {
        url: "https://example.com".into(),
        sha256: Some(format!("sha256:{}", "0".repeat(64))),
    };
    let (client, _) = http(b"hello", "https://example.com");
    assert!(matches!(
        SourceService::new(client, Git)
            .resolve(request(&source))
            .await,
        Err(SourceError::ChecksumMismatch { .. })
    ));
}

#[tokio::test]
async fn skills_require_pin_then_lock_commit() {
    let source = Source::SkillsSh {
        r#ref: "owner/repo/testing".into(),
        version: None,
    };
    let (client, _) = http(b"", "https://example.com");
    assert!(matches!(
        SourceService::new(client, Git)
            .resolve(request(&source))
            .await,
        Err(SourceError::Unpinned(_))
    ));
    let (client, _) = http(b"", "https://example.com");
    let mut input = request(&source);
    input.allow_unpinned = true;
    let ResolvedSource::Remote { lock, .. } = SourceService::new(client, Git)
        .resolve(input)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(lock.source_type, SourceType::SkillsSh);
    assert_eq!(
        lock.git_commit.as_deref(),
        Some("1111111111111111111111111111111111111111")
    );
}

#[tokio::test]
async fn mcp_registry_locks_actual_version() {
    let source = Source::McpRegistry {
        r#ref: "io.example/server".into(),
        version: Some("latest".into()),
    };
    let (client, _) = http(
        br#"{"server":{"version":"1.2.3"},"_meta":{}}"#,
        "https://registry.example/resolved",
    );
    let mut input = request(&source);
    input.kind = ContentKind::Mcp;
    let ResolvedSource::Remote { lock, vendor } = SourceService::new(client, Git)
        .resolve(input)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(lock.resolved_version.as_deref(), Some("1.2.3"));
    assert_eq!(vendor.files[0].path, "server.json");
}
