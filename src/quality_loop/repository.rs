use crate::quality_loop::PathSpec;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

const MAX_SNAPSHOT_FILES: usize = 200_000;
const MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024 * 1024;
const LOOP_STATE_PREFIX: &str = ".bcodex/loops";

#[derive(Clone, Debug)]
pub(crate) struct Worktree {
    root: PathBuf,
    git_dir: PathBuf,
    common_git_dir: PathBuf,
    index_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RepositorySnapshot {
    pub(crate) id: String,
    pub(crate) state: StateIdentity,
    entries: BTreeMap<String, Entry>,
    pub(crate) included_specs: Vec<PathSpec>,
    git: GitState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct StateIdentity {
    pub(crate) digest: String,
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) index: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct GitState {
    head: Option<String>,
    branch: Option<String>,
    index_digest: Option<String>,
    refs: BTreeMap<String, String>,
    config_digest: String,
    hooks_digest: String,
    linked_worktrees_digest: String,
    submodule_repositories_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct Entry {
    kind: EntryKind,
    mode: u32,
    digest: Option<String>,
    symlink_target: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotDelta {
    pub(crate) created: Vec<String>,
    pub(crate) modified: Vec<String>,
    pub(crate) deleted: Vec<String>,
    pub(crate) git_changed: bool,
}

impl SnapshotDelta {
    pub(crate) fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && !self.git_changed
    }

    pub(crate) fn paths(&self) -> impl Iterator<Item = &str> {
        self.created
            .iter()
            .chain(&self.modified)
            .chain(&self.deleted)
            .map(String::as_str)
    }
}

impl Worktree {
    pub(crate) fn discover(cwd: &Path) -> Result<Self> {
        let root = git_path_output(cwd, &["rev-parse", "--show-toplevel"])
            .context("the quality loop requires an active Git worktree")?;
        let root = PathBuf::from(root)
            .canonicalize()
            .context("failed to resolve the Git worktree root")?;
        let git_dir = absolute_git_path(&root, &["rev-parse", "--absolute-git-dir"])?;
        let common_git_dir = absolute_git_path(&root, &["rev-parse", "--git-common-dir"])?;
        let index_path = absolute_git_path(&root, &["rev-parse", "--git-path", "index"])?;
        Ok(Self {
            root,
            git_dir,
            common_git_dir,
            index_path,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(test)]
    pub(crate) fn common_git_dir(&self) -> &Path {
        &self.common_git_dir
    }

    pub(crate) fn install_loop_exclude(&self) -> Result<()> {
        let exclude = absolute_git_path(&self.root, &["rev-parse", "--git-path", "info/exclude"])?;
        match std::fs::symlink_metadata(&exclude) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "unsafe repository-local Git exclusion {}",
                    exclude.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let existing = match std::fs::read(&exclude) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", exclude.display()));
            }
        };
        const ENTRY: &[u8] = b"/.bcodex/loops/";
        if existing
            .split(|byte| *byte == b'\n')
            .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == ENTRY)
        {
            return Ok(());
        }
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut updated = existing;
        if !updated.is_empty() && !updated.ends_with(b"\n") {
            updated.push(b'\n');
        }
        updated.extend_from_slice(ENTRY);
        updated.push(b'\n');
        atomic_write(&exclude, &updated, 0o600)
            .with_context(|| format!("failed to install loop exclusion in {}", exclude.display()))
    }

    pub(crate) fn reject_tracked_loop_state(&self) -> Result<()> {
        let output = git_output(&self.root, &["ls-files", "--", LOOP_STATE_PREFIX], None)?;
        if !output.stdout.is_empty() {
            return Err(anyhow!(
                "tracked path `{LOOP_STATE_PREFIX}` conflicts with quality-loop state"
            ));
        }
        Ok(())
    }

    pub(crate) fn capture(
        &self,
        run_root: &Path,
        included_specs: &[PathSpec],
    ) -> Result<RepositorySnapshot> {
        let blob_root = run_root.join("blobs");
        private_dir(&blob_root)?;
        let paths = self.inventory_paths(included_specs)?;
        if paths.len() > MAX_SNAPSHOT_FILES {
            return Err(anyhow!(
                "repository snapshot requires {} paths, exceeding the {MAX_SNAPSHOT_FILES}-path limit",
                paths.len()
            ));
        }
        self.validate_snapshot_size(&paths)?;
        let mut total_bytes = 0_u64;
        let mut entries = BTreeMap::new();
        for relative in paths {
            let absolute = self.root.join(&relative);
            let Some(entry) = capture_entry(&absolute, &blob_root, &mut total_bytes)
                .with_context(|| format!("failed to capture repository path `{relative}`"))?
            else {
                continue;
            };
            if total_bytes > MAX_SNAPSHOT_BYTES {
                return Err(anyhow!(
                    "repository snapshot exceeds the {MAX_SNAPSHOT_BYTES}-byte safety limit while capturing `{relative}`"
                ));
            }
            entries.insert(relative, entry);
        }
        let git = self.capture_git(&blob_root, &mut total_bytes)?;
        let digest = canonical_state_digest(&entries, &git)?;
        let state = StateIdentity {
            digest,
            head: git.head.clone(),
            branch: git.branch.clone(),
            index: git.index_digest.clone(),
        };
        Ok(RepositorySnapshot {
            id: Uuid::new_v4().to_string(),
            state,
            entries,
            included_specs: included_specs.to_vec(),
            git,
        })
    }

    pub(crate) fn save_snapshot(
        &self,
        run_root: &Path,
        snapshot: &RepositorySnapshot,
    ) -> Result<PathBuf> {
        let directory = run_root.join("snapshots");
        private_dir(&directory)?;
        let path = directory.join(format!("{}.json", snapshot.id));
        let bytes = serde_json::to_vec(snapshot)?;
        write_new_private(&path, &bytes)?;
        sync_directory(&directory)?;
        Ok(path)
    }

    pub(crate) fn load_snapshot(path: &Path) -> Result<RepositorySnapshot> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!("snapshot {} is not a regular file", path.display()));
        }
        serde_json::from_slice(&std::fs::read(path)?)
            .with_context(|| format!("invalid repository snapshot {}", path.display()))
    }

    pub(crate) fn delta(
        &self,
        before: &RepositorySnapshot,
        after: &RepositorySnapshot,
    ) -> SnapshotDelta {
        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();
        for (path, entry) in &after.entries {
            match before.entries.get(path) {
                None => created.push(path.clone()),
                Some(previous) if previous != entry => modified.push(path.clone()),
                Some(_) => {}
            }
        }
        for path in before.entries.keys() {
            if !after.entries.contains_key(path) {
                deleted.push(path.clone());
            }
        }
        SnapshotDelta {
            created,
            modified,
            deleted,
            git_changed: before.git != after.git,
        }
    }

    pub(crate) fn text_diff_counts(
        &self,
        run_root: &Path,
        before: &RepositorySnapshot,
        after: &RepositorySnapshot,
    ) -> Result<(u64, u64)> {
        let paths = before
            .entries
            .keys()
            .chain(after.entries.keys())
            .collect::<BTreeSet<_>>();
        let mut additions = 0_u64;
        let mut deletions = 0_u64;
        for path in paths {
            let old = before.entries.get(path);
            let new = after.entries.get(path);
            if old == new {
                continue;
            }
            match (old, new) {
                (Some(old), Some(new))
                    if old.kind == EntryKind::File && new.kind == EntryKind::File =>
                {
                    let old_digest = old
                        .digest
                        .as_deref()
                        .ok_or_else(|| anyhow!("snapshot file `{path}` omitted its blob"))?;
                    let new_digest = new
                        .digest
                        .as_deref()
                        .ok_or_else(|| anyhow!("snapshot file `{path}` omitted its blob"))?;
                    if old_digest != new_digest {
                        let (added, deleted) = modified_text_counts(
                            &blob_path(run_root, old_digest)?,
                            &blob_path(run_root, new_digest)?,
                        )?;
                        additions = additions.saturating_add(added);
                        deletions = deletions.saturating_add(deleted);
                    }
                }
                (Some(old), Some(new)) => {
                    if old.kind == EntryKind::File {
                        deletions = deletions.saturating_add(blob_line_count(
                            run_root,
                            old.digest.as_deref().ok_or_else(|| {
                                anyhow!("snapshot file `{path}` omitted its blob")
                            })?,
                        )?);
                    }
                    if new.kind == EntryKind::File {
                        additions = additions.saturating_add(blob_line_count(
                            run_root,
                            new.digest.as_deref().ok_or_else(|| {
                                anyhow!("snapshot file `{path}` omitted its blob")
                            })?,
                        )?);
                    }
                }
                (Some(old), None) if old.kind == EntryKind::File => {
                    deletions = deletions.saturating_add(blob_line_count(
                        run_root,
                        old.digest
                            .as_deref()
                            .ok_or_else(|| anyhow!("snapshot file `{path}` omitted its blob"))?,
                    )?);
                }
                (None, Some(new)) if new.kind == EntryKind::File => {
                    additions = additions.saturating_add(blob_line_count(
                        run_root,
                        new.digest
                            .as_deref()
                            .ok_or_else(|| anyhow!("snapshot file `{path}` omitted its blob"))?,
                    )?);
                }
                _ => {}
            }
        }
        Ok((additions, deletions))
    }

    pub(crate) fn verify_candidate_boundary(
        &self,
        before: &RepositorySnapshot,
        after: &RepositorySnapshot,
        candidate_paths: &[PathSpec],
    ) -> Result<SnapshotDelta> {
        let delta = self.delta(before, after);
        let outside = delta
            .paths()
            .filter(|path| {
                !candidate_paths
                    .iter()
                    .any(|spec| spec.covers(Path::new(path)))
            })
            .take(16)
            .collect::<Vec<_>>();
        if !outside.is_empty() {
            return Err(anyhow!(
                "candidate changed paths outside its boundary: {}",
                outside.join(", ")
            ));
        }
        self.verify_supported_git_change(&before.git, &after.git)?;
        Ok(delta)
    }

    pub(crate) fn restore(&self, run_root: &Path, snapshot: &RepositorySnapshot) -> Result<()> {
        let current = self.capture(run_root, &snapshot.included_specs)?;
        self.restore_git(run_root, &snapshot.git, &current.git)?;

        let mut removals = current
            .entries
            .iter()
            .filter(|(path, entry)| snapshot.entries.get(*path) != Some(*entry))
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        removals.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for relative in removals {
            remove_path(&self.root.join(relative))?;
        }

        let blob_root = run_root.join("blobs");
        let changed = snapshot
            .entries
            .iter()
            .filter(|(path, entry)| current.entries.get(*path) != Some(*entry))
            .collect::<Vec<_>>();
        for (relative, entry) in changed
            .iter()
            .copied()
            .filter(|(_, entry)| entry.kind == EntryKind::Directory)
        {
            restore_entry(&self.root.join(relative), entry, &blob_root)?;
        }
        for (relative, entry) in changed
            .iter()
            .copied()
            .filter(|(_, entry)| entry.kind != EntryKind::Directory)
        {
            restore_entry(&self.root.join(relative), entry, &blob_root)?;
        }
        // Directory modes are restored after children so temporary writable
        // permissions used for materialization cannot leak into the incumbent.
        for (relative, entry) in changed
            .into_iter()
            .filter(|(_, entry)| entry.kind == EntryKind::Directory)
        {
            std::fs::set_permissions(
                self.root.join(relative),
                std::fs::Permissions::from_mode(entry.mode),
            )?;
        }

        let restored = self.capture(run_root, &snapshot.included_specs)?;
        if restored.state != snapshot.state || restored.entries != snapshot.entries {
            return Err(anyhow!(
                "repository restoration did not reproduce incumbent state {}",
                snapshot.state.digest
            ));
        }
        Ok(())
    }

    pub(crate) fn state_matches(
        &self,
        run_root: &Path,
        snapshot: &RepositorySnapshot,
    ) -> Result<bool> {
        let current = self.capture(run_root, &snapshot.included_specs)?;
        Ok(current.state == snapshot.state)
    }

    pub(crate) fn restore_paths(
        &self,
        run_root: &Path,
        snapshot: &RepositorySnapshot,
        paths: &[PathSpec],
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let current = self.capture(run_root, &snapshot.included_specs)?;
        let selected = |path: &str| paths.iter().any(|spec| spec.covers(Path::new(path)));
        let mut removals = current
            .entries
            .iter()
            .filter(|(path, entry)| selected(path) && snapshot.entries.get(*path) != Some(*entry))
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        removals.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for relative in removals {
            remove_path(&self.root.join(relative))?;
        }
        let blob_root = run_root.join("blobs");
        let changed = snapshot
            .entries
            .iter()
            .filter(|(path, entry)| selected(path) && current.entries.get(*path) != Some(*entry))
            .collect::<Vec<_>>();
        for (relative, entry) in changed
            .iter()
            .copied()
            .filter(|(_, entry)| entry.kind == EntryKind::Directory)
        {
            restore_entry(&self.root.join(relative), entry, &blob_root)?;
        }
        for (relative, entry) in changed
            .iter()
            .copied()
            .filter(|(_, entry)| entry.kind != EntryKind::Directory)
        {
            restore_entry(&self.root.join(relative), entry, &blob_root)?;
        }
        for (relative, entry) in changed
            .into_iter()
            .filter(|(_, entry)| entry.kind == EntryKind::Directory)
        {
            std::fs::set_permissions(
                self.root.join(relative),
                std::fs::Permissions::from_mode(entry.mode),
            )?;
        }
        let restored = self.capture(run_root, &snapshot.included_specs)?;
        for path in snapshot.entries.keys().chain(restored.entries.keys()) {
            if selected(path) && snapshot.entries.get(path) != restored.entries.get(path) {
                return Err(anyhow!(
                    "selective repository restoration failed for `{path}`"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn is_ignored(&self, relative: &str) -> Result<bool> {
        let status = Command::new("git")
            .current_dir(&self.root)
            .args(["check-ignore", "-q", "--", relative])
            .status()?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(anyhow!("git check-ignore failed for `{relative}`")),
        }
    }

    fn inventory_paths(&self, included_specs: &[PathSpec]) -> Result<BTreeSet<String>> {
        let ordinary = git_output(
            &self.root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
            None,
        )?;
        let ignored = git_output(
            &self.root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
            ],
            None,
        )?;
        let mut paths = BTreeSet::new();
        for raw in ordinary
            .stdout
            .split(|byte| *byte == 0)
            .chain(ignored.stdout.split(|byte| *byte == 0))
            .filter(|path| !path.is_empty())
        {
            let path = std::str::from_utf8(raw)
                .context("the quality loop cannot preserve a non-UTF-8 repository path")?;
            if !is_loop_state_path(path) {
                paths.insert(path.to_string());
            }
        }
        for spec in included_specs {
            collect_spec_paths(&self.root, spec, &mut paths)?;
        }
        let leaves = paths.iter().cloned().collect::<Vec<_>>();
        for leaf in leaves {
            let mut parent = Path::new(&leaf).parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                let relative = path.to_string_lossy().replace('\\', "/");
                if relative != ".bcodex" && !is_loop_state_path(&relative) {
                    paths.insert(relative);
                }
                parent = path.parent();
            }
        }
        Ok(paths)
    }

    fn validate_snapshot_size(&self, paths: &BTreeSet<String>) -> Result<()> {
        let index_bytes = snapshot_entry_size(&self.index_path)
            .context("failed to inspect the Git index for the repository snapshot")?;
        let mut total_bytes = index_bytes;
        let mut bytes_by_component = BTreeMap::new();
        if index_bytes > 0 {
            bytes_by_component.insert("Git index", index_bytes);
        }
        for relative in paths {
            let bytes = snapshot_entry_size(&self.root.join(relative))
                .with_context(|| format!("failed to size repository snapshot path `{relative}`"))?;
            total_bytes = total_bytes.saturating_add(bytes);
            let component = relative.split('/').next().unwrap_or(relative);
            let component_bytes = bytes_by_component.entry(component).or_insert(0_u64);
            *component_bytes = component_bytes.saturating_add(bytes);
        }
        if total_bytes <= MAX_SNAPSHOT_BYTES {
            return Ok(());
        }
        let (largest, largest_bytes) = bytes_by_component
            .into_iter()
            .max_by_key(|(_, bytes)| *bytes)
            .unwrap_or(("repository metadata", 0));
        Err(anyhow!(
            "repository snapshot requires {total_bytes} bytes, exceeding the {MAX_SNAPSHOT_BYTES}-byte safety limit; `{largest}` accounts for {largest_bytes} bytes; remove obsolete ignored build output or move non-candidate data out of the worktree before retrying"
        ))
    }

    fn capture_git(&self, blob_root: &Path, total_bytes: &mut u64) -> Result<GitState> {
        let head = optional_git_text(&self.root, &["rev-parse", "--verify", "HEAD"])?;
        let branch = optional_git_text(&self.root, &["symbolic-ref", "-q", "HEAD"])?;
        let index_digest = capture_optional_file(&self.index_path, blob_root, total_bytes)
            .context("failed to capture the Git index")?;
        if *total_bytes > MAX_SNAPSHOT_BYTES {
            return Err(anyhow!(
                "repository snapshot exceeds the {MAX_SNAPSHOT_BYTES}-byte safety limit"
            ));
        }
        let refs = capture_refs(&self.root)?;
        let config_digest = digest_optional_files(&[
            self.common_git_dir.join("config"),
            self.git_dir.join("config.worktree"),
        ])?;
        let hooks_digest = digest_tree(&self.common_git_dir.join("hooks"))?;
        let linked_worktrees_digest =
            digest_other_worktree_admin(&self.common_git_dir.join("worktrees"), &self.git_dir)?;
        let submodule_repositories_digest = digest_tree(&self.common_git_dir.join("modules"))?;
        Ok(GitState {
            head,
            branch,
            index_digest,
            refs,
            config_digest,
            hooks_digest,
            linked_worktrees_digest,
            submodule_repositories_digest,
        })
    }

    fn verify_supported_git_change(&self, before: &GitState, after: &GitState) -> Result<()> {
        if before.config_digest != after.config_digest {
            return Err(anyhow!("candidate changed repository Git configuration"));
        }
        if before.hooks_digest != after.hooks_digest {
            return Err(anyhow!("candidate changed repository Git hooks"));
        }
        if before.linked_worktrees_digest != after.linked_worktrees_digest {
            return Err(anyhow!("candidate changed another linked worktree"));
        }
        if before.submodule_repositories_digest != after.submodule_repositories_digest {
            return Err(anyhow!("candidate changed a submodule repository"));
        }
        let allowed = [before.branch.as_deref(), after.branch.as_deref()]
            .into_iter()
            .flatten()
            .collect::<HashSetRef>();
        let names = before
            .refs
            .keys()
            .chain(after.refs.keys())
            .collect::<BTreeSet<_>>();
        let unsupported = names
            .into_iter()
            .filter(|name| !allowed.contains(name.as_str()))
            .filter(|name| before.refs.get(*name) != after.refs.get(*name))
            .take(8)
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(anyhow!(
                "candidate changed unsupported Git refs: {}",
                unsupported.join(", ")
            ));
        }
        Ok(())
    }

    fn restore_git(&self, run_root: &Path, wanted: &GitState, current: &GitState) -> Result<()> {
        self.verify_supported_git_change(wanted, current)?;
        let allowed = [wanted.branch.as_deref(), current.branch.as_deref()]
            .into_iter()
            .flatten()
            .collect::<HashSetRef>();
        let names = wanted
            .refs
            .keys()
            .chain(current.refs.keys())
            .collect::<BTreeSet<_>>();
        for name in names {
            if !allowed.contains(name.as_str()) || wanted.refs.get(name) == current.refs.get(name) {
                continue;
            }
            match wanted.refs.get(name) {
                Some(object) => {
                    git_output(&self.root, &["update-ref", name, object], None)?;
                }
                None => {
                    git_output(&self.root, &["update-ref", "-d", name], None)?;
                }
            }
        }
        match (&wanted.branch, &wanted.head) {
            (Some(branch), Some(head)) => {
                git_output(&self.root, &["update-ref", branch, head], None)?;
                git_output(&self.root, &["symbolic-ref", "HEAD", branch], None)?;
            }
            (Some(branch), None) => {
                git_output(&self.root, &["update-ref", "-d", branch], None)?;
                git_output(&self.root, &["symbolic-ref", "HEAD", branch], None)?;
            }
            (None, Some(head)) => {
                git_output(
                    &self.root,
                    &["update-ref", "--no-deref", "HEAD", head],
                    None,
                )?;
            }
            (None, None) => return Err(anyhow!("cannot restore Git state without HEAD or branch")),
        }
        match &wanted.index_digest {
            Some(digest) => {
                let index = read_blob(run_root, digest)?;
                atomic_write(&self.index_path, &index, 0o600).with_context(|| {
                    format!("failed to restore Git index {}", self.index_path.display())
                })
            }
            None => remove_path(&self.index_path).with_context(|| {
                format!("failed to remove Git index {}", self.index_path.display())
            }),
        }
    }
}

