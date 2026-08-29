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
        ResourceCommand::Add { value, dry_run } => pending_mutation(kind, "add", &value, dry_run),
        ResourceCommand::Remove { id, dry_run } => pending_mutation(kind, "remove", &id, dry_run),
        ResourceCommand::Update { id, dry_run } => {
            pending_mutation(kind, "update", id.as_deref().unwrap_or("all"), dry_run)
        }
    }
}

fn pending_mutation(
    kind: &str,
    operation: &str,
    value: &str,
    _dry_run: bool,
) -> Result<Outcome, CliFailure> {
    Err(CliFailure {
        message: format!("{kind} {operation} {value}: mutation workflow is not implemented yet"),
        exit: 4,
    })
}
fn init_pending(args: InitArgs) -> Result<Outcome, CliFailure> {
    let _ = (
        args.dry_run,
        args.non_interactive,
        args.strict,
        args.targets,
        args.skills,
        args.mcp,
        args.allow_unpinned_source,
        args.allow_executable_content,
    );
    Err(CliFailure {
        message: "init workflow is not implemented yet".into(),
        exit: 4,
    })
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
