use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use crate::{
    diagnostic::{Diagnostic, DiagnosticCode},
    model::{ProjectSpec, Source, Target, Transport},
};

const PROJECT_SCHEMA: &str = include_str!("../../../schemas/project.schema.json");

#[derive(Debug, Clone)]
pub struct ValidationOutcome {
    pub spec: Option<ProjectSpec>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationOutcome {
    pub fn is_valid(&self) -> bool {
        self.spec.is_some() && self.diagnostics.is_empty()
    }
}

#[derive(Debug)]
pub struct SpecValidator {
    validator: jsonschema::Validator,
}

impl SpecValidator {
    /// Builds a validator around the embedded ProjectSpec schema. The JSON
    /// Schema is compiled once here; [`Self::validate_value`] can then be
    /// called repeatedly without recompiling it.
    pub fn new() -> Self {
        let schema = serde_json::from_str(PROJECT_SCHEMA)
            .expect("embedded ProjectSpec schema must be valid JSON");
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&schema)
            .expect("embedded ProjectSpec schema must compile");
        Self { validator }
    }

    /// Parses and validates a YAML document; see [`Self::validate_value`].
    pub fn validate_yaml(&self, yaml: &str) -> ValidationOutcome {
        let value = match serde_yaml::from_str::<serde_json::Value>(yaml) {
            Ok(value) => value,
            Err(error) => {
                return ValidationOutcome {
                    spec: None,
                    diagnostics: vec![Diagnostic::error(
                        DiagnosticCode::InvalidYaml,
                        format!("invalid YAML: {error}"),
                    )],
                };
            }
        };
        self.validate_value(&value)
    }

    /// Validates a parsed document against the JSON Schema and the semantic
    /// rules (duplicate ids, unknown references, credential-free URLs, safe
    /// paths). On success the deserialized [`ProjectSpec`] is returned.
    pub fn validate_value(&self, value: &serde_json::Value) -> ValidationOutcome {
        let mut diagnostics = self
            .validator
            .iter_errors(value)
            .map(|error| {
                let path = error.instance_path().to_string();
                let diagnostic =
                    Diagnostic::error(DiagnosticCode::SchemaViolation, error.to_string());
                if path.is_empty() {
                    diagnostic
                } else {
                    diagnostic.at_path(path)
                }
            })
            .collect::<Vec<_>>();

        if !diagnostics.is_empty() {
            sort_diagnostics(&mut diagnostics);
            return ValidationOutcome {
                spec: None,
                diagnostics,
            };
        }

        let spec = match serde_json::from_value::<ProjectSpec>(value.clone()) {
            Ok(spec) => spec,
            Err(error) => {
                return ValidationOutcome {
                    spec: None,
                    diagnostics: vec![Diagnostic::error(
                        DiagnosticCode::SchemaViolation,
                        format!("schema/model mismatch: {error}"),
                    )],
                };
            }
        };

        diagnostics.extend(validate_semantics(&spec));
        sort_diagnostics(&mut diagnostics);
        let valid = diagnostics.is_empty();
        ValidationOutcome {
            spec: valid.then_some(spec),
            diagnostics,
        }
    }
}

