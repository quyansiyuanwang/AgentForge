//! Pure renderers for supported AgentForge targets.

mod claude;
mod codex;
mod copilot;
mod generic;
mod shared;

use std::collections::BTreeMap;

use agentforge_core::{
    model::{Extensions, Settings, Target, Transport},
    planning::DesiredArtifact,
    resolver::RendererDescriptor,
};
use thiserror::Error;

pub use claude::ClaudeRenderer;
pub use codex::CodexRenderer;
pub use copilot::CopilotRenderer;
pub use generic::GenericRenderer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    pub project_name: String,
    pub instructions: Vec<ResolvedInstruction>,
    pub skills: Vec<ResolvedSkill>,
    pub mcp: Vec<ResolvedMcp>,
    pub subagents: Vec<ResolvedSubagent>,
    pub settings: Settings,
    pub extensions: Extensions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInstruction {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSkill {
    pub id: String,
    pub files: Vec<ResolvedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcp {
    pub id: String,
    pub transport: Transport,
    pub env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSubagent {
    pub id: String,
    pub description: String,
    pub instructions: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualAction {
    pub target: Target,
    pub id: String,
    pub summary: String,
    pub content: String,
    pub required_env: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderOutput {
    pub artifacts: Vec<DesiredArtifact>,
    pub manual_actions: Vec<ManualAction>,
}

pub trait Renderer: Send + Sync {
    fn descriptor(&self) -> RendererDescriptor;
    fn render(&self, request: &RenderRequest) -> Result<RenderOutput, RenderError>;
    fn validate(&self, artifacts: &[DesiredArtifact]) -> Result<(), RenderError>;
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("duplicate rendered path: {0}")]
    DuplicatePath(String),
    #[error("unsafe rendered path: {0}")]
    UnsafePath(String),
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("invalid {format} artifact {path}: {message}")]
    InvalidArtifact {
        format: &'static str,
        path: String,
        message: String,
    },
    #[error("MCP environment placeholder {variable} is not declared by {server}")]
    UndeclaredMcpEnvironment { server: String, variable: String },
    #[error("skill {0} does not contain SKILL.md")]
    MissingSkillManifest(String),
    #[error("target cannot map MCP environment key {key} to a different variable {variable}")]
    UnsupportedEnvironmentMapping { key: String, variable: String },
}

pub fn render_all(
    renderers: &[&dyn Renderer],
    request: &RenderRequest,
) -> Result<RenderOutput, RenderError> {
    let mut combined = RenderOutput::default();
    let mut paths = BTreeMap::new();
    for renderer in renderers {
        let output = renderer.render(request)?;
        renderer.validate(&output.artifacts)?;
        for artifact in output.artifacts {
            let key = artifact.path.replace('\\', "/").to_ascii_lowercase();
            if paths.insert(key, artifact.path.clone()).is_some() {
                return Err(RenderError::DuplicatePath(artifact.path));
            }
            combined.artifacts.push(artifact);
        }
        combined.manual_actions.extend(output.manual_actions);
    }
    combined.artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    combined
        .manual_actions
        .sort_by(|a, b| (a.target, &a.id).cmp(&(b.target, &b.id)));
    Ok(combined)
}
