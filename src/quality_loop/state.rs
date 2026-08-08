use crate::quality_loop::LoopInvocation;
use crate::quality_loop::Worktree;
use crate::rollout::OperatorInputRecord;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

pub(crate) const LOOP_PROTOCOL_VERSION: u32 = 1;
pub(crate) const CONTRACT_VERSION: u32 = 1;

const EVALUATOR_PROMPT: &str = include_str!("../../prompts/loop-evaluator.md");
const WORKER_PROMPT: &str = include_str!("../../prompts/loop-worker.md");
const CONTRACT_PROMPT: &str = include_str!("../../prompts/loop-contract.md");

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunPhase {
    Setup,
    Baselining,
    Iteration,
    Restoring,
    Completed,
    Blocked,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RunState {
    pub(crate) protocol_version: u32,
    pub(crate) contract_version: u32,
    pub(crate) run_id: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) build_identity: String,
    pub(crate) evaluator_prompt_identity: String,
    pub(crate) worker_prompt_identity: String,
    pub(crate) contract_prompt_identity: String,
    pub(crate) requested_iterations: usize,
    pub(crate) phase: RunPhase,
    pub(crate) active_iteration: Option<usize>,
    #[serde(default)]
    pub(crate) active_candidate_snapshot: Option<String>,
    #[serde(default)]
    pub(crate) prepared_iteration: Option<PreparedIteration>,
    pub(crate) display_name: String,
    pub(crate) starting_snapshot: String,
    pub(crate) incumbent_snapshot: String,
    pub(crate) baseline_result: Option<String>,
    pub(crate) final_result: Option<String>,
    pub(crate) blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PreparedIteration {
    pub(crate) iteration: usize,
    pub(crate) target_snapshot: String,
    pub(crate) state: String,
    pub(crate) result: String,
    pub(crate) status: String,
    pub(crate) description: String,
    pub(crate) evidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FrozenTaskRecord<'a> {
    pub(crate) invocation: &'a LoopInvocation,
    pub(crate) operator_inputs: &'a [OperatorInputRecord],
    pub(crate) frozen_context: &'a [serde_json::Value],
}

pub(crate) struct LoopRun {
    root: PathBuf,
    lock: File,
    pub(crate) state: RunState,
}

impl LoopRun {
    pub(crate) fn create(
        worktree: &Worktree,
        invocation: &LoopInvocation,
        operator_inputs: &[OperatorInputRecord],
        frozen_context: &[serde_json::Value],
    ) -> Result<Self> {
        worktree.reject_tracked_loop_state()?;
        worktree.install_loop_exclude()?;
        let loops = prepare_loop_directories(worktree.root())?;
        let mut lock = acquire_lock(&loops)?;
        recover_incomplete_runs(worktree, &loops)?;
        let run_id = Uuid::new_v4().to_string();
        let root = loops.join(&run_id);
        create_new_private_directory(&root)?;
        let initialized = (|| -> Result<RunState> {
            write_lock_metadata(&mut lock, &run_id)?;
            for directory in [
                "evaluator/workspace",
                "iterations",
                "snapshots",
                "blobs",
                "sessions",
            ] {
                create_private_directory(&root.join(directory))?;
            }

            let starting = worktree.capture(&root, &[])?;
            let snapshot_path = worktree.save_snapshot(&root, &starting)?;
            let snapshot_relative = relative_run_path(&root, &snapshot_path)?;
            let task = FrozenTaskRecord {
                invocation,
                operator_inputs,
                frozen_context,
            };
            atomic_json(&root.join("task.json"), &task)?;
            write_new_private(
                &root.join("results.tsv"),
                b"iteration\tstate\tresult\tstatus\tdescription\tevidence\n",
            )?;
            let state = RunState {
                protocol_version: LOOP_PROTOCOL_VERSION,
                contract_version: CONTRACT_VERSION,
                run_id,
                model: crate::MODEL.to_string(),
                reasoning_effort: "max".to_string(),
                build_identity: build_identity(),
                evaluator_prompt_identity: digest(EVALUATOR_PROMPT.as_bytes()),
                worker_prompt_identity: digest(WORKER_PROMPT.as_bytes()),
                contract_prompt_identity: digest(CONTRACT_PROMPT.as_bytes()),
                requested_iterations: invocation.iterations,
                phase: RunPhase::Setup,
                active_iteration: None,
                active_candidate_snapshot: None,
                prepared_iteration: None,
                display_name: "Quality loop".to_string(),
                starting_snapshot: snapshot_relative.clone(),
                incumbent_snapshot: snapshot_relative,
                baseline_result: None,
                final_result: None,
                blocker: None,
            };
            atomic_json(&root.join("state.json"), &state)?;
            sync_directory(&root)?;
            Ok(state)
        })();
        let state = match initialized {
            Ok(state) => state,
            Err(error) => {
                if let Err(cleanup_error) = discard_incomplete_run(&mut lock, &root) {
                    return Err(anyhow!(
                        "{error:#}; failed to discard incomplete loop run: {cleanup_error:#}"
                    ));
                }
                return Err(error);
            }
        };
        Ok(Self { root, lock, state })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.state.run_id
    }

    pub(crate) fn update(&mut self, update: impl FnOnce(&mut RunState)) -> Result<()> {
        update(&mut self.state);
        atomic_json(&self.root.join("state.json"), &self.state)
    }

    pub(crate) fn append_ledger(&self, row: &LedgerRow<'_>) -> Result<()> {
        append_ledger_file(&self.root.join("results.tsv"), row)
    }

    pub(crate) fn publish_iteration(&mut self, row: &LedgerRow<'_>) -> Result<()> {
        if self.state.active_iteration != Some(row.iteration) {
            return Err(anyhow!(
                "cannot publish iteration {} without its active marker",
                row.iteration
            ));
        }
        if !matches!(
            row.status,
            "keep" | "discard" | "crash" | "blocked" | "interrupted"
        ) {
            return Err(anyhow!("cannot publish unknown iteration status"));
        }
        validate_relative(row.evidence)?;
        let target_snapshot = if row.status == "keep" {
            self.state
                .active_candidate_snapshot
                .clone()
                .ok_or_else(|| anyhow!("kept iteration has no durable candidate snapshot"))?
        } else {
            self.state.incumbent_snapshot.clone()
        };
        let prepared = PreparedIteration {
            iteration: row.iteration,
            target_snapshot: target_snapshot.clone(),
            state: row.state.to_string(),
            result: row.result.to_string(),
            status: row.status.to_string(),
            description: row.description.to_string(),
            evidence: row.evidence.to_string(),
        };
        self.update(|state| state.prepared_iteration = Some(prepared))?;
        self.append_ledger(row)?;
        self.update(|state| {
            if row.status == "keep" {
                state.incumbent_snapshot = target_snapshot;
                state.final_result = Some(row.result.to_string());
            }
            state.active_iteration = None;
            state.active_candidate_snapshot = None;
            state.prepared_iteration = None;
        })
    }

    pub(crate) fn relative(&self, path: &Path) -> Result<String> {
        relative_run_path(&self.root, path)
    }

    pub(crate) fn write_json(&self, relative: &str, value: &impl Serialize) -> Result<PathBuf> {
        validate_relative(relative)?;
        let path = self.root.join(relative);
        let parent = Path::new(relative)
            .parent()
            .ok_or_else(|| anyhow!("run artifact has no parent"))?;
        verify_private_directory_chain(&self.root, parent, false)?;
        atomic_json(&path, value)?;
        Ok(path)
    }

    pub(crate) fn create_directory(&self, relative: &str) -> Result<PathBuf> {
        validate_relative(relative)?;
        verify_private_directory_chain(&self.root, Path::new(relative), true)
    }

    pub(crate) fn resolve_existing_file(&self, value: &str, under: &Path) -> Result<PathBuf> {
        resolve_existing_file(&self.root, value, under)
    }

    pub(crate) fn session_root(&self, phase: &str) -> Result<PathBuf> {
        if phase.is_empty()
            || !phase
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(anyhow!("invalid internal session name"));
        }
        verify_private_directory_chain(&self.root, &Path::new("sessions").join(phase), true)
    }

    pub(crate) fn verify_runtime_identity(&self) -> Result<()> {
        verify_runtime_state(&self.state)
    }
}

