use std::collections::BTreeMap;

use agentforge_core::{
    model::{ClaudePermissionMode, HookEvent, Sandbox, Target},
    resolver::{Capability, CapabilityDecision, RendererDescriptor},
};

use crate::{
    RenderError, RenderOutput, RenderRequest, Renderer,
    shared::{
        add_skills, artifact, capabilities, instructions, json_mcp, pretty_json,
        validate_artifacts, validate_json, validate_segment,
    },
};

const VERSION: &str = "claude@1";

pub struct ClaudeRenderer;

impl Renderer for ClaudeRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            target: Target::Claude,
            renderer_version: VERSION.into(),
            verified_at: "2026-08-29".into(),
            supported_agent_versions: "Claude Code repository formats verified 2026-08-29".into(),
            capabilities: capabilities(vec![
                (Capability::Instructions, CapabilityDecision::Render),
                (Capability::Skills, CapabilityDecision::Render),
                (Capability::Mcp, CapabilityDecision::Render),
                (Capability::Subagents, CapabilityDecision::Render),
                (Capability::Permissions, CapabilityDecision::Render),
                (Capability::Sandbox, CapabilityDecision::Render),
                (
                    Capability::ModelProfile,
                    CapabilityDecision::Degrade {
                        reason: "mapped through target model extension".into(),
                    },
                ),
                (Capability::Hooks, CapabilityDecision::Render),
            ]),
            output_paths: vec![
                "CLAUDE.md".into(),
                ".claude/skills/**".into(),
                ".mcp.json".into(),
                ".claude/agents/**".into(),
                ".claude/settings.json".into(),
            ],
        }
    }

    fn render(&self, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
        let mut output = RenderOutput::default();
        output.artifacts.push(artifact(
            Target::Claude,
            VERSION,
            "CLAUDE.md",
            instructions(request, "Claude Code Instructions"),
        ));
        add_skills(
            &mut output,
            request,
            Target::Claude,
            VERSION,
            ".claude/skills",
        )?;
        output.artifacts.push(artifact(
            Target::Claude,
            VERSION,
            ".mcp.json",
            pretty_json(&json_mcp(request)?)?,
        ));
        output.artifacts.push(artifact(
            Target::Claude,
            VERSION,
            ".claude/settings.json",
            pretty_json(&settings(request))?,
        ));
        let mut subagents = request.subagents.iter().collect::<Vec<_>>();
        subagents.sort_by(|a, b| a.id.cmp(&b.id));
        for subagent in subagents {
            validate_segment(&subagent.id)?;
            let front = serde_json::json!({ "name": subagent.id, "description": subagent.description, "skills": subagent.skills });
            let yaml = serde_yaml::to_string(&front)
                .map_err(|error| RenderError::Serialization(error.to_string()))?;
            let content = format!(
                "---\n{}---\n\n{}\n",
                yaml.trim_start_matches("---\n"),
                subagent.instructions.trim()
            );
            output.artifacts.push(artifact(
                Target::Claude,
                VERSION,
                format!(".claude/agents/{}.md", subagent.id),
                content,
            ));
        }
        Ok(output)
    }

    fn validate(
        &self,
        artifacts: &[agentforge_core::planning::DesiredArtifact],
    ) -> Result<(), RenderError> {
        validate_artifacts(artifacts)?;
        validate_json(artifacts, &[".mcp.json", ".claude/settings.json"])
    }
}

fn settings(request: &RenderRequest) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    let extension = request.extensions.claude.as_ref();
    if let Some(mode) = extension.and_then(|value| value.permission_mode) {
        root.insert(
            "permissions".into(),
            serde_json::json!({ "defaultMode": permission_mode(mode) }),
        );
    }
    if let Some(model) = extension.and_then(|value| value.model.clone()) {
        root.insert("model".into(), model.into());
    }
    if let Some(sandbox) = request.settings.sandbox {
        root.insert(
            "sandbox".into(),
            serde_json::json!({ "enabled": sandbox != Sandbox::FullAccess }),
        );
    }
    let mut hooks = BTreeMap::<&str, Vec<serde_json::Value>>::new();
    for hook in &request.settings.hooks {
        let event = match hook.event {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::PreTool => "PreToolUse",
            HookEvent::PostTool => "PostToolUse",
            HookEvent::SessionEnd => "SessionEnd",
        };
        let command = std::iter::once(hook.command.as_str())
            .chain(hook.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        hooks
            .entry(event)
            .or_default()
            .push(serde_json::json!({ "hooks": [{ "type": "command", "command": command }] }));
    }
    if !hooks.is_empty() {
        root.insert(
            "hooks".into(),
            serde_json::to_value(hooks).expect("JSON hooks are serializable"),
        );
    }
    serde_json::Value::Object(root)
}

fn permission_mode(mode: ClaudePermissionMode) -> &'static str {
    match mode {
        ClaudePermissionMode::Default => "default",
        ClaudePermissionMode::AcceptEdits => "acceptEdits",
        ClaudePermissionMode::Plan => "plan",
        ClaudePermissionMode::DontAsk => "dontAsk",
        ClaudePermissionMode::BypassPermissions => "bypassPermissions",
    }
}

fn shell_quote(value: &str) -> String {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'\\' | b'.' | b'_' | b'-')
    }) {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
