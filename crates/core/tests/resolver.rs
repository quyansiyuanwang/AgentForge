use std::collections::BTreeMap;

use agentforge_core::{
    model::Target,
    resolver::{Capability, CapabilityDecision, RendererDescriptor, resolve_capabilities},
};

fn renderer(decisions: Vec<(Capability, CapabilityDecision)>) -> RendererDescriptor {
    RendererDescriptor {
        target: Target::Copilot,
        renderer_version: "copilot@1".into(),
        verified_at: "2026-08-29".into(),
        supported_agent_versions: "verified official repository format".into(),
        capabilities: decisions.into_iter().collect::<BTreeMap<_, _>>(),
        output_paths: vec![".github/copilot-instructions.md".into()],
    }
}

#[test]
fn render_and_degrade_do_not_block() {
    let result = resolve_capabilities(
        &[Capability::Instructions, Capability::Permissions],
        &[renderer(vec![
            (Capability::Instructions, CapabilityDecision::Render),
            (
                Capability::Permissions,
                CapabilityDecision::Degrade {
                    reason: "repository fields only".into(),
                },
            ),
        ])],
    );
    assert!(result.diagnostics.is_empty());
    assert!(!result.blocks_apply(false));
    assert!(!result.blocks_apply(true));
}

#[test]
fn warning_and_manual_block_only_in_strict_mode() {
    for decision in [
        CapabilityDecision::SkipWarning {
            reason: "unsupported".into(),
        },
        CapabilityDecision::Manual {
            reason: "repository settings".into(),
        },
    ] {
        let result = resolve_capabilities(
            &[Capability::Mcp],
            &[renderer(vec![(Capability::Mcp, decision)])],
        );
        assert!(!result.blocks_apply(false));
        assert!(result.blocks_apply(true));
    }
}

#[test]
fn error_always_blocks_and_undeclared_capability_warns() {
    let error = resolve_capabilities(
        &[Capability::Hooks],
        &[renderer(vec![(
            Capability::Hooks,
            CapabilityDecision::Error {
                reason: "unsafe".into(),
            },
        )])],
    );
    assert!(error.blocks_apply(false));
    let undeclared = resolve_capabilities(&[Capability::Subagents], &[renderer(vec![])]);
    assert_eq!(undeclared.diagnostics.len(), 1);
    assert!(undeclared.blocks_apply(true));
}