impl Default for SpecValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_semantics(spec: &ProjectSpec) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let selected = spec.targets.iter().copied().collect::<BTreeSet<_>>();

    if selected.contains(&Target::Generic) && selected.len() != 1 {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GenericTargetConflict,
                "target 'generic' must be selected alone",
            )
            .at_path("/targets")
            .with_remediation("remove generic or remove all vendor targets"),
        );
    }

    let mut ids = BTreeMap::<&str, String>::new();
    for (kind, id, path) in all_ids(spec) {
        if let Some(first) = ids.insert(id, format!("{kind}:{path}")) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::DuplicateId,
                    format!("id '{id}' is already used by {first}"),
                )
                .at_path(path),
            );
        }
    }

    let skills = spec
        .skills
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    for (index, subagent) in spec.subagents.iter().enumerate() {
        for (skill_index, skill) in subagent.skills.iter().enumerate() {
            if !skills.contains(skill.as_str()) {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::UnknownReference,
                        format!(
                            "subagent '{}' references unknown skill '{skill}'",
                            subagent.id
                        ),
                    )
                    .at_path(format!("/subagents/{index}/skills/{skill_index}")),
                );
            }
        }
    }

    for (collection, items) in [
        ("instructions", &spec.instructions),
        ("skills", &spec.skills),
    ] {
        for (index, item) in items.iter().enumerate() {
            validate_content_source(&item.source, collection, index, &mut diagnostics);
            for (target_index, target) in item.applies_to.iter().enumerate() {
                if !selected.contains(target) {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::TargetNotSelected,
                            format!("appliesTo target '{}' is not selected", target.as_str()),
                        )
                        .at_path(format!("/{collection}/{index}/appliesTo/{target_index}"))
                        .for_target(target.as_str()),
                    );
                }
            }
        }
    }

    for (index, item) in spec.subagents.iter().enumerate() {
        validate_content_source(&item.instructions, "subagents", index, &mut diagnostics);
    }

    for (index, mcp) in spec.mcp.iter().enumerate() {
        if let Some(source) = &mcp.source {
            if matches!(source, Source::SkillsSh { .. }) {
                diagnostics.push(invalid_source("mcp", index, source));
            }
            validate_source_paths(source, &format!("/mcp/{index}/source"), &mut diagnostics);
            validate_source_url(source, &format!("/mcp/{index}/source"), &mut diagnostics);
        }
        if let Some(transport) = &mcp.transport {
            validate_transport(transport, &mcp.env, index, &mut diagnostics);
        }
    }

    validate_extensions(spec, &selected, &mut diagnostics);
    for (index, hook) in spec.settings.hooks.iter().enumerate() {
        validate_safe_path(
            &hook.command,
            &format!("/settings/hooks/{index}/command"),
            &mut diagnostics,
        );
    }
    diagnostics
}

fn all_ids(spec: &ProjectSpec) -> Vec<(&'static str, &str, String)> {
    let mut result = Vec::new();
    for (kind, ids) in [
        (
            "instruction",
            spec.instructions
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "skill",
            spec.skills
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "mcp",
            spec.mcp.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ),
        (
            "subagent",
            spec.subagents
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "hook",
            spec.settings
                .hooks
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
        ),
    ] {
        for (index, id) in ids.into_iter().enumerate() {
            let collection = match kind {
                "instruction" => "instructions",
                "skill" => "skills",
                "hook" => "settings/hooks",
                other => other,
            };
            result.push((kind, id, format!("/{collection}/{index}/id")));
        }
    }
    result
}

fn validate_content_source(
    source: &Source,
    collection: &str,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if matches!(source, Source::McpRegistry { .. }) {
        diagnostics.push(invalid_source(collection, index, source));
    }
    validate_source_paths(
        source,
        &format!("/{collection}/{index}/source"),
        diagnostics,
    );
    validate_source_url(
        source,
        &format!("/{collection}/{index}/source"),
        diagnostics,
    );
}

fn invalid_source(collection: &str, index: usize, source: &Source) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvalidSourceKind,
        format!(
            "source type '{}' is not allowed for {collection}",
            source.kind()
        ),
    )
    .at_path(format!("/{collection}/{index}/source/type"))
}

fn validate_source_paths(source: &Source, base: &str, diagnostics: &mut Vec<Diagnostic>) {
    for path in source.local_paths() {
        validate_safe_path(path, base, diagnostics);
    }
}

fn validate_source_url(source: &Source, base: &str, diagnostics: &mut Vec<Diagnostic>) {
    let (url, field) = match source {
        Source::Url { url, .. } => (url.as_str(), "url"),
        Source::Git { repository, .. } => (repository.as_str(), "repository"),
        _ => return,
    };
    validate_url_has_no_credentials(url, &format!("{base}/{field}"), diagnostics);
}

fn validate_url_has_no_credentials(value: &str, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Ok(url) = url::Url::parse(value) else {
        return;
    };
    let sensitive_query = url.query_pairs().any(|(key, _)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "token"
                | "access_token"
                | "refresh_token"
                | "api_key"
                | "apikey"
                | "password"
                | "secret"
                | "credential"
        )
    });
    if !url.username().is_empty() || url.password().is_some() || sensitive_query {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::UrlContainsCredentials,
                "URL must not contain user information, credentials, or sensitive query parameters",
            )
            .at_path(path)
            .with_remediation("remove credentials and use declared environment variables"),
        );
    }
}

