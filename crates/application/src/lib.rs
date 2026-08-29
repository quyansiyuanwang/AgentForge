//! Transactional application of target-neutral AgentForge plans.

use std::{
    fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
};

use agentforge_core::planning::{ChangeKind, Manifest, ResolvedPlan, content_hash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const JOURNAL_PATH: &str = ".agentforge/transaction.json";
const MANIFEST_PATH: &str = ".agentforge/manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    AfterJournal,
    AfterBackup,
    AfterArtifact,
    BeforeManifest,
    AfterManifest,
    DuringPostValidation,
}

pub trait FaultInjector: Send + Sync {
    fn check(&self, checkpoint: Checkpoint) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn check(&self, _checkpoint: Checkpoint) -> Result<(), String> {
        Ok(())
    }
}

pub trait ApplyValidator: Send + Sync {
    fn validate_staging(&self, staging_root: &Path, plan: &ResolvedPlan) -> Result<(), String>;
    fn validate_applied(&self, repository_root: &Path, manifest: &Manifest) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopValidator;

impl ApplyValidator for NoopValidator {
    fn validate_staging(&self, _staging_root: &Path, _plan: &ResolvedPlan) -> Result<(), String> {
        Ok(())
    }
    fn validate_applied(
        &self,
        _repository_root: &Path,
        _manifest: &Manifest,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ArtifactValidator;

impl ApplyValidator for ArtifactValidator {
    fn validate_staging(&self, staging_root: &Path, plan: &ResolvedPlan) -> Result<(), String> {
        for change in &plan.changes {
            if let Some(desired) = &change.desired {
                let path = staging_root.join(native(&desired.path));
                let actual =
                    fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                if actual != desired.content {
                    return Err(format!("staged bytes differ for {}", desired.path));
                }
            }
        }
        let path = staging_root.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if manifest != plan.next_manifest {
            return Err("staged manifest differs from plan".into());
        }
        Ok(())
    }

    fn validate_applied(&self, repository_root: &Path, manifest: &Manifest) -> Result<(), String> {
        for artifact in &manifest.artifacts {
            let path = repository_root.join(native(&artifact.path));
            let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            if content_hash(&bytes) != artifact.sha256 {
                return Err(format!("applied hash differs for {}", artifact.path));
            }
        }
        let path = repository_root.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        let actual: Manifest =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if &actual != manifest {
            return Err("applied manifest differs from plan".into());
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("plan contains conflicts")]
    ConflictedPlan,
    #[error("unsafe artifact path: {0}")]
    UnsafePath(String),
    #[error("stale preview for {path}: expected {expected:?}, actual {actual:?}")]
    StalePreview {
        path: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("staging validation failed: {0}")]
    StagingValidation(String),
    #[error("post-apply validation failed: {0}")]
    PostValidation(String),
    #[error("injected fault at {checkpoint:?}: {message}")]
    Injected {
        checkpoint: Checkpoint,
        message: String,
    },
    #[error("I/O failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("journal is invalid: {0}")]
    InvalidJournal(serde_json::Error),
    #[error("manifest serialization failed: {0}")]
    ManifestSerialization(serde_json::Error),
    #[error("a previous interrupted transaction was restored; regenerate the plan")]
    RecoveredPreviousTransaction,
    #[error("apply failed ({original}) and rollback also failed ({rollback})")]
    RollbackFailed { original: String, rollback: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    transaction_dir: String,
    mutations: Vec<Mutation>,
    manifest_existed: bool,
    agentforge_directory_created: bool,
    created_directories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mutation {
    path: String,
    kind: ChangeKind,
    had_original: bool,
}

pub struct ApplicationService<V = ArtifactValidator, F = NoFaults> {
    root: PathBuf,
    validator: V,
    faults: F,
}

impl ApplicationService<ArtifactValidator, NoFaults> {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            validator: ArtifactValidator,
            faults: NoFaults,
        }
    }
}

impl<V: ApplyValidator, F: FaultInjector> ApplicationService<V, F> {
    pub fn with_components(root: impl Into<PathBuf>, validator: V, faults: F) -> Self {
        Self {
            root: root.into(),
            validator,
            faults,
        }
    }

    /// Atomically writes non-generated control files such as ProjectSpec and lock metadata.
    pub fn apply_control_files(&self, files: &[(PathBuf, Vec<u8>)]) -> Result<(), ApplyError> {
        let transaction = tempfile::Builder::new()
            .prefix(".control-")
            .tempdir_in(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        let mut backups = Vec::new();
        let mut installed = Vec::new();
        let result = (|| {
            for (relative, bytes) in files {
                let relative_text = relative.to_string_lossy().replace('\\', "/");
                validate_relative(&relative_text)?;
                validate_repository_path(&self.root, &relative_text)?;
                let target = self.root.join(relative);
                let staged = transaction.path().join(&relative_text);
                if let Some(parent) = staged.parent() {
                    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
                }
                fs::write(&staged, bytes).map_err(|source| io_error(&staged, source))?;
                if target.exists() {
                    let backup = transaction.path().join(format!("backup-{}", backups.len()));
                    fs::rename(&target, &backup).map_err(|source| io_error(&target, source))?;
                    backups.push((target.clone(), backup));
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
                }
                fs::rename(&staged, &target).map_err(|source| io_error(&target, source))?;
                installed.push(target);
            }
            Ok::<(), ApplyError>(())
        })();
        if let Err(error) = result {
            for target in installed {
                let _ = fs::remove_file(target);
            }
            for (target, backup) in backups.iter().rev() {
                let _ = fs::rename(backup, target);
            }
            return Err(error);
        }
        for (_, backup) in backups {
            let _ = fs::remove_file(backup);
        }
        Ok(())
    }

    pub fn apply(&self, plan: &ResolvedPlan) -> Result<(), ApplyError> {
        if self.recover_if_needed()? {
            return Err(ApplyError::RecoveredPreviousTransaction);
        }
        if plan.has_conflicts() {
            return Err(ApplyError::ConflictedPlan);
        }
        self.validate_paths(plan)?;
        self.validate_preconditions(plan)?;
        validate_repository_path(&self.root, ".agentforge/probe")?;
        let agentforge = self.root.join(".agentforge");
        let agentforge_directory_created = !agentforge.exists();
        fs::create_dir_all(&agentforge).map_err(|source| io_error(&agentforge, source))?;
        let transaction_dir = tempfile::Builder::new()
            .prefix(".txn-")
            .tempdir_in(&agentforge)
            .map_err(|source| io_error(&agentforge, source))?
            .keep();
        let staging = transaction_dir.join("staging");
        let backup = transaction_dir.join("backup");
        let preparation = (|| {
            fs::create_dir_all(&staging).map_err(|source| io_error(&staging, source))?;
            fs::create_dir_all(&backup).map_err(|source| io_error(&backup, source))?;
            self.stage(plan, &staging)?;
            self.validator
                .validate_staging(&staging, plan)
                .map_err(ApplyError::StagingValidation)
        })();
        if let Err(error) = preparation {
            let cleanup = cleanup_preparation(
                &transaction_dir,
                &self.root,
                &agentforge,
                agentforge_directory_created,
            );
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(ApplyError::RollbackFailed {
                    original: error.to_string(),
                    rollback: cleanup.to_string(),
                }),
            };
        }

        let journal = self.build_journal(plan, &transaction_dir, agentforge_directory_created)?;
        self.write_journal(&journal)?;
        if let Err(error) = self.apply_mutations(plan, &staging, &backup) {
            return self.rollback_after(error, &journal);
        }
        if let Err(error) = self
            .validator
            .validate_applied(&self.root, &plan.next_manifest)
            .map_err(ApplyError::PostValidation)
        {
            return self.rollback_after(error, &journal);
        }
        if let Err(error) = self.check(Checkpoint::DuringPostValidation) {
            return self.rollback_after(error, &journal);
        }
        remove_file_if_exists(&self.root.join(JOURNAL_PATH))?;
        remove_tree(&transaction_dir, &self.root)?;
        Ok(())
    }

    pub fn recover_if_needed(&self) -> Result<bool, ApplyError> {
        let journal_path = self.root.join(JOURNAL_PATH);
        if !journal_path.exists() {
            return Ok(false);
        }
        validate_repository_path(&self.root, JOURNAL_PATH)?;
        let bytes = fs::read(&journal_path).map_err(|source| io_error(&journal_path, source))?;
        let journal: Journal =
            serde_json::from_slice(&bytes).map_err(ApplyError::InvalidJournal)?;
        self.rollback(&journal)?;
        Ok(true)
    }

    fn validate_paths(&self, plan: &ResolvedPlan) -> Result<(), ApplyError> {
        for change in &plan.changes {
            validate_relative(&change.path)?;
            validate_repository_path(&self.root, &change.path)?;
        }
        Ok(())
    }

    fn validate_preconditions(&self, plan: &ResolvedPlan) -> Result<(), ApplyError> {
        for change in &plan.changes {
            if matches!(
                change.kind,
                ChangeKind::Unchanged | ChangeKind::ManualAction
            ) {
                continue;
            }
            let path = self.root.join(native(&change.path));
            let actual = read_hash(&path)?;
            if actual != change.before_sha256 {
                return Err(ApplyError::StalePreview {
                    path: change.path.clone(),
                    expected: change.before_sha256.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }

    fn stage(&self, plan: &ResolvedPlan, staging: &Path) -> Result<(), ApplyError> {
        for change in &plan.changes {
            if let Some(desired) = &change.desired {
                let path = staging.join(native(&desired.path));
                create_parent(&path)?;
                write_synced(&path, &desired.content)?;
            }
        }
        let manifest_path = staging.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        create_parent(&manifest_path)?;
        let mut bytes = serde_json::to_vec_pretty(&plan.next_manifest)
            .map_err(ApplyError::ManifestSerialization)?;
        bytes.push(b'\n');
        write_synced(&manifest_path, &bytes)?;
        Ok(())
    }

    fn build_journal(
        &self,
        plan: &ResolvedPlan,
        transaction_dir: &Path,
        agentforge_directory_created: bool,
    ) -> Result<Journal, ApplyError> {
        let relative = transaction_dir
            .strip_prefix(&self.root)
            .map_err(|_| ApplyError::UnsafePath(transaction_dir.display().to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        let mutations = plan
            .changes
            .iter()
            .filter(|change| {
                matches!(
                    change.kind,
                    ChangeKind::Create | ChangeKind::Modify | ChangeKind::Delete
                )
            })
            .map(|change| Mutation {
                path: change.path.clone(),
                kind: change.kind,
                had_original: self.root.join(native(&change.path)).exists(),
            })
            .collect();
        let mut created_directories = Vec::new();
        for change in plan
            .changes
            .iter()
            .filter(|change| matches!(change.kind, ChangeKind::Create | ChangeKind::Modify))
        {
            collect_missing_parents(
                &self.root.join(native(&change.path)),
                &self.root,
                &mut created_directories,
            );
        }
        created_directories.sort();
        created_directories.dedup();
        Ok(Journal {
            transaction_dir: relative,
            mutations,
            manifest_existed: self.root.join(MANIFEST_PATH).exists(),
            agentforge_directory_created,
            created_directories,
        })
    }

    fn write_journal(&self, journal: &Journal) -> Result<(), ApplyError> {
        let path = self.root.join(JOURNAL_PATH);
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(journal).map_err(ApplyError::InvalidJournal)?;
        write_synced(&temporary, &bytes)?;
        replace(&temporary, &path)?;
        sync_parent(&path)?;
        Ok(())
    }

    fn apply_mutations(
        &self,
        plan: &ResolvedPlan,
        staging: &Path,
        backup: &Path,
    ) -> Result<(), ApplyError> {
        self.check(Checkpoint::AfterJournal)?;
        for change in &plan.changes {
            if !matches!(
                change.kind,
                ChangeKind::Create | ChangeKind::Modify | ChangeKind::Delete
            ) {
                continue;
            }
            let destination = self.root.join(native(&change.path));
            validate_repository_path(&self.root, &change.path)?;
            let backup_path = backup.join(native(&change.path));
            if destination.exists() {
                create_parent(&backup_path)?;
                fs::rename(&destination, &backup_path)
                    .map_err(|source| io_error(&destination, source))?;
                sync_parent(&destination)?;
            }
            self.check(Checkpoint::AfterBackup)?;
            if matches!(change.kind, ChangeKind::Create | ChangeKind::Modify) {
                let source = staging.join(native(&change.path));
                create_parent(&destination)?;
                fs::rename(&source, &destination)
                    .map_err(|source| io_error(&destination, source))?;
                sync_parent(&destination)?;
            }
            self.check(Checkpoint::AfterArtifact)?;
        }
        self.check(Checkpoint::BeforeManifest)?;
        let manifest = self.root.join(MANIFEST_PATH);
        validate_repository_path(&self.root, MANIFEST_PATH)?;
        let manifest_backup =
            backup.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        if manifest.exists() {
            create_parent(&manifest_backup)?;
            fs::rename(&manifest, &manifest_backup)
                .map_err(|source| io_error(&manifest, source))?;
        }
        let staged_manifest =
            staging.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        replace(&staged_manifest, &manifest)?;
        sync_parent(&manifest)?;
        self.check(Checkpoint::AfterManifest)?;
        Ok(())
    }

    fn rollback_after(&self, original: ApplyError, journal: &Journal) -> Result<(), ApplyError> {
        match self.rollback(journal) {
            Ok(()) => Err(original),
            Err(rollback) => Err(ApplyError::RollbackFailed {
                original: original.to_string(),
                rollback: rollback.to_string(),
            }),
        }
    }

    fn rollback(&self, journal: &Journal) -> Result<(), ApplyError> {
        validate_transaction_dir(&journal.transaction_dir)?;
        validate_repository_path(&self.root, &journal.transaction_dir)?;
        let transaction = self.root.join(native(&journal.transaction_dir));
        let backup = transaction.join("backup");
        for mutation in journal.mutations.iter().rev() {
            validate_relative(&mutation.path)?;
            validate_repository_path(&self.root, &mutation.path)?;
            let destination = self.root.join(native(&mutation.path));
            let backup_path = backup.join(native(&mutation.path));
            if backup_path.exists() {
                remove_file_if_exists(&destination)?;
                create_parent(&destination)?;
                fs::rename(&backup_path, &destination)
                    .map_err(|source| io_error(&destination, source))?;
            } else if mutation.kind == ChangeKind::Create && !mutation.had_original {
                remove_file_if_exists(&destination)?;
            }
        }
        let manifest = self.root.join(MANIFEST_PATH);
        let manifest_backup =
            backup.join(MANIFEST_PATH.replace('/', std::path::MAIN_SEPARATOR_STR));
        if manifest_backup.exists() {
            remove_file_if_exists(&manifest)?;
            create_parent(&manifest)?;
            fs::rename(&manifest_backup, &manifest)
                .map_err(|source| io_error(&manifest, source))?;
        } else if !journal.manifest_existed {
            remove_file_if_exists(&manifest)?;
        }
        for directory in journal.created_directories.iter().rev() {
            let path = self.root.join(native(directory));
            if path.is_dir()
                && fs::read_dir(&path)
                    .map_err(|source| io_error(&path, source))?
                    .next()
                    .is_none()
            {
                fs::remove_dir(&path).map_err(|source| io_error(&path, source))?;
            }
        }
        remove_file_if_exists(&self.root.join(JOURNAL_PATH))?;
        if transaction.exists() {
            remove_tree(&transaction, &self.root)?;
        }
        let agentforge = self.root.join(".agentforge");
        if journal.agentforge_directory_created
            && agentforge.is_dir()
            && fs::read_dir(&agentforge)
                .map_err(|source| io_error(&agentforge, source))?
                .next()
                .is_none()
        {
            fs::remove_dir(&agentforge).map_err(|source| io_error(&agentforge, source))?;
        }
        Ok(())
    }

    fn check(&self, checkpoint: Checkpoint) -> Result<(), ApplyError> {
        self.faults
            .check(checkpoint)
            .map_err(|message| ApplyError::Injected {
                checkpoint,
                message,
            })
    }
}

fn validate_relative(path: &str) -> Result<(), ApplyError> {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    let reserved_internal = normalized == MANIFEST_PATH
        || normalized == JOURNAL_PATH
        || normalized.starts_with(".agentforge/.txn-");
    if path.is_empty()
        || path.contains('\0')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || first.eq_ignore_ascii_case(".git")
        || reserved_internal
        || Path::new(path).components().any(|part| {
            matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(ApplyError::UnsafePath(path.into()));
    }
    Ok(())
}
fn validate_transaction_dir(path: &str) -> Result<(), ApplyError> {
    let normalized = path.replace('\\', "/");
    let parts = normalized.split('/').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != 2
        || parts[0] != ".agentforge"
        || !parts[1].starts_with(".txn-")
        || parts[1].len() <= ".txn-".len()
        || Path::new(path).is_absolute()
    {
        return Err(ApplyError::UnsafePath(path.into()));
    }
    Ok(())
}
fn validate_repository_path(root: &Path, relative: &str) -> Result<(), ApplyError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
    let mut cursor = root.to_path_buf();
    for component in Path::new(&native(relative)).components() {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(ApplyError::UnsafePath(relative.into()));
                }
                let canonical =
                    fs::canonicalize(&cursor).map_err(|source| io_error(&cursor, source))?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(ApplyError::UnsafePath(relative.into()));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(source) => return Err(io_error(&cursor, source)),
        }
    }
    Ok(())
}
fn native(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}
fn read_hash(path: &Path) -> Result<Option<String>, ApplyError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(content_hash(&bytes))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}
fn create_parent(path: &Path) -> Result<(), ApplyError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    Ok(())
}
fn collect_missing_parents(path: &Path, root: &Path, created: &mut Vec<String>) {
    let Some(mut cursor) = path.parent() else {
        return;
    };
    let mut missing = Vec::new();
    while cursor.starts_with(root) && cursor != root && !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().unwrap_or(root);
    }
    for directory in missing.into_iter().rev() {
        created.push(
            directory
                .strip_prefix(root)
                .expect("directory is below root")
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
}
fn replace(source: &Path, destination: &Path) -> Result<(), ApplyError> {
    if destination.exists() {
        return Err(ApplyError::UnsafePath(destination.display().to_string()));
    }
    fs::rename(source, destination).map_err(|source| io_error(destination, source))
}
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ApplyError> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))
}
#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), ApplyError> {
    let parent = path
        .parent()
        .ok_or_else(|| ApplyError::UnsafePath(path.display().to_string()))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}
#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), ApplyError> {
    Ok(())
}
fn remove_file_if_exists(path: &Path) -> Result<(), ApplyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}
fn remove_tree(path: &Path, root: &Path) -> Result<(), ApplyError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ApplyError::UnsafePath(path.display().to_string()));
    }
    let canonical_root = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
    let canonical = fs::canonicalize(path).map_err(|source| io_error(path, source))?;
    if !canonical.starts_with(&canonical_root) || canonical == canonical_root {
        return Err(ApplyError::UnsafePath(path.display().to_string()));
    }
    fs::remove_dir_all(&canonical).map_err(|source| io_error(&canonical, source))
}
fn cleanup_preparation(
    transaction: &Path,
    root: &Path,
    agentforge: &Path,
    agentforge_created: bool,
) -> Result<(), ApplyError> {
    if transaction.exists() {
        remove_tree(transaction, root)?;
    }
    if agentforge_created
        && agentforge.is_dir()
        && fs::read_dir(agentforge)
            .map_err(|source| io_error(agentforge, source))?
            .next()
            .is_none()
    {
        fs::remove_dir(agentforge).map_err(|source| io_error(agentforge, source))?;
    }
    Ok(())
}
fn io_error(path: &Path, source: io::Error) -> ApplyError {
    ApplyError::Io {
        path: path.into(),
        source,
    }
}
