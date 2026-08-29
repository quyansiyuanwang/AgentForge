use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Diagnostic, DiagnosticCode, Severity, model::Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Instructions,
    Skills,
    Mcp,
    Subagents,
    Permissions,
    Sandbox,
    ModelProfile,
    Hooks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum CapabilityDecision {
    Render,
    Degrade { reason: String },
    SkipWarning { reason: String },
    Manual { reason: String },
    Error { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererDescriptor {
    pub target: Target,
    pub renderer_version: String,
    pub verified_at: String,
    pub supported_agent_versions: String,
    pub capabilities: BTreeMap<Capability, CapabilityDecision>,
    pub output_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCapability {
    pub target: Target,
    pub capability: Capability,
    pub decision: CapabilityDecision,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityResolution {
    pub decisions: Vec<ResolvedCapability>,
    pub diagnostics: Vec<Diagnostic>,
}

impl CapabilityResolution {
    pub fn blocks_apply(&self, strict: bool) -> bool {
        self.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                || (strict && diagnostic.severity == Severity::Warning)
        })
    }
}

pub fn resolve_capabilities(
    required: &[Capability],
    renderers: &[RendererDescriptor],
) -> CapabilityResolution {
    let mut resolution = CapabilityResolution::default();
    for renderer in renderers {
        for capability in required {
            let decision = renderer
                .capabilities
                .get(capability)
                .cloned()
                .unwrap_or_else(|| CapabilityDecision::SkipWarning {
                    reason: "renderer does not declare this capability".into(),
                });
            match &decision {
                CapabilityDecision::Render | CapabilityDecision::Degrade { .. } => {}
                CapabilityDecision::SkipWarning { reason } => resolution.diagnostics.push(
                    Diagnostic::warning(
                        DiagnosticCode::UnsupportedCapability,
                        format!(
                            "{} does not render {capability:?}: {reason}",
                            renderer.target.as_str()
                        ),
                    )
                    .for_target(renderer.target.as_str()),
                ),
                CapabilityDecision::Manual { reason } => resolution.diagnostics.push(
                    Diagnostic::warning(
                        DiagnosticCode::ManualActionRequired,
                        format!(
                            "{} requires a manual action for {capability:?}: {reason}",
                            renderer.target.as_str()
                        ),
                    )
                    .for_target(renderer.target.as_str()),
                ),
                CapabilityDecision::Error { reason } => resolution.diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::UnsupportedCapability,
                        format!(
                            "{} cannot support {capability:?}: {reason}",
                            renderer.target.as_str()
                        ),
                    )
                    .for_target(renderer.target.as_str()),
                ),
            }
            resolution.decisions.push(ResolvedCapability {
                target: renderer.target,
                capability: *capability,
                decision,
            });
        }
    }
    resolution
        .decisions
        .sort_by_key(|item| (item.target, item.capability));
    resolution.diagnostics.sort_by(|a, b| {
        a.code
            .as_str()
            .cmp(b.code.as_str())
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.message.cmp(&b.message))
    });
    resolution
}
