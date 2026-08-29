use std::{
    env,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use agentforge_application::ApplicationService;
use agentforge_compiler::{Compilation, CompileError, ProjectCompiler};
use agentforge_core::{
    Diagnostic, DiagnosticCode, Severity,
    filesystem::RealFileSystem,
    model::Target,
    planning::{ArtifactChange, ChangeKind},
    validation::SpecValidator,
};
use agentforge_detectors::{DetectionContext, DetectionEngine};
use agentforge_sources::adapters::{ProcessGitFetcher, ReqwestHttpFetcher};
use agentforge_sources::{ContentKind, LockFile, ResolveRequest, ResolvedSource, SourceService};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use similar::TextDiff;

#[derive(Parser)]
#[command(
    name = "agentforge",
    version,
    about = "Compile ProjectSpec into multi-agent repository configuration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    Detect(JsonArgs),
    Sync(SyncArgs),
    Diff(JsonArgs),
    Doctor(DoctorArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Skill {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    Subagent {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    Target {
        #[command(subcommand)]
        command: ResourceCommand,
    },
}

#[derive(Args)]
struct JsonArgs {
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct SyncArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    strict: bool,
}
#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct InitArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    non_interactive: bool,
    #[arg(long)]
    strict: bool,
    #[arg(long = "target")]
    targets: Vec<TargetArg>,
    #[arg(long = "skill")]
    skills: Vec<String>,
    #[arg(long = "mcp")]
    mcp: Vec<String>,
    #[arg(long)]
    allow_unpinned_source: bool,
    #[arg(long)]
    allow_executable_content: bool,
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum TargetArg {
    Generic,
    Codex,
    Claude,
    Copilot,
}

#[derive(Subcommand)]
enum ConfigCommand {
    Validate {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Show {
        #[arg(long)]
        json: bool,
    },
}
#[derive(Subcommand)]
enum ResourceCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        value: String,
        #[arg(long)]
        dry_run: bool,
    },
    Remove {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    Update {
        id: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Success,
    Warning,
    Failure,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    schema_version: &'static str,
    command: String,
    status: Status,
    data: Value,
    diagnostics: Vec<Diagnostic>,
}
struct Outcome {
    envelope: Envelope,
    human: String,
    exit: u8,
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(outcome) => {
            if outcome.json {
                println!(
                    "{}",
                    serde_json::to_string(&outcome.envelope).expect("envelope serializes")
                );
            } else {
                if !outcome.human.is_empty() {
                    println!("{}", outcome.human);
                }
                for diagnostic in &outcome.envelope.diagnostics {
                    eprintln!(
                        "{} {:?}: {}",
                        diagnostic.code.as_str(),
                        diagnostic.severity,
                        diagnostic.message
                    );
                }
            }
            ExitCode::from(outcome.exit)
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit)
        }
    }
}

struct CliFailure {
    message: String,
    exit: u8,
}
impl std::fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn run(cli: Cli) -> Result<Outcome, CliFailure> {
    let root = repository_root().map_err(runtime)?;
    match cli.command {
        Command::Detect(args) => detect(&root, args.json),
        Command::Sync(args) => sync(&root, args),
        Command::Diff(args) => diff(&root, args.json),
        Command::Doctor(args) => doctor(&root, args),
        Command::Config { command } => config(&root, command),
        Command::Skill { command } => resources(&root, "skill", command),
        Command::Mcp { command } => resources(&root, "mcp", command),
        Command::Subagent { command } => resources(&root, "subagent", command),
        Command::Target { command } => resources(&root, "target", command),
        Command::Init(args) => init_pending(args),
    }
}

fn detect(root: &Path, json_output: bool) -> Result<Outcome, CliFailure> {
    let report = DetectionEngine::with_builtins().detect(&DetectionContext {
        root,
        filesystem: &RealFileSystem,
    });
    let human = format!(
        "Languages: {}\nFrameworks: {}\nDatabases: {}\nPackage managers: {}\nTests: {}\nCI: {}\nTools: {}",
        join(&report.profile.languages),
        join(&report.profile.frameworks),
        join(&report.profile.databases),
        join(&report.profile.package_managers),
        join(&report.profile.tests),
        join(&report.profile.ci),
        join(&report.profile.tools)
    );
    Ok(outcome(
        "detect",
        report.profile,
        report.diagnostics,
        human,
        json_output,
        false,
    ))
}

fn sync(root: &Path, args: SyncArgs) -> Result<Outcome, CliFailure> {
    let compilation = compile(root)?;
    let changes = changes_json(&compilation);
    let preview = human_diff(&compilation);
    if compilation.blocks_apply(args.strict) {
        return Ok(outcome_value(
            "sync",
            changes,
            compilation.diagnostics,
            preview,
            false,
            true,
        ));
    }
    if !args.dry_run && compilation.plan.has_changes() {
        ApplicationService::new(root)
            .apply(&compilation.plan)
            .map_err(|error| CliFailure {
                message: error.to_string(),
                exit: 3,
            })?;
    }
    let human = if args.dry_run {
        preview
    } else if compilation.plan.has_changes() {
        format!("Applied changes\n{preview}")
    } else {
        "No changes".into()
    };
    Ok(outcome_value(
        "sync",
        changes,
        compilation.diagnostics,
        human,
        false,
        false,
    ))
}

fn diff(root: &Path, json_output: bool) -> Result<Outcome, CliFailure> {
    let compilation = compile(root)?;
    let failed = compilation.plan.has_conflicts();
    let human = if !compilation.plan.has_changes() && !failed {
        "Clean".into()
    } else {
        human_diff(&compilation)
    };
    Ok(outcome_value(
        "diff",
        changes_json(&compilation),
        compilation.diagnostics,
        human,
        json_output,
        failed,
    ))
}

fn doctor(root: &Path, args: DoctorArgs) -> Result<Outcome, CliFailure> {
    let mut compilation = compile(root)?;
    let mut checks = Vec::new();
    for variable in compilation
        .spec
        .mcp
        .iter()
        .flat_map(|server| server.env.iter())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let present = env::var_os(variable).is_some();
        checks.push(json!({"check":"environment", "name":variable, "healthy":present}));
        if !present {
            compilation.diagnostics.push(
                Diagnostic::warning(
                    DiagnosticCode::MissingEnvironmentVariable,
                    format!("environment variable {variable} is not set"),
                )
                .with_remediation(format!(
                    "set {variable} without storing its value in AgentForge files"
                )),
            );
        }
    }
    for target in &compilation.spec.targets {
        let (binary, name) = match target {
            Target::Generic => continue,
            Target::Codex => ("codex", "Codex"),
            Target::Claude => ("claude", "Claude Code"),
            Target::Copilot => ("copilot", "GitHub Copilot"),
        };
        let version = ProcessCommand::new(binary)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        checks.push(json!({"check":"agentVersion", "target":target.as_str(), "version":version}));
        if version.is_none() {
            compilation.diagnostics.push(
                Diagnostic::warning(
                    DiagnosticCode::AgentUnavailable,
                    format!("{name} executable was not found"),
                )
                .for_target(target.as_str()),
            );
        }
    }
    if compilation.spec.targets.contains(&Target::Codex) {
        compilation.diagnostics.push(
            Diagnostic::warning(
                DiagnosticCode::ProjectTrustRequired,
                "Codex project config loads only for trusted projects",
            )
            .for_target("codex"),
        );
    }
    for action in &compilation.manual_actions {
        compilation.diagnostics.push(
            Diagnostic::warning(DiagnosticCode::ManualActionRequired, action.summary.clone())
                .for_target(action.target.as_str()),
        );
    }
    let failed = compilation.plan.has_conflicts()
        || compilation
            .diagnostics
            .iter()
            .any(|item| item.severity == Severity::Error)
        || (args.strict
            && compilation
                .diagnostics
                .iter()
                .any(|item| item.severity == Severity::Warning));
    let status = if failed {
        "Unhealthy"
    } else if compilation.diagnostics.is_empty() {
        "Healthy"
    } else {
        "Healthy with warnings"
    };
    Ok(outcome_value(
        "doctor",
        json!({"health":status, "checks":checks, "changes":changes_json(&compilation)}),
        compilation.diagnostics,
        status.into(),
        args.json,
        failed,
    ))
}

fn config(root: &Path, command: ConfigCommand) -> Result<Outcome, CliFailure> {
    match command {
        ConfigCommand::Validate { path, json } => {
            let path = path.unwrap_or_else(|| root.join(".agentforge/project.yaml"));
            let text = std::fs::read_to_string(&path).map_err(runtime)?;
            let validation = SpecValidator::new().validate_yaml(&text);
            let failed = !validation.is_valid();
            Ok(outcome_value(
                "config validate",
                json!({"path":path, "valid":!failed}),
                validation.diagnostics,
                if failed {
                    "Invalid".into()
                } else {
                    "Valid".into()
                },
                json,
                failed,
            ))
        }
        ConfigCommand::Show { json } => {
            let compilation = compile(root)?;
            let data = serde_json::to_value(&compilation.spec).map_err(internal)?;
            let human = serde_yaml::to_string(&compilation.spec).map_err(internal)?;
            Ok(outcome_value(
                "config show",
                data,
                vec![],
                human,
                json,
                false,
            ))
        }
    }
}

fn resources(root: &Path, kind: &str, command: ResourceCommand) -> Result<Outcome, CliFailure> {
    if let ResourceCommand::Update { id, dry_run } = &command {
        return update_resources(root, kind, id.as_deref(), *dry_run);
    }
    let compilation = compile(root)?;
    match command {
        ResourceCommand::List { json } => {
            let data = match kind {
                "skill" => serde_json::to_value(&compilation.spec.skills),
                "mcp" => serde_json::to_value(&compilation.spec.mcp),
                "subagent" => serde_json::to_value(&compilation.spec.subagents),
                "target" => serde_json::to_value(&compilation.spec.targets),
                _ => unreachable!(),
            }
            .map_err(internal)?;
            let human = match data.as_array() {
                Some(values) if values.is_empty() => "None".into(),
                Some(values) => values
                    .iter()
                    .map(|value| {
                        value
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.as_str().unwrap_or("").into())
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => String::new(),
            };
            Ok(outcome_value(
                &format!("{kind} list"),
                data,
                vec![],
                human,
                json,
                false,
            ))
        }
        ResourceCommand::Add { value, dry_run } => {
            mutate_spec(root, &compilation.spec, kind, "add", &value, dry_run)
        }
        ResourceCommand::Remove { id, dry_run } => {
            mutate_spec(root, &compilation.spec, kind, "remove", &id, dry_run)
        }
        ResourceCommand::Update { .. } => unreachable!(),
    }
}

fn update_resources(
    root: &Path,
    kind: &str,
    selected: Option<&str>,
    dry_run: bool,
) -> Result<Outcome, CliFailure> {
    if !matches!(kind, "skill" | "mcp") {
        return Err(CliFailure {
            message: "only skill and mcp support remote update".into(),
            exit: 2,
        });
    }
    let spec_text =
        std::fs::read_to_string(root.join(".agentforge/project.yaml")).map_err(runtime)?;
    let spec = SpecValidator::new()
        .validate_yaml(&spec_text)
        .spec
        .ok_or_else(|| CliFailure {
            message: "ProjectSpec is invalid".into(),
            exit: 1,
        })?;
    let lock_path = root.join(".agentforge/lock.yaml");
    let existing = std::fs::read(&lock_path)
        .ok()
        .and_then(|bytes| serde_yaml::from_slice::<LockFile>(&bytes).ok())
        .unwrap_or_else(|| {
            LockFile::new(format!("agentforge {}", env!("CARGO_PKG_VERSION")), vec![])
                .expect("empty lock")
        });
    let runtime = tokio::runtime::Runtime::new().map_err(internal)?;
    let updated = runtime
        .block_on(async {
            let http = ReqwestHttpFetcher::new().map_err(|error| error.to_string())?;
            let service = SourceService::new(http, ProcessGitFetcher::default());
            let mut entries = existing.sources.clone();
            let mut changed = Vec::new();
            if kind == "skill" {
                for item in spec
                    .skills
                    .iter()
                    .filter(|item| selected.is_none_or(|id| id == item.id))
                {
                    if matches!(item.source, agentforge_core::model::Source::Local { .. }) {
                        continue;
                    }
                    if let ResolvedSource::Remote { lock, vendor } = service
                        .resolve(ResolveRequest {
                            kind: ContentKind::Skill,
                            id: &item.id,
                            source: &item.source,
                            allow_unpinned: false,
                            allow_executable_content: false,
                            installed_vendor_bytes: 0,
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        if !dry_run {
                            write_vendor(root, &lock.vendor_path, &vendor)?;
                        }
                        entries.retain(|entry| {
                            !(entry.kind == ContentKind::Skill && entry.id == item.id)
                        });
                        entries.push(*lock);
                        changed.push(item.id.clone());
                    }
                }
            } else {
                for item in spec
                    .mcp
                    .iter()
                    .filter_map(|item| item.source.as_ref().map(|source| (item, source)))
                    .filter(|(item, _)| selected.is_none_or(|id| id == item.id))
                {
                    if matches!(item.1, agentforge_core::model::Source::Local { .. }) {
                        continue;
                    }
                    if let ResolvedSource::Remote { lock, vendor } = service
                        .resolve(ResolveRequest {
                            kind: ContentKind::Mcp,
                            id: &item.0.id,
                            source: item.1,
                            allow_unpinned: false,
                            allow_executable_content: false,
                            installed_vendor_bytes: 0,
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        if !dry_run {
                            write_vendor(root, &lock.vendor_path, &vendor)?;
                        }
                        entries.retain(|entry| {
                            !(entry.kind == ContentKind::Mcp && entry.id == item.0.id)
                        });
                        entries.push(*lock);
                        changed.push(item.0.id.clone());
                    }
                }
            }
            if !dry_run {
                let lock =
                    LockFile::new(format!("agentforge {}", env!("CARGO_PKG_VERSION")), entries)
                        .map_err(|error| error.to_string())?;
                let yaml = lock.to_yaml().map_err(|error| error.to_string())?;
                atomic_write(&lock_path, yaml.as_bytes()).map_err(|error| error.to_string())?;
            }
            Ok::<_, String>(changed)
        })
        .map_err(|message| CliFailure { message, exit: 3 })?;
    Ok(outcome_value(
        &format!("{kind} update"),
        json!({"updated":updated,"dryRun":dry_run}),
        vec![],
        if dry_run {
            "Would refresh remote sources".into()
        } else {
            "Remote sources refreshed".into()
        },
        false,
        false,
    ))
}

fn write_vendor(
    root: &Path,
    vendor_path: &str,
    vendor: &agentforge_sources::VendorTree,
) -> Result<(), String> {
    let directory = root.join(vendor_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    for file in &vendor.files {
        let path = directory.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(path, &file.content).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn mutate_spec(
    root: &Path,
    current: &agentforge_core::model::ProjectSpec,
    kind: &str,
    operation: &str,
    value: &str,
    dry_run: bool,
) -> Result<Outcome, CliFailure> {
    use agentforge_core::model::{Content, Source};
    let mut spec = current.clone();
    let id = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .trim_end_matches(".md")
        .to_lowercase();
    match (kind, operation) {
        ("skill", "add") => spec.skills.push(Content {
            id,
            source: Source::Local { path: value.into() },
            applies_to: vec![],
        }),
        ("skill", "remove") => spec.skills.retain(|item| item.id != value),
        ("mcp", "add") => spec.mcp.push(agentforge_core::model::Mcp {
            id,
            source: Some(Source::Local { path: value.into() }),
            transport: None,
            env: vec![],
        }),
        ("mcp", "remove") => spec.mcp.retain(|item| item.id != value),
        ("subagent", "remove") => spec.subagents.retain(|item| item.id != value),
        ("subagent", "add") => spec.subagents.push(agentforge_core::model::Subagent {
            id,
            description: format!("AgentForge subagent from {value}"),
            instructions: Source::Local { path: value.into() },
            skills: vec![],
        }),
        ("target", "add") => {
            let target =
                serde_yaml::from_str::<agentforge_core::model::Target>(&format!("{value}\n"))
                    .map_err(internal)?;
            if !spec.targets.contains(&target) {
                spec.targets.push(target);
            }
        }
        ("target", "remove") => {
            spec.targets.retain(|target| target.as_str() != value);
            if spec.targets.is_empty() {
                return Err(CliFailure {
                    message: "at least one target is required".into(),
                    exit: 1,
                });
            }
        }
        (_, "update") => {
            return Err(CliFailure {
                message: "update requires an explicit source refresh command".into(),
                exit: 2,
            });
        }
        _ => {
            return Err(CliFailure {
                message: format!("unsupported {kind} {operation}"),
                exit: 2,
            });
        }
    }
    let validation =
        SpecValidator::new().validate_yaml(&serde_yaml::to_string(&spec).map_err(internal)?);
    if !validation.is_valid() {
        return Ok(outcome_value(
            &format!("{kind} {operation}"),
            json!({"valid":false}),
            validation.diagnostics,
            "Invalid".into(),
            false,
            true,
        ));
    }
    let path = root.join(".agentforge/project.yaml");
    let rendered = serde_yaml::to_string(&spec).map_err(internal)?;
    if !dry_run {
        atomic_write(&path, rendered.as_bytes()).map_err(runtime)?;
    }
    Ok(outcome_value(
        &format!("{kind} {operation}"),
        json!({"path":path,"dryRun":dry_run,"changed":true}),
        vec![],
        if dry_run {
            "Would update project spec".into()
        } else {
            "Updated project spec".into()
        },
        false,
        false,
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("agentforge"),
        std::process::id()
    ));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}
fn init_pending(args: InitArgs) -> Result<Outcome, CliFailure> {
    let root = repository_root().map_err(runtime)?;
    let path = root.join(".agentforge/project.yaml");
    if path.exists() {
        return Err(CliFailure {
            message: "project spec already exists; use sync or edit it explicitly".into(),
            exit: 1,
        });
    }
    if args.non_interactive && args.targets.is_empty() {
        return Err(CliFailure {
            message: "--non-interactive requires at least one --target".into(),
            exit: 2,
        });
    }
    let targets = if args.targets.is_empty() {
        vec![TargetArg::Generic]
    } else {
        args.targets
    };
    let mut target_values = targets
        .into_iter()
        .map(|target| match target {
            TargetArg::Generic => Target::Generic,
            TargetArg::Codex => Target::Codex,
            TargetArg::Claude => Target::Claude,
            TargetArg::Copilot => Target::Copilot,
        })
        .collect::<Vec<_>>();
    target_values.sort();
    target_values.dedup();
    if target_values.contains(&Target::Generic) && target_values.len() > 1 {
        return Err(CliFailure {
            message: "generic cannot be combined with vendor targets".into(),
            exit: 1,
        });
    }
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_owned();
    let skills = args
        .skills
        .iter()
        .map(|path| agentforge_core::model::Content {
            id: path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(path)
                .trim_end_matches(".md")
                .to_lowercase(),
            source: agentforge_core::model::Source::Local { path: path.clone() },
            applies_to: vec![],
        })
        .collect();
    let mcp = args
        .mcp
        .iter()
        .map(|path| agentforge_core::model::Mcp {
            id: path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(path)
                .to_lowercase(),
            source: Some(agentforge_core::model::Source::Local { path: path.clone() }),
            transport: None,
            env: vec![],
        })
        .collect();
    let spec = agentforge_core::model::ProjectSpec {
        schema_version: "agentforge/v0.1".into(),
        project: agentforge_core::model::Project { name },
        targets: target_values,
        instructions: vec![],
        skills,
        mcp,
        subagents: vec![],
        settings: Default::default(),
        extensions: Default::default(),
    };
    let rendered = serde_yaml::to_string(&spec).map_err(internal)?;
    let report = DetectionEngine::with_builtins().detect(&DetectionContext {
        root: &root,
        filesystem: &RealFileSystem,
    });
    if !args.dry_run {
        atomic_write(&path, rendered.as_bytes()).map_err(runtime)?;
    }
    Ok(outcome(
        "init",
        json!({"path":path,"dryRun":args.dry_run,"spec":spec,"facts":report.profile,"evidence":report.profile.evidence}),
        report.diagnostics,
        if args.dry_run {
            "Would initialize project".into()
        } else {
            "Initialized project".into()
        },
        false,
        false,
    ))
}

fn compile(root: &Path) -> Result<Compilation, CliFailure> {
    ProjectCompiler::new(root)
        .compile()
        .map_err(|error| match error {
            CompileError::InvalidSpec(diagnostics) => CliFailure {
                message: diagnostics
                    .iter()
                    .map(|item| format!("{}: {}", item.code.as_str(), item.message))
                    .collect::<Vec<_>>()
                    .join("\n"),
                exit: 1,
            },
            CompileError::MissingFile(_)
            | CompileError::MissingLock { .. }
            | CompileError::InvalidLock(_)
            | CompileError::InvalidManifest(_)
            | CompileError::Vendor(_)
            | CompileError::EmptySource { .. }
            | CompileError::AmbiguousTextSource { .. }
            | CompileError::InvalidMcp(_) => CliFailure {
                message: error.to_string(),
                exit: 1,
            },
            _ => CliFailure {
                message: error.to_string(),
                exit: 3,
            },
        })
}

fn outcome<T: Serialize>(
    command: &str,
    data: T,
    diagnostics: Vec<Diagnostic>,
    human: String,
    json_output: bool,
    failed: bool,
) -> Outcome {
    outcome_value(
        command,
        serde_json::to_value(data).expect("command data serializes"),
        diagnostics,
        human,
        json_output,
        failed,
    )
}
fn outcome_value(
    command: &str,
    data: Value,
    diagnostics: Vec<Diagnostic>,
    human: String,
    json_output: bool,
    failed: bool,
) -> Outcome {
    let warning = diagnostics
        .iter()
        .any(|item| item.severity == Severity::Warning);
    Outcome {
        envelope: Envelope {
            schema_version: "1",
            command: command.into(),
            status: if failed {
                Status::Failure
            } else if warning {
                Status::Warning
            } else {
                Status::Success
            },
            data,
            diagnostics,
        },
        human,
        exit: u8::from(failed),
        json: json_output,
    }
}

fn changes_json(compilation: &Compilation) -> Value {
    Value::Array(compilation.plan.changes.iter().map(|change| json!({"path":change.path, "kind":change.kind, "beforeSha256":change.before_sha256, "afterSha256":change.after_sha256, "reason":change.reason})).chain(compilation.manual_actions.iter().map(|action| json!({"id":action.id, "kind":"MANUAL_ACTION", "target":action.target, "summary":action.summary, "requiredEnv":action.required_env}))).collect())
}
fn human_diff(compilation: &Compilation) -> String {
    let mut output = String::new();
    for change in &compilation.plan.changes {
        if change.kind == ChangeKind::Unchanged {
            continue;
        }
        output.push_str(&format!("{:?} {}\n", change.kind, change.path));
        if matches!(change.kind, ChangeKind::Modify | ChangeKind::Conflict) {
            output.push_str(&text_diff(change));
        }
    }
    for action in &compilation.manual_actions {
        output.push_str(&format!(
            "MANUAL_ACTION {}: {}\n",
            action.target.as_str(),
            action.summary
        ));
    }
    output.trim_end().into()
}
fn text_diff(change: &ArtifactChange) -> String {
    let before = change
        .before_content
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok());
    let after = change
        .desired
        .as_ref()
        .and_then(|desired| std::str::from_utf8(&desired.content).ok());
    match (before, after) {
        (Some(before), Some(after)) => TextDiff::from_lines(before, after)
            .unified_diff()
            .header(&format!("a/{}", change.path), &format!("b/{}", change.path))
            .to_string(),
        _ => format!(
            "  {:?} -> {:?}\n",
            change.before_sha256, change.after_sha256
        ),
    }
}
fn repository_root() -> std::io::Result<PathBuf> {
    let current = env::current_dir()?;
    for ancestor in current.ancestors() {
        if ancestor.join(".git").exists() || ancestor.join(".agentforge/project.yaml").exists() {
            return Ok(ancestor.into());
        }
    }
    Ok(current)
}
fn join(values: &[String]) -> String {
    if values.is_empty() {
        "-".into()
    } else {
        values.join(", ")
    }
}
fn runtime(error: impl std::fmt::Display) -> CliFailure {
    CliFailure {
        message: error.to_string(),
        exit: 3,
    }
}
fn internal(error: impl std::fmt::Display) -> CliFailure {
    CliFailure {
        message: error.to_string(),
        exit: 4,
    }
}
