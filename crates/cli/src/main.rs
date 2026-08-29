use std::{
    env,
    io::IsTerminal,
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
    #[arg(long)]
    json: bool,
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
    #[arg(long)]
    json: bool,
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
        #[arg(long)]
        json: bool,
    },
    Remove {
        id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    Update {
        id: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        allow_unpinned_source: bool,
        #[arg(long)]
        allow_executable_content: bool,
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
            args.json,
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
        args.json,
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
    let mut compilation = match compile(root) {
        Ok(compilation) => compilation,
        Err(error) => {
            let message = error.to_string();
            let diagnostic = Diagnostic::error(DiagnosticCode::IoFailure, message.clone());
            return Ok(outcome_value(
                "doctor",
                json!({"health":"Unhealthy","checks":[{"check":"compile","healthy":false,"error":message}]}),
                vec![diagnostic],
                "Unhealthy".into(),
                args.json,
                true,
            ));
        }
    };
    let mut checks = Vec::new();
    let lock_path = root.join(".agentforge/lock.yaml");
    let manifest_path = root.join(".agentforge/manifest.json");
    checks.push(json!({"check":"lock","path":lock_path,"healthy":lock_path.exists()}));
    if !lock_path.exists() {
        compilation.diagnostics.push(
            Diagnostic::warning(
                DiagnosticCode::UnknownReference,
                "lock.yaml is missing; run init or explicitly vendor remote sources",
            )
            .with_remediation(
                "create .agentforge/lock.yaml before committing the project configuration",
            ),
        );
    }
    if let Ok(bytes) = std::fs::read(&lock_path) {
        if let Ok(lock) = serde_yaml::from_slice::<LockFile>(&bytes) {
            let mut referenced = std::collections::BTreeSet::new();
            for item in &compilation.spec.instructions {
                if !matches!(item.source, agentforge_core::model::Source::Local { .. }) {
                    referenced.insert((ContentKind::Instruction, item.id.clone()));
                }
            }
            for item in &compilation.spec.skills {
                if !matches!(item.source, agentforge_core::model::Source::Local { .. }) {
                    referenced.insert((ContentKind::Skill, item.id.clone()));
                }
            }
            for item in &compilation.spec.mcp {
                if let Some(source) = &item.source {
                    if !matches!(source, agentforge_core::model::Source::Local { .. }) {
                        referenced.insert((ContentKind::Mcp, item.id.clone()));
                    }
                }
            }
            for item in &compilation.spec.subagents {
                if !matches!(
                    item.instructions,
                    agentforge_core::model::Source::Local { .. }
                ) {
                    referenced.insert((ContentKind::Subagent, item.id.clone()));
                }
            }
            let stale = lock
                .sources
                .iter()
                .filter(|entry| !referenced.contains(&(entry.kind, entry.id.clone())))
                .map(|entry| format!("{:?}/{}", entry.kind, entry.id))
                .collect::<Vec<_>>();
            checks.push(json!({"check":"lockReferences","healthy":stale.is_empty(),"stale":stale}));
            if !stale.is_empty() {
                compilation.diagnostics.push(Diagnostic::warning(
                    DiagnosticCode::UnknownReference,
                    format!("lock contains stale sources: {}", stale.join(", ")),
                ));
            }
        }
    }
    checks.push(json!({"check":"vendor","healthy":true,"verified":"offline"}));
    let manifest = std::fs::read(&manifest_path).ok().and_then(|bytes| {
        serde_json::from_slice::<agentforge_core::planning::Manifest>(&bytes).ok()
    });
    checks.push(json!({"check":"manifest","path":manifest_path,"healthy":manifest.is_some()}));
    if let Some(manifest) = &manifest {
        let renderers = manifest
            .artifacts
            .iter()
            .map(|artifact| json!({"target":artifact.target,"version":artifact.renderer_version}))
            .collect::<Vec<_>>();
        checks.push(json!({"check":"rendererVersion","healthy":!renderers.is_empty(),"renderers":renderers}));
    }
    checks.push(json!({"check":"artifactDrift","healthy":!compilation.plan.has_conflicts(),"changes":changes_json(&compilation)}));
    checks.push(json!({
        "check":"capabilities",
        "healthy":!compilation.capabilities.blocks_apply(false),
        "decisions":compilation.capabilities.decisions
    }));
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
            let spec = load_spec(root)?;
            let data = serde_json::to_value(&spec).map_err(internal)?;
            let human = serde_yaml::to_string(&spec).map_err(internal)?;
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
    if let ResourceCommand::Update {
        id,
        dry_run,
        json,
        allow_unpinned_source,
        allow_executable_content,
    } = &command
    {
        return update_resources(
            root,
            kind,
            id.as_deref(),
            *dry_run,
            *json,
            *allow_unpinned_source,
            *allow_executable_content,
        );
    }
    if kind == "target" {
        if let ResourceCommand::List { json } = command {
            let targets = [
                Target::Generic,
                Target::Codex,
                Target::Claude,
                Target::Copilot,
            ];
            let data = serde_json::to_value(targets).map_err(internal)?;
            return Ok(outcome_value(
                "target list",
                data,
                vec![],
                targets
                    .iter()
                    .map(|target| target.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                json,
                false,
            ));
        }
    }
    let spec = load_spec(root)?;
    match command {
        ResourceCommand::List { json } => {
            let data = match kind {
                "skill" => serde_json::to_value(&spec.skills),
                "mcp" => serde_json::to_value(&spec.mcp),
                "subagent" => serde_json::to_value(&spec.subagents),
                "target" => serde_json::to_value(&spec.targets),
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
        ResourceCommand::Add {
            value,
            dry_run,
            json,
        } => mutate_spec(root, &spec, kind, "add", &value, dry_run, json),
        ResourceCommand::Remove { id, dry_run, json } => {
            mutate_spec(root, &spec, kind, "remove", &id, dry_run, json)
        }
        ResourceCommand::Update { .. } => unreachable!(),
    }
}

fn load_spec(root: &Path) -> Result<agentforge_core::model::ProjectSpec, CliFailure> {
    let text = std::fs::read_to_string(root.join(".agentforge/project.yaml")).map_err(runtime)?;
    let validation = SpecValidator::new().validate_yaml(&text);
    validation.spec.ok_or_else(|| CliFailure {
        message: validation
            .diagnostics
            .iter()
            .map(|item| format!("{}: {}", item.code.as_str(), item.message))
            .collect::<Vec<_>>()
            .join("\n"),
        exit: 1,
    })
}

fn update_resources(
    root: &Path,
    kind: &str,
    selected: Option<&str>,
    dry_run: bool,
    json_output: bool,
    allow_unpinned: bool,
    allow_executable_content: bool,
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
            let mut pending_vendor = Vec::new();
            let mut cleanup_vendor = Vec::new();
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
                            allow_unpinned,
                            allow_executable_content,
                            installed_vendor_bytes: existing
                                .sources
                                .iter()
                                .filter(|entry| {
                                    !(entry.kind == ContentKind::Skill && entry.id == item.id)
                                })
                                .map(|entry| entry.total_bytes)
                                .sum(),
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        if !dry_run {
                            pending_vendor.push((lock.vendor_path.clone(), vendor));
                            cleanup_vendor.push(PathBuf::from(
                                lock.vendor_path.replace('/', std::path::MAIN_SEPARATOR_STR),
                            ));
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
                            allow_unpinned,
                            allow_executable_content,
                            installed_vendor_bytes: existing
                                .sources
                                .iter()
                                .filter(|entry| {
                                    !(entry.kind == ContentKind::Mcp && entry.id == item.0.id)
                                })
                                .map(|entry| entry.total_bytes)
                                .sum(),
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        if !dry_run {
                            pending_vendor.push((lock.vendor_path.clone(), vendor));
                            cleanup_vendor.push(PathBuf::from(
                                lock.vendor_path.replace('/', std::path::MAIN_SEPARATOR_STR),
                            ));
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
                let files = pending_vendor
                    .into_iter()
                    .flat_map(|(vendor_path, vendor)| {
                        vendor.files.into_iter().map(move |file| {
                            (
                                PathBuf::from(format!("{vendor_path}/{}", file.path)),
                                file.content,
                                file.mode,
                            )
                        })
                    })
                    .chain(std::iter::once((
                        PathBuf::from(".agentforge/lock.yaml"),
                        yaml.into_bytes(),
                        0o644,
                    )))
                    .collect::<Vec<_>>();
                ApplicationService::new(root)
                    .apply_control_files_with_modes_and_cleanup(&files, &cleanup_vendor)
                    .map_err(|error| error.to_string())?;
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
        json_output,
        false,
    ))
}

fn mutate_spec(
    root: &Path,
    current: &agentforge_core::model::ProjectSpec,
    kind: &str,
    operation: &str,
    value: &str,
    dry_run: bool,
    json_output: bool,
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
    let previous_rendered = std::fs::read(&path).map_err(runtime)?;
    let rendered = serde_yaml::to_string(&spec).map_err(internal)?;
    let summary = json!({
        "operation": operation,
        "resource": kind,
        "id": value,
        "beforeCount": resource_count(current, kind),
        "afterCount": resource_count(&spec, kind),
    });
    if !dry_run {
        ApplicationService::new(root)
            .apply_control_files(&[(
                PathBuf::from(".agentforge/project.yaml"),
                rendered.into_bytes(),
            )])
            .map_err(runtime)?;
        let compilation = match compile(root) {
            Ok(compilation) => compilation,
            Err(error) => {
                restore_spec(root, &previous_rendered);
                return Err(error);
            }
        };
        if compilation.blocks_apply(false) {
            let changes = changes_json(&compilation);
            let preview = human_diff(&compilation);
            restore_spec(root, &previous_rendered);
            return Ok(outcome_value(
                &format!("{kind} {operation}"),
                json!({"path":path,"dryRun":false,"changed":false,"summary":summary,"changes":changes}),
                compilation.diagnostics,
                preview,
                json_output,
                true,
            ));
        }
        if compilation.plan.has_changes() {
            if let Err(error) = ApplicationService::new(root).apply(&compilation.plan) {
                restore_spec(root, &previous_rendered);
                return Err(runtime(error));
            }
        }
    }
    Ok(outcome_value(
        &format!("{kind} {operation}"),
        json!({"path":path,"dryRun":dry_run,"changed":true,"summary":summary}),
        vec![],
        if dry_run {
            "Would update project spec".into()
        } else {
            "Updated project spec".into()
        },
        json_output,
        false,
    ))
}

fn restore_spec(root: &Path, bytes: &[u8]) {
    let _ = ApplicationService::new(root)
        .apply_control_files(&[(PathBuf::from(".agentforge/project.yaml"), bytes.to_vec())]);
}

fn resource_count(spec: &agentforge_core::model::ProjectSpec, kind: &str) -> usize {
    match kind {
        "skill" => spec.skills.len(),
        "mcp" => spec.mcp.len(),
        "subagent" => spec.subagents.len(),
        "target" => spec.targets.len(),
        _ => 0,
    }
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
    let targets = if args.targets.is_empty() {
        if args.non_interactive || !std::io::stdin().is_terminal() {
            return Err(CliFailure {
                message: if args.non_interactive {
                    "--non-interactive requires at least one --target".into()
                } else {
                    "init requires an explicit --target when no interactive terminal is available"
                        .into()
                },
                exit: 2,
            });
        }
        match agentforge_tui::run_target_selection().map_err(runtime)? {
            agentforge_tui::FlowState::Confirmed { targets } if !targets.is_empty() => targets
                .into_iter()
                .map(|target| match target {
                    Target::Generic => TargetArg::Generic,
                    Target::Codex => TargetArg::Codex,
                    Target::Claude => TargetArg::Claude,
                    Target::Copilot => TargetArg::Copilot,
                })
                .collect(),
            agentforge_tui::FlowState::Cancelled => {
                return Err(CliFailure {
                    message: "init cancelled".into(),
                    exit: 1,
                });
            }
            _ => {
                return Err(CliFailure {
                    message: "at least one target must be selected".into(),
                    exit: 2,
                });
            }
        }
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
    let report = DetectionEngine::with_builtins().detect(&DetectionContext {
        root: &root,
        filesystem: &RealFileSystem,
    });
    let context_path = PathBuf::from(".agentforge/generated/project-context.md");
    let context_content = project_context(&report.profile);
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
            source: cli_source(path),
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
            source: Some(cli_source(path)),
            transport: None,
            env: vec![],
        })
        .collect();
    let spec = agentforge_core::model::ProjectSpec {
        schema_version: "1".into(),
        project: agentforge_core::model::Project { name },
        targets: target_values.clone(),
        instructions: vec![agentforge_core::model::Content {
            id: "agentforge-context".into(),
            source: agentforge_core::model::Source::Local {
                path: context_path.to_string_lossy().replace('\\', "/"),
            },
            applies_to: vec![],
        }],
        skills,
        mcp,
        subagents: vec![],
        settings: Default::default(),
        extensions: Default::default(),
    };
    let rendered = serde_yaml::to_string(&spec).map_err(internal)?;
    let planned_artifacts = planned_artifacts(&target_values);
    let lock_path = root.join(".agentforge/lock.yaml");
    let previous_lock = std::fs::read(&lock_path).ok();
    let vendor_root = root.join(".agentforge/vendor");
    let previous_vendor = vendor_root.exists();
    if !args.dry_run {
        let empty_lock = LockFile::new(format!("agentforge {}", env!("CARGO_PKG_VERSION")), vec![])
            .map_err(runtime)?
            .to_yaml()
            .map_err(runtime)?;
        ApplicationService::new(&root)
            .apply_control_files(&[
                (
                    PathBuf::from(".agentforge/project.yaml"),
                    rendered.into_bytes(),
                ),
                (
                    PathBuf::from(".agentforge/lock.yaml"),
                    empty_lock.into_bytes(),
                ),
                (context_path.clone(), context_content.into_bytes()),
            ])
            .map_err(runtime)?;
        if spec
            .skills
            .iter()
            .any(|item| !matches!(item.source, agentforge_core::model::Source::Local { .. }))
        {
            if let Err(error) = update_resources(
                &root,
                "skill",
                None,
                false,
                args.json,
                args.allow_unpinned_source,
                args.allow_executable_content,
            ) {
                rollback_init_files(&root, previous_lock.as_deref(), previous_vendor);
                return Err(error);
            }
        }
        if spec.mcp.iter().any(|item| {
            item.source.as_ref().is_some_and(|source| {
                !matches!(source, agentforge_core::model::Source::Local { .. })
            })
        }) {
            if let Err(error) = update_resources(
                &root,
                "mcp",
                None,
                false,
                args.json,
                args.allow_unpinned_source,
                args.allow_executable_content,
            ) {
                rollback_init_files(&root, previous_lock.as_deref(), previous_vendor);
                return Err(error);
            }
        }
    }
    let mut human = if args.dry_run {
        "Would initialize project".to_owned()
    } else {
        let compilation = match compile(&root) {
            Ok(compilation) => compilation,
            Err(error) => {
                rollback_init_files(&root, previous_lock.as_deref(), previous_vendor);
                return Err(error);
            }
        };
        if compilation.blocks_apply(args.strict) {
            let changes = changes_json(&compilation);
            let preview = human_diff(&compilation);
            let diagnostics = compilation.diagnostics.clone();
            rollback_init_files(&root, previous_lock.as_deref(), previous_vendor);
            return Ok(outcome_value(
                "init",
                json!({"path":path,"dryRun":false,"spec":spec,"facts":report.profile,"evidence":report.profile.evidence,"changes":changes}),
                diagnostics,
                preview,
                args.json,
                true,
            ));
        }
        if compilation.plan.has_changes() {
            if let Err(error) = ApplicationService::new(&root)
                .apply(&compilation.plan)
                .map_err(|error| CliFailure {
                    message: error.to_string(),
                    exit: 3,
                })
            {
                rollback_init_files(&root, previous_lock.as_deref(), previous_vendor);
                return Err(error);
            }
            format!("Initialized project\n{}", human_diff(&compilation))
        } else {
            "Initialized project".to_owned()
        }
    };
    Ok(outcome(
        "init",
        json!({"path":path,"dryRun":args.dry_run,"spec":spec,"facts":report.profile,"evidence":report.profile.evidence,"plannedArtifacts":planned_artifacts}),
        report.diagnostics,
        std::mem::take(&mut human),
        args.json,
        false,
    ))
}

fn planned_artifacts(targets: &[Target]) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for target in targets {
        match target {
            Target::Generic => {
                paths.insert("AGENTS.md".to_owned());
                paths.insert(".agentforge/generated/mcp.json".to_owned());
                paths.insert(".agentforge/generated/settings.yaml".to_owned());
            }
            Target::Codex => {
                paths.insert("AGENTS.md".to_owned());
                paths.insert(".codex/config.toml".to_owned());
            }
            Target::Claude => {
                paths.insert("CLAUDE.md".to_owned());
                paths.insert(".mcp.json".to_owned());
                paths.insert(".claude/settings.json".to_owned());
            }
            Target::Copilot => {
                paths.insert(".github/copilot-instructions.md".to_owned());
                paths.insert(".github/mcp.json".to_owned());
            }
        }
    }
    paths.into_iter().collect()
}

fn cli_source(value: &str) -> agentforge_core::model::Source {
    if value.starts_with("https://") {
        agentforge_core::model::Source::Url {
            url: value.into(),
            sha256: None,
        }
    } else if let Some(reference) = value.strip_prefix("github:") {
        let (reference, rev) = split_ref_version(reference);
        let mut parts = reference.split('/');
        let owner = parts.next().unwrap_or_default();
        let repository = parts.next().unwrap_or_default();
        let subpath = parts.collect::<Vec<_>>().join("/");
        if owner.is_empty() || repository.is_empty() {
            return agentforge_core::model::Source::Local { path: value.into() };
        }
        agentforge_core::model::Source::Git {
            repository: format!("https://github.com/{owner}/{repository}.git"),
            rev: rev.unwrap_or("HEAD").into(),
            subpath: (!subpath.is_empty()).then_some(subpath),
        }
    } else if let Some(reference) = value
        .strip_prefix("skills:")
        .or_else(|| value.strip_prefix("skills.sh:"))
    {
        let (reference, version) = split_ref_version(reference);
        agentforge_core::model::Source::SkillsSh {
            r#ref: reference.into(),
            version: version.map(str::to_owned),
        }
    } else if let Some(reference) = value.strip_prefix("registry:") {
        let (reference, version) = split_ref_version(reference);
        agentforge_core::model::Source::McpRegistry {
            r#ref: reference.into(),
            version: version.map(str::to_owned),
        }
    } else {
        agentforge_core::model::Source::Local { path: value.into() }
    }
}

fn rollback_init_files(root: &Path, previous_lock: Option<&[u8]>, previous_vendor: bool) {
    let project = root.join(".agentforge/project.yaml");
    let lock = root.join(".agentforge/lock.yaml");
    let context = root.join(".agentforge/generated/project-context.md");
    let _ = std::fs::remove_file(project);
    let _ = std::fs::remove_file(context);
    match previous_lock {
        Some(bytes) => {
            let _ = std::fs::write(lock, bytes);
        }
        None => {
            let _ = std::fs::remove_file(lock);
        }
    }
    if !previous_vendor {
        let vendor = root.join(".agentforge/vendor");
        if std::fs::symlink_metadata(&vendor)
            .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir_all(vendor);
        }
    }
}

fn split_ref_version(value: &str) -> (&str, Option<&str>) {
    value
        .rsplit_once('@')
        .map_or((value, None), |(reference, version)| {
            if reference.is_empty() || version.is_empty() {
                (value, None)
            } else {
                (reference, Some(version))
            }
        })
}

fn project_context(profile: &agentforge_core::model::ProjectProfile) -> String {
    let command = |name: &str| {
        profile
            .commands
            .get(name)
            .map(|value| format!("`{value}`"))
            .unwrap_or_else(|| "TODO (confirm the repository command)".into())
    };
    format!(
        "# AgentForge Project Context\n\n## Detected project facts\n- Languages: {}\n- Frameworks: {}\n- Databases: {}\n- Package managers: {}\n- Tests: {}\n- CI: {}\n- Tools: {}\n\n## Project commands\n- install: {}\n- build: {}\n- test: {}\n- lint: {}\n\n## Editing boundaries\n- Edit canonical sources and `.agentforge/project.yaml`; do not hand-edit generated target files.\n- Run `agentforge diff` before committing generated changes.\n- Move team-specific instructions, skills, MCP servers, and subagents into declared sources.\n",
        join(&profile.languages),
        join(&profile.frameworks),
        join(&profile.databases),
        join(&profile.package_managers),
        join(&profile.tests),
        join(&profile.ci),
        join(&profile.tools),
        command("install"),
        command("build"),
        command("test"),
        command("lint")
    )
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
        (Some(before), Some(after)) => {
            TextDiff::from_lines(&redact_text(before), &redact_text(after))
                .unified_diff()
                .header(&format!("a/{}", change.path), &format!("b/{}", change.path))
                .to_string()
        }
        _ => format!(
            "  {:?} -> {:?}\n",
            change.before_sha256, change.after_sha256
        ),
    }
}