impl Drop for LoopRun {
    fn drop(&mut self) {
        let _ = self.lock.sync_all();
        let _ = unsafe { libc::flock(self.lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(crate) struct LedgerRow<'a> {
    pub(crate) iteration: usize,
    pub(crate) state: &'a str,
    pub(crate) result: &'a str,
    pub(crate) status: &'a str,
    pub(crate) description: &'a str,
    pub(crate) evidence: &'a str,
}

fn recover_incomplete_runs(worktree: &Worktree, loops: &Path) -> Result<()> {
    let mut entries = std::fs::read_dir(loops)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if name == "worktree.lock" {
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("quality-loop state contains a non-UTF-8 entry"))?;
        if Uuid::parse_str(name).is_err() {
            return Err(anyhow!(
                "unexpected entry in quality-loop state: {}",
                entry.path().display()
            ));
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(anyhow!(
                "unsafe quality-loop run directory {}",
                entry.path().display()
            ));
        }
        recover_run(worktree, &entry.path(), name)?;
    }
    Ok(())
}

fn recover_run(worktree: &Worktree, root: &Path, directory_id: &str) -> Result<()> {
    let state_path = root.join("state.json");
    let metadata = std::fs::symlink_metadata(&state_path)
        .with_context(|| format!("incomplete loop run `{directory_id}` has no state record"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(anyhow!(
            "loop run `{directory_id}` has an unsafe state record"
        ));
    }
    let mut state: RunState = serde_json::from_slice(&std::fs::read(&state_path)?)
        .with_context(|| format!("loop run `{directory_id}` has invalid state"))?;
    if state.run_id != directory_id {
        return Err(anyhow!(
            "loop run directory `{directory_id}` disagrees with state ID `{}`",
            state.run_id
        ));
    }
    if matches!(
        state.phase,
        RunPhase::Completed | RunPhase::Blocked | RunPhase::Interrupted
    ) {
        return Ok(());
    }
    verify_runtime_state(&state)
        .with_context(|| format!("cannot recover incomplete quality-loop run `{directory_id}`"))?;
    if state.phase == RunPhase::Iteration
        && state.active_iteration.is_none()
        && state.prepared_iteration.is_none()
    {
        return Err(anyhow!(
            "incomplete loop run `{directory_id}` has no active iteration marker"
        ));
    }
    if let Some(prepared) = state.prepared_iteration.clone() {
        return recover_prepared_iteration(worktree, root, directory_id, &mut state, prepared);
    }
    let incumbent = load_run_snapshot(root, &state.incumbent_snapshot)?;
    if !worktree.state_matches(root, &incumbent)? {
        let candidate = state
            .active_candidate_snapshot
            .as_deref()
            .map(|relative| load_run_snapshot(root, relative))
            .transpose()?;
        let matches_candidate = match candidate.as_ref() {
            Some(candidate) => worktree.state_matches(root, candidate)?,
            None => false,
        };
        if !matches_candidate {
            return Err(anyhow!(
                "incomplete loop run `{directory_id}` conflicts with repository changes made after its last durable state; no files were overwritten"
            ));
        }
        worktree.restore(root, &incumbent).with_context(|| {
            format!("failed to restore incomplete quality-loop run `{directory_id}`")
        })?;
    }

    if let Some(iteration) = state.active_iteration {
        if iteration == 0 || iteration > state.requested_iterations {
            return Err(anyhow!(
                "incomplete loop run `{directory_id}` has an invalid active iteration"
            ));
        }
        let evidence_relative = format!("iterations/{iteration}/recovered.json");
        let evidence_path = root.join(&evidence_relative);
        atomic_json(
            &evidence_path,
            &serde_json::json!({
                "iteration": iteration,
                "status": "crash",
                "state": incumbent.state.digest,
                "description": "cold recovery restored the durable incumbent"
            }),
        )?;
        let ledger = root.join("results.tsv");
        repair_torn_ledger_tail(&ledger)?;
        if !ledger_has_iteration(&ledger, iteration)? {
            append_ledger_file(
                &ledger,
                &LedgerRow {
                    iteration,
                    state: &incumbent.state.digest,
                    result: "recovered process crash",
                    status: "crash",
                    description: "cold recovery restored the durable incumbent",
                    evidence: &evidence_relative,
                },
            )?;
        }
    }
    state.phase = RunPhase::Interrupted;
    state.active_iteration = None;
    state.active_candidate_snapshot = None;
    state.prepared_iteration = None;
    state.blocker = None;
    atomic_json(&state_path, &state)?;
    Ok(())
}

fn recover_prepared_iteration(
    worktree: &Worktree,
    root: &Path,
    directory_id: &str,
    state: &mut RunState,
    prepared: PreparedIteration,
) -> Result<()> {
    if state.active_iteration != Some(prepared.iteration)
        || prepared.iteration == 0
        || prepared.iteration > state.requested_iterations
        || !matches!(
            prepared.status.as_str(),
            "keep" | "discard" | "crash" | "blocked" | "interrupted"
        )
    {
        return Err(anyhow!(
            "incomplete loop run `{directory_id}` has an invalid prepared iteration"
        ));
    }
    validate_relative(&prepared.evidence)?;
    let incumbent = load_run_snapshot(root, &state.incumbent_snapshot)?;
    let target = load_run_snapshot(root, &prepared.target_snapshot)?;
    if target.state.digest != prepared.state {
        return Err(anyhow!(
            "incomplete loop run `{directory_id}` has a prepared row bound to the wrong state"
        ));
    }
    let candidate = state
        .active_candidate_snapshot
        .as_deref()
        .map(|relative| load_run_snapshot(root, relative))
        .transpose()?;
    let matches_target = worktree.state_matches(root, &target)?;
    let matches_incumbent = worktree.state_matches(root, &incumbent)?;
    let matches_candidate = match candidate.as_ref() {
        Some(candidate) => worktree.state_matches(root, candidate)?,
        None => false,
    };
    if !matches_target {
        if !matches_incumbent && !matches_candidate {
            return Err(anyhow!(
                "incomplete loop run `{directory_id}` conflicts with repository changes made during publication; no files were overwritten"
            ));
        }
        worktree.restore(root, &target).with_context(|| {
            format!("failed to finish prepared quality-loop iteration in `{directory_id}`")
        })?;
    }

    let ledger = root.join("results.tsv");
    repair_torn_ledger_tail(&ledger)?;
    if !ledger_has_iteration(&ledger, prepared.iteration)? {
        append_ledger_file(
            &ledger,
            &LedgerRow {
                iteration: prepared.iteration,
                state: &prepared.state,
                result: &prepared.result,
                status: &prepared.status,
                description: &prepared.description,
                evidence: &prepared.evidence,
            },
        )?;
    }
    if prepared.status == "keep" {
        state.incumbent_snapshot = prepared.target_snapshot;
        state.final_result = Some(prepared.result.clone());
    }
    state.phase = if prepared.status == "blocked" {
        state.blocker = Some(prepared.description);
        RunPhase::Blocked
    } else {
        RunPhase::Interrupted
    };
    state.active_iteration = None;
    state.active_candidate_snapshot = None;
    state.prepared_iteration = None;
    atomic_json(&root.join("state.json"), state)
}

fn load_run_snapshot(
    root: &Path,
    relative: &str,
) -> Result<crate::quality_loop::RepositorySnapshot> {
    validate_relative(relative)?;
    if !Path::new(relative).starts_with("snapshots") {
        return Err(anyhow!(
            "loop recovery snapshot is outside the snapshot store"
        ));
    }
    let path = root.join(relative);
    let canonical_root = root.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(canonical_root.join("snapshots")) {
        return Err(anyhow!("loop recovery snapshot escapes its run directory"));
    }
    Worktree::load_snapshot(&path)
}

fn append_ledger_file(path: &Path, row: &LedgerRow<'_>) -> Result<()> {
    verify_private_regular_file(path)?;
    repair_torn_ledger_tail(path)?;
    if ledger_has_iteration(path, row.iteration)? {
        return Err(anyhow!(
            "results ledger already contains iteration {}",
            row.iteration
        ));
    }
    let line = [
        row.iteration.to_string(),
        escape_tsv(row.state),
        escape_tsv(row.result),
        escape_tsv(row.status),
        escape_tsv(row.description),
        escape_tsv(row.evidence),
    ]
    .join("\t");
    let mut file = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

fn ledger_has_iteration(path: &Path, iteration: usize) -> Result<bool> {
    let existing = std::fs::read_to_string(path)?;
    let rows = parse_ledger(&existing)?;
    Ok(rows.into_iter().any(|row| row == iteration))
}

fn parse_ledger(value: &str) -> Result<Vec<usize>> {
    let mut lines = value.lines();
    if lines.next() != Some("iteration\tstate\tresult\tstatus\tdescription\tevidence") {
        return Err(anyhow!("results ledger has an invalid header"));
    }
    let mut rows = Vec::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 6 {
            return Err(anyhow!("results ledger has a malformed row"));
        }
        let iteration = fields[0]
            .parse::<usize>()
            .context("results ledger has an invalid iteration")?;
        if rows.contains(&iteration) {
            return Err(anyhow!("results ledger repeats iteration {iteration}"));
        }
        for field in &fields[1..] {
            unescape_tsv(field)?;
        }
        rows.push(iteration);
    }
    Ok(rows)
}

fn prepare_loop_directories(root: &Path) -> Result<PathBuf> {
    let project = safe_component(root, ".bcodex")?;
    let loops = safe_component(&project, "loops")?;
    std::fs::set_permissions(&loops, std::fs::Permissions::from_mode(0o700))?;
    Ok(loops)
}

fn safe_component(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(anyhow!("unsafe quality-loop state path {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700).create(&path)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

fn acquire_lock(loops: &Path) -> Result<File> {
    let path = loops.join("worktree.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(anyhow!("unsafe quality-loop lock path {}", path.display()));
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        return Err(anyhow!("another quality loop already owns this worktree"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn write_lock_metadata(file: &mut File, run_id: &str) -> Result<()> {
    file.set_len(0)?;
    file.write_all(format!("pid={}\nrun={}\n", std::process::id(), run_id).as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn discard_incomplete_run(lock: &mut File, root: &Path) -> Result<()> {
    std::fs::remove_dir_all(root)
        .with_context(|| format!("failed to remove {}", root.display()))?;
    sync_directory(
        root.parent()
            .ok_or_else(|| anyhow!("loop run directory has no parent"))?,
    )?;
    lock.set_len(0)?;
    lock.sync_all()?;
    Ok(())
}

fn resolve_existing_file(root: &Path, value: &str, under: &Path) -> Result<PathBuf> {
    validate_relative(value)?;
    let path = root.join(value);
    let relative_under = under
        .strip_prefix(root)
        .context("authorized artifact workspace is outside the loop run")?;
    verify_private_directory_chain(root, relative_under, false)?;
    let parent = Path::new(value)
        .parent()
        .ok_or_else(|| anyhow!("run artifact has no parent"))?;
    verify_private_directory_chain(root, parent, false)?;
    let canonical_root = root.canonicalize()?;
    let canonical_under = under.canonicalize()?;
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve run artifact `{value}`"))?;
    if !canonical.starts_with(&canonical_root) || !canonical.starts_with(&canonical_under) {
        return Err(anyhow!("run artifact escapes its authorized workspace"));
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(anyhow!("run artifact must be a regular file"));
    }
    Ok(path)
}

pub(super) fn verify_private_directory_chain(
    root: &Path,
    relative: &Path,
    create: bool,
) -> Result<PathBuf> {
    if !relative.as_os_str().is_empty() {
        validate_relative(
            relative
                .to_str()
                .ok_or_else(|| anyhow!("quality-loop directory path is not UTF-8"))?,
        )?;
    }
    let root_metadata = std::fs::symlink_metadata(root)?;
    if !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || root_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(anyhow!(
            "unsafe quality-loop run directory {}",
            root.display()
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(anyhow!("invalid quality-loop directory path"));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.permissions().mode() & 0o077 == 0 => {}
            Ok(_) => {
                return Err(anyhow!(
                    "unsafe quality-loop directory {}",
                    current.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700).create(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn validate_relative(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() || value.len() > 1_024 {
        return Err(anyhow!("invalid run-relative path"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(anyhow!("invalid run-relative path")),
        }
    }
    Ok(())
}

fn relative_run_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .context("artifact is outside the loop run")?;
    let value = relative
        .to_str()
        .ok_or_else(|| anyhow!("loop artifact path is not UTF-8"))?
        .replace('\\', "/");
    validate_relative(&value)?;
    Ok(value)
}

fn repair_torn_ledger_tail(path: &Path) -> Result<()> {
    verify_private_regular_file(path)?;
    let bytes = std::fs::read(path)?;
    if bytes.ends_with(b"\n") {
        return Ok(());
    }
    let valid = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(valid as u64)?;
    file.sync_all()?;
    Ok(())
}

fn escape_tsv(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn unescape_tsv(value: &str) -> Result<String> {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            if character.is_control() {
                return Err(anyhow!(
                    "results ledger contains an unescaped control character"
                ));
            }
            output.push(character);
            continue;
        }
        let escaped = characters
            .next()
            .ok_or_else(|| anyhow!("results ledger ends with an incomplete escape"))?;
        output.push(match escaped {
            '\\' => '\\',
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            _ => return Err(anyhow!("results ledger contains an invalid escape")),
        });
    }
    Ok(output)
}

fn verify_private_regular_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(anyhow!("unsafe quality-loop file {}", path.display()));
    }
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    let temporary = parent.join(format!(".loop-write-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_directory(parent)?;
    }
    atomic_write(path, bytes)
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(anyhow!("unsafe quality-loop directory {}", path.display()));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn create_new_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(anyhow!("unsafe quality-loop directory {}", path.display()));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn build_identity() -> String {
    format!(
        "bettercodex-{}-{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("BETTERCODEX_BUILD_REVISION").unwrap_or("source")
    )
}

pub(crate) fn verify_runtime_state(state: &RunState) -> Result<()> {
    if state.protocol_version != LOOP_PROTOCOL_VERSION
        || state.contract_version != CONTRACT_VERSION
        || state.model != crate::MODEL
        || state.reasoning_effort != "max"
        || state.build_identity != build_identity()
        || state.evaluator_prompt_identity != digest(EVALUATOR_PROMPT.as_bytes())
        || state.worker_prompt_identity != digest(WORKER_PROMPT.as_bytes())
        || state.contract_prompt_identity != digest(CONTRACT_PROMPT.as_bytes())
    {
        return Err(anyhow!(
            "loop run identity is incompatible with this bettercodex build"
        ));
    }
    Ok(())
}