type HashSetRef<'a> = std::collections::HashSet<&'a str>;

fn collect_spec_paths(root: &Path, spec: &PathSpec, output: &mut BTreeSet<String>) -> Result<()> {
    let relative = spec.path();
    let absolute = root.join(&relative);
    match std::fs::symlink_metadata(&absolute) {
        Ok(metadata) => {
            output.insert(spec.root().to_string());
            if spec.is_tree() && metadata.is_dir() && !metadata.file_type().is_symlink() {
                collect_directory(root, &absolute, output)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", absolute.display()));
        }
    }
    Ok(())
}

fn collect_directory(root: &Path, directory: &Path, output: &mut BTreeSet<String>) -> Result<()> {
    let mut entries = std::fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked path remains under worktree")
            .to_str()
            .ok_or_else(|| anyhow!("quality loop cannot preserve a non-UTF-8 path"))?
            .to_string();
        if is_loop_state_path(&relative) {
            continue;
        }
        output.insert(relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_directory(root, &path, output)?;
        }
    }
    Ok(())
}

fn capture_entry(path: &Path, blob_root: &Path, total_bytes: &mut u64) -> Result<Option<Entry>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    let mode = metadata.permissions().mode() & 0o7777;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        let bytes = target.into_os_string().into_vec();
        *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        return Ok(Some(Entry {
            kind: EntryKind::Symlink,
            mode,
            digest: None,
            symlink_target: Some(STANDARD.encode(bytes)),
        }));
    }
    if metadata.is_dir() {
        return Ok(Some(Entry {
            kind: EntryKind::Directory,
            mode,
            digest: None,
            symlink_target: None,
        }));
    }
    if !metadata.is_file() {
        return Err(anyhow!(
            "unsupported filesystem object in candidate state: {}",
            path.display()
        ));
    }
    let digest = capture_file(path, blob_root, total_bytes)?;
    Ok(Some(Entry {
        kind: EntryKind::File,
        mode,
        digest: Some(digest),
        symlink_target: None,
    }))
}

