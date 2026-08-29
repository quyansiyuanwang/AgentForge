//! Offline ProjectSpec-to-plan compiler used by every AgentForge command.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use agentforge_core::{
    Diagnostic,
    model::{Content, Mcp, ProjectSpec, Source, Target, Transport},
    planning::{Manifest, ResolvedPlan, build_plan, content_hash},
    resolver::{Capability, CapabilityResolution, RendererDescriptor, resolve_capabilities},
    validation::SpecValidator,
};
use agentforge_sources::{ContentKind, LockEntry, LockFile, VendorLimits, verify_vendor_offline};
use agentforge_targets::{
    ClaudeRenderer, CodexRenderer, CopilotRenderer, GenericRenderer, ManualAction, RenderError,
    RenderRequest, Renderer, ResolvedFile, ResolvedInstruction, ResolvedMcp, ResolvedSkill,
    ResolvedSubagent,
};
use thiserror::Error;

const SPEC_PATH: &str = ".agentforge/project.yaml";
const LOCK_PATH: &str = ".agentforge/lock.yaml";
const MANIFEST_PATH: &str = ".agentforge/manifest.json";

#[derive(Debug)]
pub struct Compilation {
    pub spec: ProjectSpec,
    pub plan: ResolvedPlan,
    pub capabilities: CapabilityResolution,
    pub descriptors: Vec<RendererDescriptor>,
    pub manual_actions: Vec<ManualAction>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Compilation {
    pub fn blocks_apply(&self, strict: bool) -> bool {
        self.plan.has_conflicts()
            || self.capabilities.blocks_apply(strict)
            || (strict && !self.manual_actions.is_empty())
    }
}

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("required file is missing: {0}")]
    MissingFile(String),
    #[error("I/O failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("ProjectSpec is invalid")]
    InvalidSpec(Vec<Diagnostic>),
    #[error("lock file is invalid: {0}")]
    InvalidLock(serde_yaml::Error),
    #[error("manifest is invalid: {0}")]
    InvalidManifest(serde_json::Error),
    #[error("vendor verification failed: {0}")]
    Vendor(#[from] agentforge_sources::SourceError),
    #[error("missing lock entry for {kind:?}/{id}")]
    MissingLock { kind: ContentKind, id: String },
    #[error("local source escapes the repository: {0}")]
    EscapingLocalSource(String),
    #[error("source {kind:?}/{id} has no usable content")]
    EmptySource { kind: ContentKind, id: String },
    #[error("source {kind:?}/{id} must resolve to one text file")]
    AmbiguousTextSource { kind: ContentKind, id: String },
    #[error("source content is not UTF-8: {0}")]
    NonUtf8(String),
    #[error("MCP metadata for {0} is unsupported or invalid")]
    InvalidMcp(String),
    #[error("render failed: {0}")]
    Render(#[from] RenderError),
    #[error("planning failed: {0}")]
    Planning(#[from] agentforge_core::planning::PlanningError),
}

pub struct ProjectCompiler {
    root: PathBuf,
}

impl ProjectCompiler {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn compile(&self) -> Result<Compilation, CompileError> {
        let spec_bytes = read_required(&self.root.join(SPEC_PATH))?;
        let spec_text = std::str::from_utf8(&spec_bytes)
            .map_err(|_| CompileError::NonUtf8(SPEC_PATH.into()))?;
        let outcome = SpecValidator::new().validate_yaml(spec_text);
        let spec = outcome
            .spec
            .ok_or(CompileError::InvalidSpec(outcome.diagnostics))?;
        let lock_bytes = read_optional(&self.root.join(LOCK_PATH))?.unwrap_or_default();
        let lock = if lock_bytes.is_empty() {
            LockFile::new(format!("agentforge {}", env!("CARGO_PKG_VERSION")), vec![])?
        } else {
            serde_yaml::from_slice(&lock_bytes).map_err(CompileError::InvalidLock)?
        };
        verify_vendor_offline(&self.root, &lock, VendorLimits::default())?;
        let previous = read_optional(&self.root.join(MANIFEST_PATH))?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(CompileError::InvalidManifest))
            .transpose()?;
        let mut artifacts = Vec::new();
        let mut manual_actions = Vec::new();
        let mut descriptors = Vec::new();
        for target in &spec.targets {
            let renderer = renderer(*target);
            let request = self.render_request(&spec, &lock, *target)?;
            let output = renderer.render(&request)?;
            renderer.validate(&output.artifacts)?;
            artifacts.extend(output.artifacts);
            manual_actions.extend(output.manual_actions);
            descriptors.push(renderer.descriptor());
        }
        let required = required_capabilities(&spec);
        let capabilities = resolve_capabilities(&required, &descriptors);
        let current = read_current(
            &self.root,
            artifacts.iter().map(|artifact| artifact.path.as_str()),
            previous.as_ref(),
        )?;
        let plan = build_plan(
            artifacts,
            &current,
            previous.as_ref(),
            &format!("agentforge {}", env!("CARGO_PKG_VERSION")),
            &content_hash(&spec_bytes),
            &content_hash(&lock_bytes),
        )?;
        let mut diagnostics = capabilities.diagnostics.clone();
        diagnostics.extend(plan.diagnostics.clone());
        Ok(Compilation {
            spec,
            plan,
            capabilities,
            descriptors,
            manual_actions,
            diagnostics,
        })
    }

    fn render_request(
        &self,
        spec: &ProjectSpec,
        lock: &LockFile,
        target: Target,
    ) -> Result<RenderRequest, CompileError> {
        let instructions = spec
            .instructions
            .iter()
            .filter(|item| item.applies_to.is_empty() || item.applies_to.contains(&target))
            .map(|item| self.instruction(item, lock))
            .collect::<Result<Vec<_>, _>>()?;
        let skills = spec
            .skills
            .iter()
            .filter(|item| item.applies_to.is_empty() || item.applies_to.contains(&target))
            .map(|item| self.skill(item, lock))
            .collect::<Result<Vec<_>, _>>()?;
        let available_skills = skills
            .iter()
            .map(|skill| skill.id.as_str())
            .collect::<BTreeSet<_>>();
        let mcp = spec
            .mcp
            .iter()
            .map(|item| self.mcp(item, lock))
            .collect::<Result<Vec<_>, _>>()?;
        let subagents = spec
            .subagents
            .iter()
            .map(|item| {
                Ok(ResolvedSubagent {
                    id: item.id.clone(),
                    description: item.description.clone(),
                    instructions: self.text_source(
                        ContentKind::Subagent,
                        &item.id,
                        &item.instructions,
                        lock,
                    )?,
                    skills: item
                        .skills
                        .iter()
                        .filter(|skill| available_skills.contains(skill.as_str()))
                        .cloned()
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        Ok(RenderRequest {
            project_name: spec.project.name.clone(),
            instructions,
            skills,
            mcp,
            subagents,
            settings: spec.settings.clone(),
            extensions: spec.extensions.clone(),
        })
    }

    fn instruction(
        &self,
        item: &Content,
        lock: &LockFile,
    ) -> Result<ResolvedInstruction, CompileError> {
        Ok(ResolvedInstruction {
            id: item.id.clone(),
            content: self.text_source(ContentKind::Instruction, &item.id, &item.source, lock)?,
        })
    }
    fn skill(&self, item: &Content, lock: &LockFile) -> Result<ResolvedSkill, CompileError> {
        Ok(ResolvedSkill {
            id: item.id.clone(),
            files: self.files_source(ContentKind::Skill, &item.id, &item.source, lock)?,
        })
    }

    fn mcp(&self, item: &Mcp, lock: &LockFile) -> Result<ResolvedMcp, CompileError> {
        let transport = if let Some(transport) = &item.transport {
            transport.clone()
        } else {
            let source = item
                .source
                .as_ref()
                .ok_or_else(|| CompileError::InvalidMcp(item.id.clone()))?;
            let files = self.files_source(ContentKind::Mcp, &item.id, source, lock)?;
            parse_mcp(&item.id, &files)?
        };
        Ok(ResolvedMcp {
            id: item.id.clone(),
            transport,
            env: item.env.clone(),
        })
    }

    fn text_source(
        &self,
        kind: ContentKind,
        id: &str,
        source: &Source,
        lock: &LockFile,
    ) -> Result<String, CompileError> {
        let files = self.files_source(kind, id, source, lock)?;
        let selected = if files.len() == 1 {
            files.first()
        } else {
            files.iter().find(|file| {
                ["INSTRUCTIONS.md", "AGENTS.md", "CLAUDE.md"].contains(&file.path.as_str())
            })
        };
        let file = selected.ok_or_else(|| {
            if files.is_empty() {
                CompileError::EmptySource {
                    kind,
                    id: id.into(),
                }
            } else {
                CompileError::AmbiguousTextSource {
                    kind,
                    id: id.into(),
                }
            }
        })?;
        String::from_utf8(file.content.clone())
            .map_err(|_| CompileError::NonUtf8(format!("{kind:?}/{id}/{}", file.path)))
    }

    fn files_source(
        &self,
        kind: ContentKind,
        id: &str,
        source: &Source,
        lock: &LockFile,
    ) -> Result<Vec<ResolvedFile>, CompileError> {
        match source {
            Source::Local { path } => read_local(&self.root, path),
            _ => {
                let entry = lock
                    .sources
                    .iter()
                    .find(|entry| entry.kind == kind && entry.id == id)
                    .ok_or_else(|| CompileError::MissingLock {
                        kind,
                        id: id.into(),
                    })?;
                read_vendor(&self.root, entry)
            }
        }
    }
}

fn renderer(target: Target) -> Box<dyn Renderer> {
    match target {
        Target::Generic => Box::new(GenericRenderer),
        Target::Codex => Box::new(CodexRenderer),
        Target::Claude => Box::new(ClaudeRenderer),
        Target::Copilot => Box::new(CopilotRenderer),
    }
}

fn required_capabilities(spec: &ProjectSpec) -> Vec<Capability> {
    let mut result = vec![Capability::Instructions];
    if !spec.skills.is_empty() {
        result.push(Capability::Skills);
    }
    if !spec.mcp.is_empty() {
        result.push(Capability::Mcp);
    }
    if !spec.subagents.is_empty() {
        result.push(Capability::Subagents);
    }
    if spec.settings.permissions != Default::default() {
        result.push(Capability::Permissions);
    }
    if spec.settings.sandbox.is_some() {
        result.push(Capability::Sandbox);
    }
    if spec.settings.model_profile.is_some() {
        result.push(Capability::ModelProfile);
    }
    if !spec.settings.hooks.is_empty() {
        result.push(Capability::Hooks);
    }
    result
}

fn read_required(path: &Path) -> Result<Vec<u8>, CompileError> {
    fs::read(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CompileError::MissingFile(path.display().to_string())
        } else {
            io_error(path, source)
        }
    })
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, CompileError> {
    match fs::read(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}
fn io_error(path: &Path, source: io::Error) -> CompileError {
    CompileError::Io {
        path: path.into(),
        source,
    }
}

fn read_local(root: &Path, relative: &str) -> Result<Vec<ResolvedFile>, CompileError> {
    let root = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
    let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    let canonical = fs::canonicalize(&path).map_err(|source| io_error(&path, source))?;
    if !canonical.starts_with(&root) {
        return Err(CompileError::EscapingLocalSource(relative.into()));
    }
    if canonical.is_file() {
        return Ok(vec![ResolvedFile {
            path: canonical.file_name().unwrap().to_string_lossy().into(),
            content: fs::read(&canonical).map_err(|source| io_error(&canonical, source))?,
        }]);
    }
    read_tree(&canonical)
}

fn read_vendor(root: &Path, entry: &LockEntry) -> Result<Vec<ResolvedFile>, CompileError> {
    read_tree(
        &root.join(
            entry
                .vendor_path
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        ),
    )
}

fn read_tree(root: &Path) -> Result<Vec<ResolvedFile>, CompileError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for child in fs::read_dir(&directory).map_err(|source| io_error(&directory, source))? {
            let path = child.map_err(|source| io_error(&directory, source))?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            if metadata.file_type().is_symlink() {
                return Err(CompileError::EscapingLocalSource(
                    path.display().to_string(),
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(ResolvedFile {
                    path: path
                        .strip_prefix(root)
                        .expect("walked path is below root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                    content: fs::read(&path).map_err(|source| io_error(&path, source))?,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn parse_mcp(id: &str, files: &[ResolvedFile]) -> Result<Transport, CompileError> {
    let file = files
        .iter()
        .find(|file| {
            file.path == "server.json" || file.path == "mcp.json" || file.path == "content"
        })
        .ok_or_else(|| CompileError::InvalidMcp(id.into()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&file.content).map_err(|_| CompileError::InvalidMcp(id.into()))?;
    if let Ok(transport) = serde_json::from_value::<Transport>(value.clone()) {
        return Ok(transport);
    }
    let server = value.get("server").unwrap_or(&value);
    if let Some(remote) = server
        .get("remotes")
        .and_then(|value| value.as_array())
        .and_then(|values| values.first())
    {
        let kind = remote
            .get("type")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CompileError::InvalidMcp(id.into()))?;
        let url = remote
            .get("url")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CompileError::InvalidMcp(id.into()))?
            .to_owned();
        let headers = registry_headers(remote.get("headers"));
        return match kind {
            "streamable-http" => Ok(Transport::StreamableHttp { url, headers }),
            "sse" => Ok(Transport::Sse { url, headers }),
            _ => Err(CompileError::InvalidMcp(id.into())),
        };
    }
    let package = server
        .get("packages")
        .and_then(|value| value.as_array())
        .and_then(|values| values.first())
        .ok_or_else(|| CompileError::InvalidMcp(id.into()))?;
    let command = package
        .get("runtimeHint")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CompileError::InvalidMcp(id.into()))?
        .to_owned();
    let args = package
        .get("runtimeArguments")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .chain(
            package
                .get("packageArguments")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten(),
        )
        .filter_map(|value| {
            value
                .get("value")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect();
    let env = package
        .get("environmentVariables")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("name").and_then(|value| value.as_str()))
        .map(|name| (name.into(), format!("${{{name}}}")))
        .collect();
    Ok(Transport::Stdio { command, args, env })
}

fn registry_headers(value: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    value
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|header| {
            let name = header.get("name")?.as_str()?;
            let value = header
                .get("value")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("${{{name}}}"));
            Some((name.into(), value))
        })
        .collect()
}

fn read_current<'a>(
    root: &Path,
    desired: impl Iterator<Item = &'a str>,
    previous: Option<&Manifest>,
) -> Result<BTreeMap<String, Vec<u8>>, CompileError> {
    let paths = desired
        .map(str::to_owned)
        .chain(previous.into_iter().flat_map(|manifest| {
            manifest
                .artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
        }))
        .collect::<BTreeSet<_>>();
    let mut current = BTreeMap::new();
    for path in paths {
        let absolute = root.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
        match fs::read(&absolute) {
            Ok(bytes) => {
                current.insert(path, bytes);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&absolute, source)),
        }
    }
    Ok(current)
}
