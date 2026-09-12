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
    allow_http_for_tests: bool,
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
            allow_http_for_tests: false,
        })
    }
}

#[async_trait]
impl HttpFetcher for ReqwestHttpFetcher {
    async fn get(&self, value: &str, max_bytes: u64) -> Result<HttpResponse, SourceError> {
        let mut current = secure_url(value, self.allow_http_for_tests)?;
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
                    self.allow_http_for_tests,
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

fn secure_url(value: &str, allow_http_for_tests: bool) -> Result<Url, SourceError> {
    let url = Url::parse(value).map_err(SourceError::InvalidUrl)?;
    if url.scheme() != "https" && !(allow_http_for_tests && url.scheme() == "http") {
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
pub struct ProcessGitFetcher {
    #[cfg(test)]
    allow_file_for_tests: bool,
}

#[async_trait]
impl GitFetcher for ProcessGitFetcher {
    async fn fetch(
        &self,
        repository: &str,
        rev: &str,
        subpath: Option<&str>,
        limits: VendorLimits,
    ) -> Result<GitResolution, SourceError> {
        #[cfg(test)]
        let repository_url = if self.allow_file_for_tests {
            let url = Url::parse(repository).map_err(SourceError::InvalidUrl)?;
            if !matches!(url.scheme(), "file" | "https") {
                return Err(SourceError::InsecureUrl(repository.into()));
            }
            url
        } else {
            secure_url(repository, false)?
        };
        #[cfg(not(test))]
        let repository_url = secure_url(repository, false)?;
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
    let subpath = subpath.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    if path == subpath {
        return path
            .rsplit('/')
            .next()
            .ok_or_else(|| SourceError::Git("invalid Git path".into()));
    }
    // Segment-aware: only a literal "subpath/" prefix counts, so sibling
    // directories that merely share a text prefix stay outside the subpath.
    match path.strip_prefix(subpath) {
        Some(rest) if rest.starts_with('/') && rest.len() > 1 => Ok(&rest[1..]),
        _ => Err(SourceError::Git(
            "Git returned a path outside subpath".into(),
        )),
    }
}

#[cfg(test)]
mod path_tests {
    use super::relative_git_path;

    #[test]
    fn subpath_matching_is_segment_aware() {
        assert_eq!(relative_git_path("SKILL.md", None).unwrap(), "SKILL.md");
        assert_eq!(
            relative_git_path("skills/testing/SKILL.md", Some("skills/testing")).unwrap(),
            "SKILL.md"
        );
        assert_eq!(
            relative_git_path("skills/testing", Some("skills/testing")).unwrap(),
            "testing"
        );
        assert_eq!(
            relative_git_path("skills/testing/", Some("skills/testing/")).unwrap(),
            "testing"
        );
        assert!(relative_git_path("skills-extra/foo.md", Some("skills")).is_err());
        assert!(relative_git_path("skillsfoo", Some("skills")).is_err());
        assert!(relative_git_path("other/SKILL.md", Some("skills/testing")).is_err());
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut request = [0u8; 2048];
                    let count = stream.read(&mut request).await.unwrap();
                    let line = String::from_utf8_lossy(&request[..count]);
                    let path = line.split_whitespace().nth(1).unwrap_or("/");
                    let response = match path {
                        "/start" => {
                            "HTTP/1.1 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\n\r\n"
                        }
                        "/loop" => {
                            "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n"
                        }
                        "/rate" => "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n",
                        "/large" => "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789",
                        "/slow" => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
                        }
                        _ => "HTTP/1.1 200 OK\r\nETag: fixture\r\nContent-Length: 2\r\n\r\nok",
                    };
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}"), task)
    }

    fn fetcher(timeout: Duration, max_redirects: usize) -> ReqwestHttpFetcher {
        ReqwestHttpFetcher {
            client: reqwest::Client::builder()
                .redirect(Policy::none())
                .timeout(timeout)
                .build()
                .unwrap(),
            max_redirects,
            allow_http_for_tests: true,
        }
    }

    #[tokio::test]
    async fn follows_redirect_and_locks_final_url() {
        let (base, task) = server().await;
        let result = fetcher(Duration::from_secs(1), 5)
            .get(&format!("{base}/start"), 10)
            .await
            .unwrap();
        assert_eq!(result.final_url, format!("{base}/ok"));
        assert_eq!(result.etag.as_deref(), Some("fixture"));
        task.abort();
    }

    #[tokio::test]
    async fn classifies_redirect_rate_size_and_timeout_failures() {
        let (base, task) = server().await;
        let normal = fetcher(Duration::from_secs(1), 1);
        assert!(matches!(
            normal.get(&format!("{base}/loop"), 10).await,
            Err(SourceError::TooManyRedirects(1))
        ));
        assert!(matches!(
            normal.get(&format!("{base}/rate"), 10).await,
            Err(SourceError::RateLimited)
        ));
        assert!(matches!(
            normal.get(&format!("{base}/large"), 5).await,
            Err(SourceError::DownloadTooLarge { .. })
        ));
        assert!(matches!(
            fetcher(Duration::from_millis(10), 1)
                .get(&format!("{base}/slow"), 10)
                .await,
            Err(SourceError::Http(_))
        ));
        task.abort();
    }

    #[test]
    fn production_url_policy_rejects_http_and_credentials() {
        assert!(matches!(
            secure_url("http://example.com", false),
            Err(SourceError::InsecureUrl(_))
        ));
        assert!(matches!(
            secure_url("https://user:secret@example.com", false),
            Err(SourceError::UrlCredentials)
        ));
    }

    fn run_git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    #[tokio::test]
    async fn process_git_fetcher_pins_commit_and_reads_blobs_without_checkout() {
        let repository = tempfile::tempdir().unwrap();
        run_git(repository.path(), &["init"]);
        run_git(
            repository.path(),
            &["config", "user.name", "AgentForge Test"],
        );
        run_git(
            repository.path(),
            &["config", "user.email", "test@agentforge.invalid"],
        );
        fs::create_dir_all(repository.path().join("skills/testing")).unwrap();
        fs::write(
            repository.path().join("skills/testing/SKILL.md"),
            b"fixture skill",
        )
        .unwrap();
        run_git(repository.path(), &["add", "."]);
        run_git(repository.path(), &["commit", "-m", "fixture"]);

        let url = Url::from_file_path(repository.path()).unwrap().to_string();
        let result = ProcessGitFetcher {
            allow_file_for_tests: true,
        }
        .fetch(
            &url,
            "HEAD",
            Some("skills/testing"),
            VendorLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.commit.len(), 40);
        assert!(!result.tree_hash.is_empty());
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].path, "SKILL.md");
        assert_eq!(result.entries[0].content, b"fixture skill");
    }
}
