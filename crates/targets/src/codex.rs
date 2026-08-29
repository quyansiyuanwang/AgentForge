use std::collections::{BTreeMap, BTreeSet};

use agentforge_core::{
    model::{Permission, Sandbox, Target, Transport},
    resolver::{Capability, CapabilityDecision, RendererDescriptor},
};
use serde::Serialize;

use crate::{
    RenderError, RenderOutput, RenderRequest, Renderer,
    shared::{
        add_skills, artifact, capabilities, env_placeholder, instructions, validate_artifacts,
        validate_mcp_env, validate_segment,
    },
};

const VERSION: &str = "codex@1";

pub struct CodexRenderer;

#[derive(Serialize)]
struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_workspace_write: Option<WorkspaceWrite>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    mcp_servers: BTreeMap<String, McpConfig>,
}

#[derive(Serialize)]
struct WorkspaceWrite {
    network_access: bool,
}
#[derive(Serialize)]
struct AgentConfig {
    name: String,
    description: String,
    developer_instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    skills: Option<AgentSkills>,
}
#[derive(Serialize)]
struct AgentSkills {
    config: Vec<AgentSkillConfig>,
}
#[derive(Serialize)]
struct AgentSkillConfig {
    path: String,
    enabled: bool,
}
#[derive(Default, Serialize)]
struct McpConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    env_vars: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    http_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env_http_headers: BTreeMap<String, String>,
}

impl Renderer for CodexRenderer {
    fn descriptor(&self) -> RendererDescriptor {
        RendererDescriptor {
            target: Target::Codex,
            renderer_version: VERSION.into(),
            verified_at: "2026-08-29".into(),
            supported_agent_versions: "Codex repository configuration verified 2026-08-29".into(),
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
                        reason: "mapped through target profile/defaults".into(),
                    },
                ),
                (
                    Capability::Hooks,
                    CapabilityDecision::SkipWarning {
                        reason: "Codex has no equivalent repository hook contract".into(),
                    },
                ),
            ]),
            output_paths: vec![
                "AGENTS.md".into(),
                ".agents/skills/**".into(),
                ".codex/config.toml".into(),
                ".codex/agents/**".into(),
            ],
        }
    }

    fn render(&self, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
        let mut output = RenderOutput::default();
        output.artifacts.push(artifact(
            Target::Codex,
            VERSION,
            "AGENTS.md",
            instructions(request, "Codex Instructions"),
        ));
        add_skills(
            &mut output,
            request,
            Target::Codex,
            VERSION,
            ".agents/skills",
        )?;
        let extension = request.extensions.codex.as_ref();
        let mcp_servers = request
            .mcp
            .iter()
            .map(|server| Ok((server.id.clone(), codex_mcp(server)?)))
            .collect::<Result<BTreeMap<_, _>, RenderError>>()?;
        let sandbox_mode = request.settings.sandbox.map(|value| {
            match value {
                Sandbox::ReadOnly => "read-only",
                Sandbox::WorkspaceWrite => "workspace-write",
                Sandbox::FullAccess => "danger-full-access",
            }
            .into()
        });
        let config = Config {
            model: extension.and_then(|value| value.model.clone()),
            model_reasoning_effort: extension.and_then(|value| {
                value
                    .reasoning_effort
                    .map(|effort| format!("{effort:?}").to_ascii_lowercase())
            }),
            web_search: extension.and_then(|value| {
                value
                    .web_search
                    .map(|search| format!("{search:?}").to_ascii_lowercase())
            }),
            approval_policy: request.settings.permissions.shell.map(|value| {
                match value {
                    Permission::Ask => "on-request",
                    Permission::Deny | Permission::Allow => "never",
                }
                .into()
            }),
            sandbox_mode,
            sandbox_workspace_write: (request.settings.sandbox == Some(Sandbox::WorkspaceWrite))
                .then(|| WorkspaceWrite {
                    network_access: request.settings.permissions.network == Some(Permission::Allow),
                }),
            mcp_servers,
        };
        let config = toml::to_string_pretty(&config)
            .map_err(|error| RenderError::Serialization(error.to_string()))?;
        output.artifacts.push(artifact(
            Target::Codex,
            VERSION,
            ".codex/config.toml",
            config,
        ));
        let mut subagents = request.subagents.iter().collect::<Vec<_>>();
        subagents.sort_by(|a, b| a.id.cmp(&b.id));
        for subagent in subagents {
            validate_segment(&subagent.id)?;
            let value = AgentConfig {
                name: subagent.id.clone(),
                description: subagent.description.clone(),
                developer_instructions: subagent.instructions.clone(),
                skills: (!subagent.skills.is_empty()).then(|| AgentSkills {
                    config: subagent
                        .skills
                        .iter()
                        .map(|skill| AgentSkillConfig {
                            path: format!("../../.agents/skills/{skill}/SKILL.md"),
                            enabled: true,
                        })
                        .collect(),
                }),
            };
            output.artifacts.push(artifact(
                Target::Codex,
                VERSION,
                format!(".codex/agents/{}.toml", subagent.id),
                toml::to_string_pretty(&value)
                    .map_err(|error| RenderError::Serialization(error.to_string()))?,
            ));
        }
        Ok(output)
    }

    fn validate(
        &self,
        artifacts: &[agentforge_core::planning::DesiredArtifact],
    ) -> Result<(), RenderError> {
        validate_artifacts(artifacts)?;
        for artifact in artifacts
            .iter()
            .filter(|artifact| artifact.path.ends_with(".toml"))
        {
            let document = std::str::from_utf8(&artifact.content).map_err(|error| {
                RenderError::InvalidArtifact {
                    format: "TOML",
                    path: artifact.path.clone(),
                    message: error.to_string(),
                }
            })?;
            toml::from_str::<toml::Table>(document).map_err(|error| {
                RenderError::InvalidArtifact {
                    format: "TOML",
                    path: artifact.path.clone(),
                    message: error.to_string(),
                }
            })?;
        }
        Ok(())
    }
}

fn codex_mcp(server: &crate::ResolvedMcp) -> Result<McpConfig, RenderError> {
    validate_mcp_env(server)?;
    match &server.transport {
        Transport::Stdio { command, args, env } => {
            let mut variables = server.env.iter().cloned().collect::<BTreeSet<_>>();
            for (key, value) in env {
                if let Some(variable) = env_placeholder(value) {
                    if key != variable {
                        return Err(RenderError::UnsupportedEnvironmentMapping {
                            key: key.clone(),
                            variable: variable.into(),
                        });
                    }
                    variables.insert(variable.into());
                }
            }
            Ok(McpConfig {
                command: Some(command.clone()),
                args: args.clone(),
                env_vars: variables.into_iter().collect(),
                ..Default::default()
            })
        }
        Transport::StreamableHttp { url, headers } | Transport::Sse { url, headers } => {
            let mut config = McpConfig {
                url: Some(url.clone()),
                ..Default::default()
            };
            for (header, value) in headers {
                if let Some(variable) = env_placeholder(value) {
                    config
                        .env_http_headers
                        .insert(header.clone(), variable.into());
                } else {
                    config.http_headers.insert(header.clone(), value.clone());
                }
            }
            Ok(config)
        }
    }
}