fn snapshot_entry_size(path: &Path) -> Result<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(std::fs::read_link(path)?.into_os_string().into_vec().len() as u64);
    }
    Ok(if metadata.is_file() {
        metadata.len()
    } else {
        0
    })
}

fn capture_file(path: &Path, blob_root: &Path, total_bytes: &mut u64) -> Result<String> {
    let file = File::open(path)?;
    let remaining = MAX_SNAPSHOT_BYTES.saturating_sub(*total_bytes);
    if file.metadata()?.len() > remaining {
        return Err(anyhow!(
            "repository snapshot exceeds the {MAX_SNAPSHOT_BYTES}-byte safety limit"
        ));
    }
    let mut bytes = Vec::new();
    file.take(remaining.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > remaining {
        return Err(anyhow!(
            "repository snapshot exceeds the {MAX_SNAPSHOT_BYTES}-byte safety limit"
        ));
    }
    *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
    store_blob(blob_root, &bytes)
}

fn capture_optional_file(
    path: &Path,
    blob_root: &Path,
    total_bytes: &mut u64,
) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            capture_file(path, blob_root, total_bytes).map(Some)
        }
        Ok(_) => Err(anyhow!(
            "Git state path {} is not a regular file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn store_blob(root: &Path, bytes: &[u8]) -> Result<String> {
    let digest = hash_bytes(bytes);
    let directory = root.join(&digest[..2]);
    private_dir(&directory)?;
    let path = directory.join(&digest[2..]);
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            sync_directory(&directory)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if hash_bytes(&std::fs::read(&path)?) != digest {
                return Err(anyhow!("content-addressed snapshot blob is corrupt"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(digest)
}

fn read_blob(run_root: &Path, digest: &str) -> Result<Vec<u8>> {
    let path = blob_path(run_root, digest)?;
    let bytes = std::fs::read(&path)
        .with_context(|| format!("failed to read snapshot blob {}", path.display()))?;
    if hash_bytes(&bytes) != digest {
        return Err(anyhow!(
            "snapshot blob {} failed integrity validation",
            path.display()
        ));
    }
    Ok(bytes)
}

fn blob_path(run_root: &Path, digest: &str) -> Result<PathBuf> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid snapshot blob identity"));
    }
    Ok(run_root.join("blobs").join(&digest[..2]).join(&digest[2..]))
}

fn blob_line_count(run_root: &Path, digest: &str) -> Result<u64> {
    let bytes = read_blob(run_root, digest)?;
    if bytes.contains(&0) || bytes.is_empty() {
        return Ok(0);
    }
    Ok(bytes.iter().filter(|byte| **byte == b'\n').count() as u64
        + u64::from(!bytes.ends_with(b"\n")))
}

fn modified_text_counts(before: &Path, after: &Path) -> Result<(u64, u64)> {
    let output = Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ])
        .arg(before)
        .arg(after)
        .output()
        .context("failed to calculate quality-loop text diff")?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(anyhow!(
            "git diff --no-index failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.len() > MAX_COMMAND_OUTPUT || output.stderr.len() > MAX_COMMAND_OUTPUT {
        return Err(anyhow!("Git diff output exceeded the loop safety limit"));
    }
    let Some(line) = output
        .stdout
        .split(|byte| *byte == b'\n')
        .find(|line| !line.is_empty())
    else {
        return Ok((0, 0));
    };
    let mut fields = line.split(|byte| *byte == b'\t');
    let added = fields.next().unwrap_or_default();
    let deleted = fields.next().unwrap_or_default();
    if added == b"-" || deleted == b"-" {
        return Ok((0, 0));
    }
    let added = std::str::from_utf8(added)?.parse::<u64>()?;
    let deleted = std::str::from_utf8(deleted)?.parse::<u64>()?;
    Ok((added, deleted))
}

fn restore_entry(path: &Path, entry: &Entry, blob_root: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_path(path)?;
    match entry.kind {
        EntryKind::Directory => {
            std::fs::create_dir(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        EntryKind::File => {
            let digest = entry
                .digest
                .as_deref()
                .ok_or_else(|| anyhow!("file entry omitted blob"))?;
            let run_root = blob_root
                .parent()
                .ok_or_else(|| anyhow!("blob root has no parent"))?;
            atomic_write(path, &read_blob(run_root, digest)?, entry.mode)?;
        }
        EntryKind::Symlink => {
            let encoded = entry
                .symlink_target
                .as_deref()
                .ok_or_else(|| anyhow!("symlink entry omitted target"))?;
            let target = OsString::from_vec(STANDARD.decode(encoded)?);
            std::os::unix::fs::symlink(target, path)?;
        }
    }
    if entry.kind == EntryKind::File {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(entry.mode))?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)?;
        }
        Ok(_) => std::fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn capture_refs(root: &Path) -> Result<BTreeMap<String, String>> {
    let output = git_output(
        root,
        &["for-each-ref", "--format=%(refname)%00%(objectname)"],
        None,
    )?;
    let mut refs = BTreeMap::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Some(separator) = line.iter().position(|byte| *byte == 0) else {
            continue;
        };
        let (name, object) = line.split_at(separator);
        let object = &object[1..];
        if name.is_empty() || object.is_empty() {
            continue;
        }
        refs.insert(
            String::from_utf8(name.to_vec())?,
            String::from_utf8(object.to_vec())?,
        );
    }
    Ok(refs)
}

fn digest_optional_files(paths: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.as_os_str().as_encoded_bytes());
        match std::fs::read(path) {
            Ok(bytes) => {
                hasher.update([1]);
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update([0]),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest_other_worktree_admin(root: &Path, current_git_dir: &Path) -> Result<String> {
    let current = current_git_dir.canonicalize().ok();
    let mut entries = BTreeMap::new();
    let children = match std::fs::read_dir(root) {
        Ok(children) => children.collect::<std::result::Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for child in children {
        let path = child.path();
        if path.canonicalize().ok().as_ref() == current.as_ref() {
            continue;
        }
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("linked-worktree metadata path is not UTF-8"))?;
        entries.insert(name, digest_tree(&path)?);
    }
    Ok(hash_bytes(&serde_json::to_vec(&entries)?))
}

fn digest_tree(root: &Path) -> Result<String> {
    let mut entries = BTreeMap::new();
    if root.exists() {
        collect_tree_digest(root, root, &mut entries)?;
    }
    Ok(hash_bytes(&serde_json::to_vec(&entries)?))
}

fn collect_tree_digest(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root)?.to_string_lossy().to_string();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            entries.insert(
                relative,
                format!("link:{}", std::fs::read_link(path)?.display()),
            );
        } else if metadata.is_dir() {
            entries.insert(
                relative.clone(),
                format!("dir:{:o}", metadata.permissions().mode()),
            );
            collect_tree_digest(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.insert(
                relative,
                format!("file:{}", hash_bytes(&std::fs::read(path)?)),
            );
        }
    }
    Ok(())
}

fn canonical_state_digest(entries: &BTreeMap<String, Entry>, git: &GitState) -> Result<String> {
    Ok(hash_bytes(&serde_json::to_vec(&(entries, git))?))
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_loop_state_path(path: &str) -> bool {
    path == LOOP_STATE_PREFIX
        || path
            .strip_prefix(LOOP_STATE_PREFIX)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

struct CommandOutput {
    stdout: Vec<u8>,
}

fn git_output(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<CommandOutput> {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    if stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn().context("failed to start Git")?;
    if let Some(stdin) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Git stdin was unavailable"))?
            .write_all(stdin)?;
    }
    let output = child.wait_with_output()?;
    if output.stdout.len() > MAX_COMMAND_OUTPUT || output.stderr.len() > MAX_COMMAND_OUTPUT {
        return Err(anyhow!("Git command output exceeded the loop safety limit"));
    }
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(CommandOutput {
        stdout: output.stdout,
    })
}

fn git_path_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(root, args, None)?;
    let value = String::from_utf8(output.stdout)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("Git returned an empty path"));
    }
    Ok(value.to_string())
}

