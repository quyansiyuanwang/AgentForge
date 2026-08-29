use std::collections::BTreeMap;

use agentforge_core::{
    model::Target,
    resolver::{Capability, CapabilityDecision, RendererDescriptor},
};

use crate::{
    RenderError, RenderOutput, RenderRequest, Renderer,
    shared::{
        add_skills, artifact, capabilities, instructions, json_mcp, pretty_json,
        validate_artifacts, validate_json, validate_segment,
    },
};

const VERSION: &str = "generic@1";

pub struct GenericRenderer;

impl Renderer for GenericRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            target: Target::Generic,
            renderer_version: VERSION.into(),
            verified_at: "2026-08-29".into(),
            supported_agent_versions: "target-neutral Agent Skills and MCP representation".into(),
            capabilities: capabilities(vec![
                (Capability::Instructions, CapabilityDecision::Render),
                (Capability::Skills, CapabilityDecision::Render),
                (Capability::Mcp, CapabilityDecision::Render),
                (Capability::Subagents, CapabilityDecision::Render),
                (Capability::Permissions, CapabilityDecision::Render),
                (Capability::Sandbox, CapabilityDecision::Render),
                (Capability::ModelProfile, CapabilityDecision::Render),
                (Capability::Hooks, CapabilityDecision::Render),
            ]),
            output_paths: vec![
                "AGENTS.md".into(),
                ".agents/skills/**".into(),
                ".agentforge/generated/**".into(),
            ],
        }
    }

    fn render(&self, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
        let mut output = RenderOutput::default();
        output.artifacts.push(artifact(
            Target::Generic,
            VERSION,
            "AGENTS.md",
            instructions(request, "Agent Instructions"),
        ));
        add_skills(
            &mut output,
            request,
            Target::Generic,
            VERSION,
            ".agents/skills",
        )?;
        output.artifacts.push(artifact(
            Target::Generic,
            VERSION,
            ".agentforge/generated/mcp.json",
            pretty_json(&json_mcp(request)?)?,
        ));
        let settings = serde_yaml::to_string(&request.settings)
            .map_err(|error| RenderError::Serialization(error.to_string()))?;
        output.artifacts.push(artifact(
            Target::Generic,
            VERSION,
            ".agentforge/generated/settings.yaml",
            settings,
        ));
        let mut subagents = request.subagents.iter().collect::<Vec<_>>();
        subagents.sort_by(|a, b| a.id.cmp(&b.id));
        for subagent in subagents {
            validate_segment(&subagent.id)?;
            let front = serde_yaml::to_string(&BTreeMap::from([
                ("description", subagent.description.clone()),
                ("skills", subagent.skills.join(",")),
            ]))
            .map_err(|error| RenderError::Serialization(error.to_string()))?;
            let content = format!(
                "---\n{}---\n\n{}\n",
                front.trim_start_matches("---\n"),
                subagent.instructions.trim()
            );
            output.artifacts.push(artifact(
                Target::Generic,
                VERSION,
                format!(".agentforge/generated/subagents/{}.md", subagent.id),
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
        validate_json(artifacts, &[".agentforge/generated/mcp.json"])?;
        for artifact in artifacts
            .iter()
            .filter(|artifact| artifact.path == ".agentforge/generated/settings.yaml")
        {
            serde_yaml::from_slice::<serde_yaml::Value>(&artifact.content).map_err(|error| {
                RenderError::InvalidArtifact {
                    format: "YAML",
                    path: artifact.path.clone(),
                    message: error.to_string(),
                }
            })?;
        }
        Ok(())
    }
}