fn redact_text(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "token",
                "secret",
                "password",
                "authorization",
                "cookie",
                "api_key",
            ]
            .iter()
            .any(|key| lower.contains(key))
            {
                "[REDACTED SENSITIVE LINE]".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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

#[cfg(test)]
mod tests {
    use super::{cli_source, redact_text};
    use agentforge_core::model::Source;

    #[test]
    fn sensitive_diff_lines_are_redacted() {
        let output = redact_text("command: safe\nGITHUB_TOKEN=do-not-print\nnext: value");
        assert!(!output.contains("do-not-print"));
        assert!(output.contains("[REDACTED SENSITIVE LINE]"));
        assert!(output.contains("command: safe"));
    }

    #[test]
    fn cli_source_parses_documented_remote_shorthands() {
        assert_eq!(
            cli_source("github:acme/agents/security-review@v2.0.0"),
            Source::Git {
                repository: "https://github.com/acme/agents.git".into(),
                rev: "v2.0.0".into(),
                subpath: Some("security-review".into()),
            }
        );
        assert_eq!(
            cli_source("skills:acme/agents/testing@1.2.0"),
            Source::SkillsSh {
                r#ref: "acme/agents/testing".into(),
                version: Some("1.2.0".into()),
            }
        );
        assert_eq!(
            cli_source("registry:io.github.example/server@1.4.0"),
            Source::McpRegistry {
                r#ref: "io.github.example/server".into(),
                version: Some("1.4.0".into()),
            }
        );
    }
}
