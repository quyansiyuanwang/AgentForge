use std::{path::Path, process::Output, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{StatusCode, header, redirect::Policy};
use url::Url;

use crate::{EntryKind, SourceError, VendorLimits, security::UntrustedEntry};

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub final_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait HttpFetcher: Send + Sync {
    async fn get(&self, url: &str, max_bytes: u64) -> Result<HttpResponse, SourceError>;
}

pub struct ReqwestHttpFetcher {
    client: reqwest::Client,
    max_redirects: usize,
}

impl ReqwestHttpFetcher {
    pub fn new() -> Result<Self, SourceError> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("AgentForge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(SourceError::HttpClient)?;
        Ok(Self {
            client,
            max_redirects: 5,
        })
    }
}

#[async_trait]
impl HttpFetcher for ReqwestHttpFetcher {
    async fn get(&self, value: &str, max_bytes: u64) -> Result<HttpResponse, SourceError> {
        let mut current = secure_url(value)?;
        for redirects in 0..=self.max_redirects {
            let response = self
                .client
                .get(current.clone())
                .send()
                .await
                .map_err(SourceError::Http)?;
            if response.status().is_redirection() {
                if redirects == self.max_redirects {
                    return Err(SourceError::TooManyRedirects(self.max_redirects));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .ok_or(SourceError::RedirectWithoutLocation)?
                    .to_str()
                    .map_err(|_| SourceError::InvalidRedirect)?
                    .to_owned();
                current = secure_url(
                    current
                        .join(&location)
                        .map_err(SourceError::InvalidUrl)?
                        .as_str(),
                )?;
                continue;
            }
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                return Err(SourceError::RateLimited);
            }
            if !response.status().is_success() {
                return Err(SourceError::HttpStatus(response.status().as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|size| size > max_bytes)
            {
                return Err(SourceError::DownloadTooLarge {
                    size: response.content_length().unwrap_or_default(),
                    limit: max_bytes,
                });
            }
            let etag = header_value(&response, header::ETAG);
            let last_modified = header_value(&response, header::LAST_MODIFIED);
            let final_url = current.to_string();
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(SourceError::Http)?;
                let next = u64::try_from(body.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                if next > max_bytes {
                    return Err(SourceError::DownloadTooLarge {
                        size: next,
                        limit: max_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(HttpResponse {
                final_url,
                etag,
                last_modified,
                body,
            });
        }
        unreachable!("redirect loop always returns")
    }
}

fn header_value(response: &reqwest::Response, name: header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn secure_url(value: &str) -> Result<Url, SourceError> {
    let url = Url::parse(value).map_err(SourceError::InvalidUrl)?;
    if url.scheme() != "https" {
        return Err(SourceError::InsecureUrl(value.into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SourceError::UrlCredentials);
    }
    Ok(url)
}

#[derive(Debug, Clone)]
pub struct GitResolution {
    pub commit: String,
    pub tree_hash: String,
    pub entries: Vec<UntrustedEntry>,
}

#[async_trait]
pub trait GitFetcher: Send + Sync {
    async fn fetch(
        &self,
        repository: &str,
        rev: &str,
        subpath: Option<&str>,
        limits: VendorLimits,
    ) -> Result<GitResolution, SourceError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessGitFetcher;

#[async_trait]
impl GitFetcher for ProcessGitFetcher {
    async fn fetch(
        &self,
        repository: &str,
        rev: &str,
        subpath: Option<&str>,
        limits: VendorLimits,
    ) -> Result<GitResolution, SourceError> {
        let repository_url = secure_url(repository)?;
        let temporary = tempfile::tempdir().map_err(|error| SourceError::Git(error.to_string()))?;
        git(temporary.path(), ["init", "--bare", "."]).await?;
        git(
            temporary.path(),
            ["fetch", "--depth=1", "--", repository_url.as_str(), rev],
        )
        .await?;
        let commit = text(git(temporary.path(), ["rev-parse", "FETCH_HEAD^{commit}"]).await?)?;
        if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SourceError::Git(
                "resolved commit is not a full SHA-1".into(),
            ));
        }
        let tree_expression = subpath.map_or_else(
            || format!("{commit}^{{tree}}"),
            |path| format!("{commit}:{path}"),
        );
        let tree_hash = text(git(temporary.path(), ["rev-parse", &tree_expression]).await?)?;
        let mut arguments = vec!["ls-tree", "-rz", "-r", "--full-tree", commit.as_str(), "--"];
        if let Some(path) = subpath {
            arguments.push(path);
        }
        let listing = git(temporary.path(), arguments).await?.stdout;
        let mut entries = Vec::new();
        for record in listing
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let (metadata, path) = split_once(record, b'\t')
                .ok_or_else(|| SourceError::Git("invalid ls-tree record".into()))?;
            let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(SourceError::Git("invalid ls-tree metadata".into()));
            }
            let mode = utf8(fields[0])?;
            let object_type = utf8(fields[1])?;
            let object = utf8(fields[2])?;
            let full_path = utf8(path)?;
            let relative = relative_git_path(full_path, subpath)?;
            let kind = match (mode, object_type) {
                ("100644" | "100755", "blob") => EntryKind::Regular,
                ("120000", "blob") => EntryKind::Symlink,
                ("160000", "commit") => EntryKind::Device,
                _ => {
                    return Err(SourceError::Git(format!(
                        "unsupported Git tree entry {mode} {object_type}"
                    )));
                }
            };
            let content = if kind == EntryKind::Regular {
                let output = git(temporary.path(), ["cat-file", "blob", object])
                    .await?
                    .stdout;
                if u64::try_from(output.len()).unwrap_or(u64::MAX) > limits.max_file_bytes {
                    return Err(SourceError::FileTooLarge {
                        path: relative.into(),
                        size: u64::try_from(output.len()).unwrap_or(u64::MAX),
                        limit: limits.max_file_bytes,
                    });
                }
                output
            } else {
                Vec::new()
            };
            entries.push(UntrustedEntry {
                path: relative.into(),
                kind,
                mode: if mode == "100755" { 0o755 } else { 0o644 },
                content,
            });
            if entries.len() > limits.max_files {
                return Err(SourceError::TooManyFiles {
                    count: entries.len(),
                    limit: limits.max_files,
                });
            }
        }
        if entries.is_empty() {
            return Err(SourceError::Git("Git source resolved to no files".into()));
        }
        Ok(GitResolution {
            commit: commit.to_ascii_lowercase(),
            tree_hash,
            entries,
        })
    }
}

async fn git<I, S>(directory: &Path, arguments: I) -> Result<Output, SourceError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let future = tokio::process::Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            directory.join("agentforge-empty-gitconfig"),
        )
        .output();
    let output = tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .map_err(|_| SourceError::Git("Git command timed out".into()))?
        .map_err(|error| SourceError::Git(error.to_string()))?;
    if !output.status.success() {
        return Err(SourceError::Git(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(output)
}

fn text(output: Output) -> Result<String, SourceError> {
    Ok(utf8(&output.stdout)?.trim().to_owned())
}
fn utf8(bytes: &[u8]) -> Result<&str, SourceError> {
    std::str::from_utf8(bytes)
        .map_err(|_| SourceError::Git("Git returned a non-UTF-8 path or identifier".into()))
}
fn split_once(bytes: &[u8], needle: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|byte| *byte == needle)?;
    Some((&bytes[..index], &bytes[index + 1..]))
}

fn relative_git_path<'a>(path: &'a str, subpath: Option<&str>) -> Result<&'a str, SourceError> {
    let Some(subpath) = subpath else {
        return Ok(path);
    };
    if path == subpath {
        return path
            .rsplit('/')
            .next()
            .ok_or_else(|| SourceError::Git("invalid Git path".into()));
    }
    path.strip_prefix(subpath)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(|| SourceError::Git("Git returned a path outside subpath".into()))
}