fn validate_safe_path(path: &str, diagnostic_path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let parsed = Path::new(path);
    let has_unsafe_component = parsed.components().any(|part| {
        matches!(
            part,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    let has_empty_or_dot = path
        .split(['/', '\\'])
        .any(|part| part.is_empty() || part == ".");
    let has_reserved = path.split(['/', '\\']).any(is_windows_reserved_name);
    if has_unsafe_component || has_empty_or_dot || has_reserved {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::UnsafePath,
                format!("unsafe repository-relative path '{path}'"),
            )
            .at_path(diagnostic_path)
            .with_remediation("use a normalized relative path without reserved Windows names"),
        );
    }
}

fn is_windows_reserved_name(component: &str) -> bool {
    let stem = component
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or("");
    matches!(
        stem.to_ascii_uppercase().as_str(),
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

fn validate_transport(
    transport: &Transport,
    declared_env: &[String],
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let declared_env = declared_env
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let base = format!("/mcp/{index}/transport");
    if let Transport::Stdio { env, .. } = transport {
        for (name, value) in env {
            validate_declared_placeholder(
                value,
                &declared_env,
                &format!("{base}/env/{name}"),
                diagnostics,
            );
        }
        return;
    }
    let headers = match transport {
        Transport::StreamableHttp { url, headers } | Transport::Sse { url, headers } => {
            validate_url_has_no_credentials(url, &format!("{base}/url"), diagnostics);
            headers
        }
        Transport::Stdio { .. } => unreachable!("stdio returned above"),
    };
    for (name, value) in headers {
        if is_sensitive_header(name) && !is_env_placeholder(value) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::InsecureSecretValue,
                    format!("sensitive MCP header '{name}' must use an environment placeholder"),
                )
                .at_path(format!("/mcp/{index}/transport/headers/{name}"))
                .with_remediation(
                    "replace the value with ${ENV_NAME} and declare ENV_NAME in mcp.env",
                ),
            );
        }
        if is_env_placeholder(value) {
            validate_declared_placeholder(
                value,
                &declared_env,
                &format!("{base}/headers/{name}"),
                diagnostics,
            );
        }
    }
}

fn validate_declared_placeholder(
    value: &str,
    declared_env: &BTreeSet<&str>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(name) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) else {
        return;
    };
    if !declared_env.contains(name) {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::UndeclaredEnvironmentVariable,
                format!("environment variable '{name}' is used but not declared in mcp.env"),
            )
            .at_path(path)
            .with_remediation(format!("add {name} to the MCP entry's env list")),
        );
    }
}

fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) || name.contains("token")
        || name.contains("secret")
        || name.contains("api-key")
}

fn is_env_placeholder(value: &str) -> bool {
    value
        .strip_prefix("${")
        .and_then(|v| v.strip_suffix('}'))
        .is_some_and(|name| {
            !name.is_empty()
                && name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_uppercase()
                        || (index > 0 && byte.is_ascii_digit())
                })
        })
}

fn validate_extensions(
    spec: &ProjectSpec,
    selected: &BTreeSet<Target>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (target, present, path) in [
        (
            Target::Codex,
            spec.extensions.codex.is_some(),
            "/extensions/codex",
        ),
        (
            Target::Claude,
            spec.extensions.claude.is_some(),
            "/extensions/claude",
        ),
        (
            Target::Copilot,
            spec.extensions.copilot.is_some(),
            "/extensions/copilot",
        ),
    ] {
        if present && !selected.contains(&target) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::ExtensionTargetMismatch,
                    format!(
                        "extension '{}' requires its target to be selected",
                        target.as_str()
                    ),
                )
                .at_path(path)
                .for_target(target.as_str()),
            );
        }
    }
}

fn sort_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.code
            .as_str()
            .cmp(right.code.as_str())
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.message.cmp(&right.message))
    });
}
