use std::collections::BTreeMap;

use agentforge_core::model::{
    ClaudeExtension, ClaudePermissionMode, CodexExtension, CopilotExtension, Extensions, Hook,
    HookEvent, McpSurface, Permission, Permissions, ReasoningEffort, Sandbox, Settings, Transport,
    WebSearch,
};
use agentforge_targets::{
    ClaudeRenderer, CodexRenderer, CopilotRenderer, GenericRenderer, RenderOutput, RenderRequest,
    Renderer, ResolvedFile, ResolvedInstruction, ResolvedMcp, ResolvedSkill, ResolvedSubagent,
    render_all,
};

fn fixture() -> RenderRequest {
    RenderRequest {
        project_name: "fixture-app".into(),
        instructions: vec![ResolvedInstruction {
            id: "project".into(),
            content: "Run tests before committing.".into(),
        }],
        skills: vec![ResolvedSkill {
            id: "testing".into(),
            files: vec![ResolvedFile {
                path: "SKILL.md".into(),
                content: b"---\nname: testing\n---\nTest carefully.\n".to_vec(),
            }],
        }],
        mcp: vec![ResolvedMcp {
            id: "github".into(),
            transport: Transport::Stdio {
                command: "github-mcp-server".into(),
                args: vec!["stdio".into()],
                env: BTreeMap::from([("GITHUB_TOKEN".into(), "${GITHUB_TOKEN}".into())]),
            },
            env: vec!["GITHUB_TOKEN".into()],
        }],
        subagents: vec![ResolvedSubagent {
            id: "reviewer".into(),
            description: "Review correctness and security".into(),
            instructions: "Inspect the diff and report findings.".into(),
            skills: vec!["testing".into()],
        }],
        settings: Settings {
            permissions: Permissions {
                shell: Some(Permission::Ask),
                network: Some(Permission::Allow),
            },
            sandbox: Some(Sandbox::WorkspaceWrite),
            model_profile: None,
            hooks: vec![Hook {
                id: "validate".into(),
                event: HookEvent::PostTool,
                command: "scripts/validate".into(),
                args: vec!["--all".into()],
                env: vec![],
            }],
        },
        extensions: Extensions {
            codex: Some(CodexExtension {
                model: Some("gpt-5.6-codex".into()),
                reasoning_effort: Some(ReasoningEffort::High),
                web_search: Some(WebSearch::Cached),
            }),
            claude: Some(ClaudeExtension {
                model: Some("sonnet".into()),
                permission_mode: Some(ClaudePermissionMode::Default),
            }),
            copilot: Some(CopilotExtension {
                mcp_surface: Some(McpSurface::Cli),
            }),
        },
    }
}

fn snapshot(output: &RenderOutput) -> String {
    let mut text = String::new();
    for artifact in &output.artifacts {
        text.push_str(&format!(
            "===== {} [{}] =====\n",
            artifact.path, artifact.renderer_version
        ));
        text.push_str(&String::from_utf8_lossy(&artifact.content));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    for action in &output.manual_actions {
        text.push_str(&format!(
            "===== MANUAL {} =====\n{}ENV: {}\n",
            action.id,
            action.content,
            action.required_env.join(",")
        ));
    }
    text
}

#[test]
fn generic_golden() {
    let renderer = GenericRenderer;
    let output = renderer.render(&fixture()).unwrap();
    renderer.validate(&output.artifacts).unwrap();
    insta::assert_snapshot!("generic", snapshot(&output));
}

#[test]
fn codex_golden() {
    let renderer = CodexRenderer;
    let output = renderer.render(&fixture()).unwrap();
    renderer.validate(&output.artifacts).unwrap();
    insta::assert_snapshot!("codex", snapshot(&output));
}

#[test]
fn claude_golden() {
    let renderer = ClaudeRenderer;
    let output = renderer.render(&fixture()).unwrap();
    renderer.validate(&output.artifacts).unwrap();
    insta::assert_snapshot!("claude", snapshot(&output));
}

#[test]
fn copilot_cli_golden() {
    let renderer = CopilotRenderer;
    let output = renderer.render(&fixture()).unwrap();
    renderer.validate(&output.artifacts).unwrap();
    insta::assert_snapshot!("copilot_cli", snapshot(&output));
}

#[test]
fn copilot_cloud_is_manual_and_prefixes_environment_names() {
    let mut request = fixture();
    request.extensions.copilot.as_mut().unwrap().mcp_surface = Some(McpSurface::CloudManual);
    let output = CopilotRenderer.render(&request).unwrap();
    assert!(
        !output
            .artifacts
            .iter()
            .any(|artifact| artifact.path == ".github/mcp.json")
    );
    assert_eq!(
        output.manual_actions[0].required_env,
        ["COPILOT_MCP_GITHUB_TOKEN"]
    );
    assert!(
        output.manual_actions[0]
            .content
            .contains("${COPILOT_MCP_GITHUB_TOKEN}")
    );
    insta::assert_snapshot!("copilot_cloud", snapshot(&output));
}

#[test]
fn vendor_renderers_compose_without_path_conflicts() {
    let output = render_all(
        &[&CodexRenderer, &ClaudeRenderer, &CopilotRenderer],
        &fixture(),
    )
    .unwrap();
    assert_eq!(output.artifacts.len(), 13);
    let conflict = render_all(&[&GenericRenderer, &CodexRenderer], &fixture());
    assert!(conflict.is_err());
}

#[test]
fn descriptors_bind_capabilities_to_renderer_versions() {
    for renderer in [
        &GenericRenderer as &dyn Renderer,
        &CodexRenderer,
        &ClaudeRenderer,
        &CopilotRenderer,
    ] {
        let descriptor = renderer.descriptor();
        assert!(descriptor.renderer_version.ends_with("@1"));
        assert_eq!(descriptor.verified_at, "2026-08-29");
        assert_eq!(descriptor.capabilities.len(), 8);
        assert!(!descriptor.output_paths.is_empty());
    }
}