fn absolute_git_path(root: &Path, args: &[&str]) -> Result<PathBuf> {
    let path = PathBuf::from(git_path_output(root, args)?);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn optional_git_text(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout)?.trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

fn private_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(anyhow!("{} is not a private directory", path.display()));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
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

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".loop-write-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("bettercodex-loop-repository-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            git(&root, &["init", "-q"]);
            git(&root, &["config", "user.name", "Loop Test"]);
            git(&root, &["config", "user.email", "loop@example.invalid"]);
            std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
            std::fs::write(root.join("staged.txt"), "base\n").unwrap();
            std::fs::write(root.join("deleted.txt"), "base\n").unwrap();
            std::fs::write(root.join(".gitignore"), ".cache/\n").unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-qm", "base"]);
            Self { root }
        }

        fn run_root(&self) -> PathBuf {
            let root = self
                .root
                .join(".bcodex/loops")
                .join(Uuid::new_v4().to_string());
            std::fs::create_dir_all(root.join("blobs")).unwrap();
            std::fs::create_dir_all(root.join("snapshots")).unwrap();
            root
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn dirty_fixture(fixture: &Fixture) {
        std::fs::write(fixture.root.join("staged.txt"), "staged operator work\n").unwrap();
        git(&fixture.root, &["add", "staged.txt"]);
        std::fs::write(fixture.root.join("tracked.txt"), "unstaged operator work\n").unwrap();
        std::fs::remove_file(fixture.root.join("deleted.txt")).unwrap();
        std::fs::write(fixture.root.join("untracked.txt"), "operator untracked\n").unwrap();
        std::fs::create_dir_all(fixture.root.join(".cache")).unwrap();
        std::fs::write(fixture.root.join(".cache/value"), b"operator ignored\0data").unwrap();
        std::os::unix::fs::symlink("tracked.txt", fixture.root.join("operator-link")).unwrap();
    }

    #[test]
    fn restores_dirty_tracked_untracked_ignored_symlink_and_index_state_exactly() {
        let fixture = Fixture::new();
        dirty_fixture(&fixture);
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        let before = worktree.capture(&run_root, &[]).unwrap();

        std::fs::write(fixture.root.join("tracked.txt"), "worker overwrite\n").unwrap();
        std::fs::write(fixture.root.join("staged.txt"), "worker staged overwrite\n").unwrap();
        git(&fixture.root, &["add", "tracked.txt", "staged.txt"]);
        std::fs::write(fixture.root.join("deleted.txt"), "worker resurrected\n").unwrap();
        std::fs::remove_file(fixture.root.join("untracked.txt")).unwrap();
        std::fs::write(fixture.root.join(".cache/value"), b"worker cache").unwrap();
        std::fs::write(fixture.root.join(".cache/new"), b"new cache").unwrap();
        std::fs::remove_file(fixture.root.join("operator-link")).unwrap();
        std::fs::write(fixture.root.join("worker.txt"), "new\n").unwrap();

        worktree.restore(&run_root, &before).unwrap();
        let restored = worktree.capture(&run_root, &[]).unwrap();
        assert_eq!(restored.state, before.state);
        assert_eq!(restored.entries, before.entries);
        assert_eq!(
            std::fs::read(fixture.root.join(".cache/value")).unwrap(),
            b"operator ignored\0data"
        );
        assert!(!fixture.root.join(".cache/new").exists());
        assert_eq!(
            std::fs::read_link(fixture.root.join("operator-link")).unwrap(),
            PathBuf::from("tracked.txt")
        );
        assert!(!fixture.root.join("worker.txt").exists());
        let staged = git(&fixture.root, &["diff", "--cached", "--", "staged.txt"]);
        assert!(staged.contains("-base"), "{staged}");
        assert!(staged.contains("+staged operator work"), "{staged}");
    }

    #[test]
    fn identities_cover_bytes_modes_symlink_targets_index_and_head_but_not_run_state() {
        let fixture = Fixture::new();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        std::os::unix::fs::symlink("tracked.txt", fixture.root.join("link")).unwrap();
        let baseline = worktree.capture(&run_root, &[]).unwrap();

        std::fs::write(run_root.join("diagnostic"), "ignored control state").unwrap();
        assert_eq!(
            worktree.capture(&run_root, &[]).unwrap().state,
            baseline.state
        );

        let mut permissions = std::fs::metadata(fixture.root.join("tracked.txt"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(fixture.root.join("tracked.txt"), permissions).unwrap();
        assert_ne!(
            worktree.capture(&run_root, &[]).unwrap().state,
            baseline.state
        );
        worktree.restore(&run_root, &baseline).unwrap();

        std::fs::remove_file(fixture.root.join("link")).unwrap();
        std::os::unix::fs::symlink("staged.txt", fixture.root.join("link")).unwrap();
        assert_ne!(
            worktree.capture(&run_root, &[]).unwrap().state,
            baseline.state
        );
        worktree.restore(&run_root, &baseline).unwrap();

        std::fs::write(fixture.root.join("tracked.txt"), "index-only\n").unwrap();
        git(&fixture.root, &["add", "tracked.txt"]);
        std::fs::write(fixture.root.join("tracked.txt"), "base\n").unwrap();
        assert_ne!(
            worktree.capture(&run_root, &[]).unwrap().state,
            baseline.state
        );
        worktree.restore(&run_root, &baseline).unwrap();

        std::fs::write(fixture.root.join("tracked.txt"), "commit\n").unwrap();
        git(&fixture.root, &["add", "tracked.txt"]);
        git(&fixture.root, &["commit", "-qm", "move head"]);
        assert_ne!(
            worktree.capture(&run_root, &[]).unwrap().state,
            baseline.state
        );
    }

    #[test]
    fn unsupported_git_configuration_hook_and_ref_changes_are_blocking() {
        let fixture = Fixture::new();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        let before = worktree.capture(&run_root, &[]).unwrap();

        git(&fixture.root, &["update-ref", "refs/tags/worker", "HEAD"]);
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert!(
            worktree
                .verify_candidate_boundary(
                    &before,
                    &after,
                    &[PathSpec::try_from("tracked.txt".to_string()).unwrap()]
                )
                .is_err()
        );

        git(&fixture.root, &["update-ref", "-d", "refs/tags/worker"]);
        git(&fixture.root, &["config", "loop.worker", "changed"]);
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert!(
            worktree
                .verify_supported_git_change(&before.git, &after.git)
                .is_err()
        );

        git(&fixture.root, &["config", "--unset", "loop.worker"]);
        std::fs::write(
            worktree.common_git_dir().join("hooks/pre-commit"),
            "#!/bin/sh\n",
        )
        .unwrap();
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert!(
            worktree
                .verify_supported_git_change(&before.git, &after.git)
                .is_err()
        );

        std::fs::remove_file(worktree.common_git_dir().join("hooks/pre-commit")).unwrap();
        let linked = worktree.common_git_dir().join("worktrees/other");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert!(
            worktree
                .verify_supported_git_change(&before.git, &after.git)
                .is_err()
        );

        std::fs::remove_dir_all(worktree.common_git_dir().join("worktrees")).unwrap();
        let submodule = worktree.common_git_dir().join("modules/component");
        std::fs::create_dir_all(&submodule).unwrap();
        std::fs::write(submodule.join("HEAD"), "changed\n").unwrap();
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert!(
            worktree
                .verify_supported_git_change(&before.git, &after.git)
                .is_err()
        );
    }

    #[test]
    fn cumulative_text_diff_is_relative_to_the_dirty_starting_state() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("tracked.txt"), "operator\nsecond\n").unwrap();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        let before = worktree.capture(&run_root, &[]).unwrap();
        std::fs::write(
            fixture.root.join("tracked.txt"),
            "operator\nreplacement\nthird\n",
        )
        .unwrap();
        std::fs::write(fixture.root.join("new.txt"), "one\ntwo\n").unwrap();
        let after = worktree.capture(&run_root, &[]).unwrap();
        assert_eq!(
            worktree
                .text_diff_counts(&run_root, &before, &after)
                .unwrap(),
            (4, 1)
        );
    }

    #[test]
    fn repository_exclude_is_idempotent_and_preserves_existing_bytes() {
        let fixture = Fixture::new();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        let exclude = PathBuf::from(git(
            &fixture.root,
            &["rev-parse", "--git-path", "info/exclude"],
        ));
        let exclude = if exclude.is_absolute() {
            exclude
        } else {
            fixture.root.join(exclude)
        };
        std::fs::write(&exclude, b"# existing without newline").unwrap();
        worktree.install_loop_exclude().unwrap();
        let once = std::fs::read(&exclude).unwrap();
        worktree.install_loop_exclude().unwrap();
        assert_eq!(std::fs::read(&exclude).unwrap(), once);
        assert_eq!(once, b"# existing without newline\n/.bcodex/loops/\n");
        assert_eq!(
            std::fs::read(fixture.root.join(".gitignore")).unwrap(),
            b".cache/\n"
        );

        let unsafe_fixture = Fixture::new();
        let unsafe_worktree = Worktree::discover(&unsafe_fixture.root).unwrap();
        let exclude = unsafe_fixture.root.join(".git/info/exclude");
        std::fs::remove_file(&exclude).unwrap();
        std::os::unix::fs::symlink("../config", &exclude).unwrap();
        assert!(unsafe_worktree.install_loop_exclude().is_err());
    }

    #[test]
    fn snapshot_rejects_an_oversized_sparse_file_before_reading_it() {
        let fixture = Fixture::new();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        let sparse = std::fs::File::create(fixture.root.join("oversized.bin")).unwrap();
        sparse.set_len(MAX_SNAPSHOT_BYTES + 1).unwrap();
        let error = worktree.capture(&run_root, &[]).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("oversized.bin"), "{message}");
        assert!(
            message.contains(&MAX_SNAPSHOT_BYTES.to_string()),
            "{message}"
        );
        assert_eq!(
            std::fs::read_dir(run_root.join("blobs")).unwrap().count(),
            0
        );
    }

    #[test]
    fn snapshot_rejects_an_oversized_ignored_aggregate_before_writing_blobs() {
        let fixture = Fixture::new();
        let worktree = Worktree::discover(&fixture.root).unwrap();
        worktree.install_loop_exclude().unwrap();
        let run_root = fixture.run_root();
        std::fs::write(fixture.root.join(".gitignore"), ".cache/\ntarget/\n").unwrap();
        std::fs::create_dir(fixture.root.join("target")).unwrap();
        for name in ["large-a.bin", "large-b.bin"] {
            let sparse = std::fs::File::create(fixture.root.join("target").join(name)).unwrap();
            sparse.set_len(MAX_SNAPSHOT_BYTES / 2 + 1).unwrap();
        }

        let error = worktree.capture(&run_root, &[]).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("repository snapshot requires"),
            "{message}"
        );
        assert!(
            message.contains(&MAX_SNAPSHOT_BYTES.to_string()),
            "{message}"
        );
        assert!(message.contains("`target` accounts"), "{message}");
        assert_eq!(
            std::fs::read_dir(run_root.join("blobs")).unwrap().count(),
            0
        );
    }
}
