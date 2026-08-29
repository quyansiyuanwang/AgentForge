use std::collections::{BTreeMap, BTreeSet};

use agentforge_core::{
    model::{Target, Transport},
    planning::DesiredArtifact,
};

use crate::{RenderError, RenderOutput, RenderRequest, ResolvedMcp};

pub fn artifact(
    target: Target,
    version: &str,
    path: impl Into<String>,
    content: impl Into<Vec<u8>>,
) -> DesiredArtifact {
    DesiredArtifact {
        path: path.into(),
        target,
        renderer_version: version.into(),
        content: content.into(),
    }
}

pub fn instructions(request: &RenderRequest, title: &str) -> String {
    let mut items = request.instructions.iter().collect::<Vec<_>>();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let mut output = format!("# {title}\n\nProject: `{}`\n", request.project_name);
    for item in items {
        output.push_str(&format!("\n## {}\n\n{}\n", item.id, item.content.trim()));
    }
    output
}

pub fn add_skills(
    output: &mut RenderOutput,
    request: &RenderRequest,
    target: Target,
    version: &str,
    root: &str,
) -> Result<(), RenderError> {
    let mut skills = request.skills.iter().collect::<Vec<_>>();
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    for skill in skills {
        validate_segment(&skill.id)?;
        if !skill
            .files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case("SKILL.md"))
        {
            return Err(RenderError::MissingSkillManifest(skill.id.clone()));
        }
        let mut files = skill.files.iter().collect::<Vec<_>>();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        for file in files {
            validate_relative(&file.path)?;
            output.artifacts.push(artifact(
                target,
                version,
                format!("{root}/{}/{}", skill.id, file.path.replace('\\', "/")),
                file.content.clone(),
            ));
        }
    }
    Ok(())
}

pub fn validate_relative(path: &str) -> Result<(), RenderError> {
    let normalized = path.replace('\\', "/");
    if path.is_empty()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(RenderError::UnsafePath(path.into()));
    }
    Ok(())
}

pub fn validate_segment(value: &str) -> Result<(), RenderError> {
    if value.is_empty()
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
        })
    {
        return Err(RenderError::UnsafePath(value.into()));
    }
    Ok(())
}

pub fn validate_artifacts(artifacts: &[DesiredArtifact]) -> Result<(), RenderError> {
    let mut paths = BTreeSet::new();
    for artifact in artifacts {
        validate_relative(&artifact.path)?;
        let key = artifact.path.replace('\\', "/").to_ascii_lowercase();
        if !paths.insert(key) {
            return Err(RenderError::DuplicatePath(artifact.path.clone()));
        }
    }
    Ok(())
}

pub fn env_placeholder(value: &str) -> Option<&str> {
    value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
}

pub fn validate_mcp_env(server: &ResolvedMcp) -> Result<(), RenderError> {
    let declared = server
        .env
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let values = match &server.transport {
        Transport::Stdio { env, .. } => env.values().collect::<Vec<_>>(),
        Transport::StreamableHttp { headers, .. } | Transport::Sse { headers, .. } => {
            headers.values().collect::<Vec<_>>()
        }
    };
    for value in values {
        if let Some(variable) = env_placeholder(value)
            && !declared.contains(variable)
        {
            return Err(RenderError::UndeclaredMcpEnvironment {
                server: server.id.clone(),
                variable: variable.into(),
            });
        }
    }
    Ok(())
}

pub fn json_mcp(request: &RenderRequest) -> Result<serde_json::Value, RenderError> {
    let mut servers = serde_json::Map::new();
    let mut mcp = request.mcp.iter().collect::<Vec<_>>();
    mcp.sort_by(|a, b| a.id.cmp(&b.id));
    for server in mcp {
        validate_mcp_env(server)?;
        servers.insert(server.id.clone(), transport_json(&server.transport));
    }
    Ok(serde_json::json!({ "mcpServers": servers }))
}

fn transport_json(transport: &Transport) -> serde_json::Value {
    match transport {
        Transport::Stdio { command, args, env } => {
            serde_json::json!({ "type": "stdio", "command": command, "args": args, "env": env })
        }
        Transport::StreamableHttp { url, headers } => {
            serde_json::json!({ "type": "http", "url": url, "headers": headers })
        }
        Transport::Sse { url, headers } => {
            serde_json::json!({ "type": "sse", "url": url, "headers": headers })
        }
    }
}

pub fn pretty_json(value: &serde_json::Value) -> Result<Vec<u8>, RenderError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| RenderError::Serialization(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn validate_json(artifacts: &[DesiredArtifact], paths: &[&str]) -> Result<(), RenderError> {
    for artifact in artifacts
        .iter()
        .filter(|artifact| paths.contains(&artifact.path.as_str()))
    {
        serde_json::from_slice::<serde_json::Value>(&artifact.content).map_err(|error| {
            RenderError::InvalidArtifact {
                format: "JSON",
                path: artifact.path.clone(),
                message: error.to_string(),
            }
        })?;
    }
    Ok(())
}

pub fn capabilities(
    entries: Vec<(
        agentforge_core::resolver::Capability,
        agentforge_core::resolver::CapabilityDecision,
    )>,
) -> BTreeMap<agentforge_core::resolver::Capability, agentforge_core::resolver::CapabilityDecision>
{
    entries.into_iter().collect()
}
