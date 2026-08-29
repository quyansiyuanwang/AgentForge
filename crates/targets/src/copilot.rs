use agentforge_core::{
    model::{McpSurface, Target},
    resolver::{Capability, CapabilityDecision, RendererDescriptor},
};

use crate::{
    ManualAction, RenderError, RenderOutput, RenderRequest, Renderer,
    shared::{
        add_skills, artifact, capabilities, instructions, json_mcp, pretty_json,
        validate_artifacts, validate_json, validate_segment,
    },
};

const VERSION: &str = "copilot@1";

pub struct CopilotRenderer;

impl Renderer for CopilotRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            target: Target::Copilot,
            renderer_version: VERSION.into(),
            verified_at: "2026-08-29".into(),
            supported_agent_versions: "GitHub Copilot repository formats verified 2026-08-29"
                .into(),
            capabilities: capabilities(vec![
                (Capability::Instructions, CapabilityDecision::Render),
                (Capability::Skills, CapabilityDecision::Render),
                (Capability::Mcp, CapabilityDecision::Render),
                (Capability::Subagents, CapabilityDecision::Render),
                (
                    Capability::Permissions,
                    CapabilityDecision::SkipWarning {
                        reason: "no general repository permission contract".into(),
                    },
                ),
                (
                    Capability::Sandbox,
                    CapabilityDecision::SkipWarning {
                        reason: "managed by Copilot execution surface".into(),
                    },
                ),
                (
                    Capability::ModelProfile,
                    CapabilityDecision::SkipWarning {
                        reason: "managed by Copilot execution surface".into(),
                    },
                ),
                (
                    Capability::Hooks,
                    CapabilityDecision::SkipWarning {
                        reason: "no equivalent repository hook contract".into(),
                    },
                ),
            ]),
            output_paths: vec![
                ".github/copilot-instructions.md".into(),
                ".github/skills/**".into(),
                ".github/mcp.json".into(),
                ".github/agents/**".into(),
            ],
        }
    }

    fn render(&self, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
        let mut output = RenderOutput::default();
        output.artifacts.push(artifact(
            Target::Copilot,
            VERSION,
            ".github/copilot-instructions.md",
            instructions(request, "GitHub Copilot Instructions"),
        ));
        add_skills(
            &mut output,
            request,
            Target::Copilot,
            VERSION,
            ".github/skills",
        )?;
        let mcp = json_mcp(request)?;
        if request
            .extensions
            .copilot
            .as_ref()
            .and_then(|value| value.mcp_surface)
            == Some(McpSurface::CloudManual)
        {
            let (content, required_env) = cloud_mcp(&mcp, request)?;
            output.manual_actions.push(ManualAction {
                target: Target::Copilot,
                id: "configure-cloud-mcp".into(),
                summary: "Configure MCP in GitHub repository settings".into(),
                content,
                required_env,
            });
        } else {
            output.artifacts.push(artifact(
                Target::Copilot,
                VERSION,
                ".github/mcp.json",
                pretty_json(&mcp)?,
            ));
        }
        let mut subagents = request.subagents.iter().collect::<Vec<_>>();
        subagents.sort_by(|a, b| a.id.cmp(&b.id));
        for subagent in subagents {
            validate_segment(&subagent.id)?;
            let front =
                serde_json::json!({ "name": subagent.id, "description": subagent.description });
            let yaml = serde_yaml::to_string(&front)
                .map_err(|error| RenderError::Serialization(error.to_string()))?;
            let content = format!(
                "---\n{}---\n\n{}\n",
                yaml.trim_start_matches("---\n"),
                subagent.instructions.trim()
            );
            output.artifacts.push(artifact(
                Target::Copilot,
                VERSION,
                format!(".github/agents/{}.agent.md", subagent.id),
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
        validate_json(artifacts, &[".github/mcp.json"])
    }
}

fn cloud_mcp(
    value: &serde_json::Value,
    request: &RenderRequest,
) -> Result<(String, Vec<String>), RenderError> {
    let mut transformed = value.clone();
    replace_placeholders(&mut transformed);
    let mut required = request
        .mcp
        .iter()
        .flat_map(|server| server.env.iter())
        .map(|name| format!("COPILOT_MCP_{name}"))
        .collect::<Vec<_>>();
    required.sort();
    required.dedup();
    let content = String::from_utf8(pretty_json(&transformed)?).expect("JSON is UTF-8");
    Ok((content, required))
}

fn replace_placeholders(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(variable) = text
                .strip_prefix("${")
                .and_then(|value| value.strip_suffix('}'))
            {
                *text = format!("${{COPILOT_MCP_{variable}}}");
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_placeholders(item);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values_mut() {
                replace_placeholders(value);
            }
        }
        _ => {}
    }
}
