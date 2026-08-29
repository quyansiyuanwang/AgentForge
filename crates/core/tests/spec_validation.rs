use agentforge_core::{DiagnosticCode, SpecValidator};

const VALID_SPEC: &str = r#"
schemaVersion: "1"
project:
  name: my-app
targets: [codex, claude, copilot]
instructions:
  - id: project
    source: { type: local, path: ai/instructions/project.md }
skills:
  - id: testing
    source:
      type: skills-sh
      ref: owner/repository/testing
      version: "^1.2"
  - id: security-review
    source:
      type: git
      repository: https://github.com/example/ai-assets.git
      rev: v2.0.0
      subpath: skills/security-review
mcp:
  - id: github
    source:
      type: mcp-registry
      ref: io.github.github/github-mcp-server
      version: "1.4.0"
    env: [GITHUB_TOKEN]
  - id: internal-docs
    transport:
      type: streamable-http
      url: https://mcp.example.com/mcp
      headers:
        Authorization: "${DOCS_MCP_TOKEN}"
    env: [DOCS_MCP_TOKEN]
subagents:
  - id: reviewer
    description: Review correctness, security, and missing tests
    instructions: { type: local, path: ai/subagents/reviewer.md }
    skills: [testing, security-review]
settings:
  permissions: { shell: ask, network: ask }
  sandbox: workspace-write
  modelProfile: balanced
  hooks:
    - id: validate-after-edit
      event: post-tool
      command: scripts/validate-change
      args: []
      env: []
extensions:
  codex: { reasoningEffort: high, webSearch: cached }
  claude: { permissionMode: default }
  copilot: { mcpSurface: cli }
"#;

fn codes(yaml: &str) -> Vec<DiagnosticCode> {
    SpecValidator::new()
        .validate_yaml(yaml)
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn accepts_the_normative_project_spec() {
    let outcome = SpecValidator::new().validate_yaml(VALID_SPEC);
    assert!(outcome.is_valid(), "{:#?}", outcome.diagnostics);
    let spec = outcome.spec.expect("valid spec");
    assert_eq!(spec.project.name, "my-app");
    assert_eq!(spec.targets.len(), 3);
}

#[test]
fn default_optional_fields_round_trip_through_schema() {
    let spec = agentforge_core::model::ProjectSpec {
        schema_version: "1".into(),
        project: agentforge_core::model::Project {
            name: "empty".into(),
        },
        targets: vec![agentforge_core::model::Target::Generic],
        instructions: vec![],
        skills: vec![],
        mcp: vec![],
        subagents: vec![],
        settings: Default::default(),
        extensions: Default::default(),
    };
    let yaml = serde_yaml::to_string(&spec).unwrap();
    let validation = SpecValidator::new().validate_yaml(&yaml);
    assert!(
        validation.is_valid(),
        "{:#?}\n{yaml}",
        validation.diagnostics
    );
}

#[test]
fn rejects_unknown_fields_at_schema_boundary() {
    let yaml = VALID_SPEC.replace("  name: my-app", "  name: my-app\n  unknown: true");
    assert!(codes(&yaml).contains(&DiagnosticCode::SchemaViolation));
}

#[test]
fn rejects_generic_with_vendor_target() {
    let yaml = VALID_SPEC.replace(
        "targets: [codex, claude, copilot]",
        "targets: [generic, codex]",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::GenericTargetConflict));
}

#[test]
fn rejects_ids_duplicated_across_collections() {
    let yaml = VALID_SPEC.replace("  - id: testing", "  - id: project");
    assert!(codes(&yaml).contains(&DiagnosticCode::DuplicateId));
}

#[test]
fn rejects_unknown_skill_references() {
    let yaml = VALID_SPEC.replace("skills: [testing, security-review]", "skills: [missing]");
    assert!(codes(&yaml).contains(&DiagnosticCode::UnknownReference));
}

#[test]
fn rejects_extension_for_unselected_target() {
    let yaml = VALID_SPEC.replace(
        "targets: [codex, claude, copilot]",
        "targets: [codex, claude]",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::ExtensionTargetMismatch));
}

#[test]
fn rejects_registry_used_for_wrong_entity_kind() {
    let yaml = VALID_SPEC.replace(
        "type: skills-sh\n      ref: owner/repository/testing",
        "type: mcp-registry\n      ref: owner/repository/testing",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::InvalidSourceKind));
}

#[test]
fn rejects_cleartext_sensitive_header() {
    let yaml = VALID_SPEC.replace(
        "Authorization: \"${DOCS_MCP_TOKEN}\"",
        "Authorization: secret-value",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::InsecureSecretValue));
}

#[test]
fn rejects_windows_reserved_path_on_every_platform() {
    let yaml = VALID_SPEC.replace("ai/instructions/project.md", "ai/CON.txt");
    assert!(codes(&yaml).contains(&DiagnosticCode::UnsafePath));
}

#[test]
fn rejects_url_user_information() {
    let yaml = VALID_SPEC.replace(
        "https://mcp.example.com/mcp",
        "https://user:password@mcp.example.com/mcp",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::UrlContainsCredentials));
}

#[test]
fn rejects_sensitive_url_query_parameters() {
    let yaml = VALID_SPEC.replace(
        "https://mcp.example.com/mcp",
        "https://mcp.example.com/mcp?api_key=secret-value",
    );
    assert!(codes(&yaml).contains(&DiagnosticCode::UrlContainsCredentials));
}

#[test]
fn requires_mcp_environment_placeholders_to_be_declared() {
    let yaml = VALID_SPEC.replace("    env: [DOCS_MCP_TOKEN]", "    env: [OTHER_TOKEN]");
    assert!(codes(&yaml).contains(&DiagnosticCode::UndeclaredEnvironmentVariable));
}

#[test]
fn rejects_invalid_yaml_without_panicking() {
    let result = codes("project: [");
    assert_eq!(result, vec![DiagnosticCode::InvalidYaml]);
}

#[test]
fn diagnostics_serialize_with_stable_code() {
    let outcome = SpecValidator::new().validate_yaml("project: [");
    let value = serde_json::to_value(&outcome.diagnostics[0]).expect("serialize diagnostic");
    assert_eq!(value["code"], "AF1001");
    assert_eq!(value["severity"], "error");
}
