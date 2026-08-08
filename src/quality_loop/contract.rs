use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

const MAX_CONTRACT_BYTES: u64 = 256 * 1024;
const MAX_PROMISES: usize = 64;
const MAX_CHECKS: usize = 64;
const MAX_PATHS: usize = 256;
const MAX_ARGUMENTS: usize = 64;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_FIELD_CHARS: usize = 4_096;
const MAX_PATH_BYTES: usize = 512;
const MAX_TIMEOUT_SECONDS: u64 = 3_600;
const MAX_BASELINE_REPEATS: u8 = 10;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvaluatorContract {
    pub(crate) version: u32,
    pub(crate) loop_name: String,
    pub(crate) promises: Vec<Promise>,
    pub(crate) candidate_paths: Vec<PathSpec>,
    pub(crate) fixed_constraints: Vec<String>,
    pub(crate) integrity_paths: Vec<PathSpec>,
    pub(crate) scratch_paths: Vec<PathSpec>,
    pub(crate) machine_checks: Vec<MachineCheck>,
    pub(crate) discrimination_checks: Vec<DiscriminationCheck>,
    pub(crate) model_checks: Vec<ModelCheck>,
    pub(crate) acceptance: AcceptanceRule,
    pub(crate) comparison: ComparisonRule,
    pub(crate) environment: Vec<String>,
    pub(crate) uncovered: Vec<String>,
    pub(crate) known_loopholes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Promise {
    pub(crate) id: String,
    pub(crate) class: PromiseClass,
    pub(crate) statement: String,
    pub(crate) failure_mode: String,
    pub(crate) method: String,
    pub(crate) required_evidence: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromiseClass {
    Acceptance,
    Improvement,
    Regression,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct PathSpec(String);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MachineCheck {
    pub(crate) id: String,
    pub(crate) promise_ids: Vec<String>,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) input_paths: Vec<ArtifactPath>,
    pub(crate) fixture_paths: Vec<ArtifactPath>,
    pub(crate) timeout_seconds: u64,
    pub(crate) resource_budget: String,
    pub(crate) side_effects: SideEffects,
    pub(crate) approval: Approval,
    pub(crate) expected_exit_codes: Vec<i32>,
    pub(crate) extract: ExtractRule,
    pub(crate) baseline_repeats: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscriminationCheck {
    pub(crate) linked_check_id: String,
    pub(crate) check: MachineCheck,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactPath {
    pub(crate) root: ArtifactRoot,
    pub(crate) path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactRoot {
    Worktree,
    Evaluator,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SideEffects {
    None,
    DeclaredScratch,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Approval {
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtractRule {
    pub(crate) kind: ExtractKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) json_pointer: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtractKind {
    Pass,
    LastLine,
    JsonNumber,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCheck {
    pub(crate) id: String,
    pub(crate) promise_ids: Vec<String>,
    pub(crate) kind: ModelCheckKind,
    pub(crate) rubric_path: String,
    pub(crate) required_artifacts: Vec<String>,
    pub(crate) calibration_paths: Vec<String>,
    pub(crate) output_shape: String,
    pub(crate) hard_gate: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelCheckKind {
    PassFail,
    Pairwise,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcceptanceRule {
    pub(crate) required_check_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComparisonRule {
    pub(crate) kind: ComparisonKind,
    pub(crate) check_id: Option<String>,
    pub(crate) direction: Option<MetricDirection>,
    pub(crate) minimum_delta: f64,
    pub(crate) tolerance: f64,
    pub(crate) ties: DiscardOutcome,
    pub(crate) inconclusive: DiscardOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComparisonKind {
    AcceptanceTransition,
    Metric,
    Pairwise,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricDirection {
    Higher,
    Lower,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiscardOutcome {
    Discard,
}

impl PathSpec {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_tree(&self) -> bool {
        self.0.ends_with("/**")
    }

    pub(crate) fn root(&self) -> &str {
        self.0.strip_suffix("/**").unwrap_or(&self.0)
    }

    pub(crate) fn covers(&self, relative: &Path) -> bool {
        if self.root() == "." {
            return true;
        }
        let candidate = slash_path(relative);
        candidate == self.root()
            || self.is_tree()
                && candidate
                    .strip_prefix(self.root())
                    .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        self.root() == "."
            || other.root() == "."
            || self.root() == other.root()
            || self.is_tree()
                && other
                    .root()
                    .strip_prefix(self.root())
                    .is_some_and(|suffix| suffix.starts_with('/'))
            || other.is_tree()
                && self
                    .root()
                    .strip_prefix(other.root())
                    .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub(crate) fn path(&self) -> PathBuf {
        PathBuf::from(self.root())
    }
}

impl TryFrom<String> for PathSpec {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        validate_relative_path(&value, true).map_err(|error| error.to_string())?;
        Ok(Self(value))
    }
}

impl From<PathSpec> for String {
    fn from(value: PathSpec) -> Self {
        value.0
    }
}

impl EvaluatorContract {
    pub(crate) fn load(path: &Path, worktree_root: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect evaluator contract {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!("evaluator contract must be a regular file"));
        }
        if metadata.len() > MAX_CONTRACT_BYTES {
            return Err(anyhow!(
                "evaluator contract exceeds the {MAX_CONTRACT_BYTES}-byte limit"
            ));
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read evaluator contract {}", path.display()))?;
        let contract: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid evaluator contract {}", path.display()))?;
        contract.validate(worktree_root)?;
        Ok(contract)
    }

    pub(crate) fn validate(&self, worktree_root: &Path) -> Result<()> {
        if self.version != 1 {
            return Err(anyhow!(
                "unsupported evaluator contract version {}",
                self.version
            ));
        }
        validate_loop_name(&self.loop_name)?;
        bounded_count("promises", self.promises.len(), 1, MAX_PROMISES)?;
        bounded_count(
            "checks",
            self.machine_checks.len() + self.discrimination_checks.len() + self.model_checks.len(),
            1,
            MAX_CHECKS,
        )?;
        bounded_count("candidate paths", self.candidate_paths.len(), 1, MAX_PATHS)?;
        if self.integrity_paths.len() + self.scratch_paths.len() > MAX_PATHS {
            return Err(anyhow!(
                "evaluator contract declares too many protected paths"
            ));
        }

        let mut promise_ids = HashSet::new();
        for promise in &self.promises {
            validate_id("promise", &promise.id)?;
            if !promise_ids.insert(promise.id.as_str()) {
                return Err(anyhow!("duplicate promise ID `{}`", promise.id));
            }
            validate_text("promise statement", &promise.statement, false)?;
            validate_text("promise failure mode", &promise.failure_mode, false)?;
            validate_text("promise method", &promise.method, false)?;
            validate_text_list("required evidence", &promise.required_evidence)?;
        }
        validate_text_list("fixed constraints", &self.fixed_constraints)?;
        validate_text_list("environment", &self.environment)?;
        validate_text_list("uncovered properties", &self.uncovered)?;
        validate_text_list("known loopholes", &self.known_loopholes)?;

        validate_path_sets(self)?;
        for path in self
            .candidate_paths
            .iter()
            .chain(&self.integrity_paths)
            .chain(&self.scratch_paths)
        {
            validate_no_symlink_escape(worktree_root, path)?;
        }

        let mut check_ids = HashSet::new();
        for check in &self.machine_checks {
            validate_machine_check(self, worktree_root, check, &promise_ids, &mut check_ids)?;
        }
        for check in &self.model_checks {
            validate_check_identity(&check.id, &check.promise_ids, &promise_ids, &mut check_ids)?;
            validate_relative_path(&check.rubric_path, false)?;
            bounded_count(
                "model-check artifacts",
                check.required_artifacts.len(),
                1,
                MAX_PATHS,
            )?;
            validate_text_list("model-check artifacts", &check.required_artifacts)?;
            if check.calibration_paths.len() > MAX_PATHS {
                return Err(anyhow!(
                    "model check `{}` declares too many calibration paths",
                    check.id
                ));
            }
            for path in &check.calibration_paths {
                validate_relative_path(path, false)?;
            }
            validate_text("model-check output shape", &check.output_shape, false)?;
            if check.hard_gate && check.calibration_paths.is_empty() {
                return Err(anyhow!(
                    "model check `{}` cannot be a hard gate without calibration evidence",
                    check.id
                ));
            }
        }
        let selectable_check_ids = check_ids.clone();
        for discrimination in &self.discrimination_checks {
            let linked = self
                .machine_checks
                .iter()
                .find(|check| check.id == discrimination.linked_check_id)
                .ok_or_else(|| {
                    anyhow!(
                        "discrimination check `{}` references unknown machine check `{}`",
                        discrimination.check.id,
                        discrimination.linked_check_id
                    )
                })?;
            let check = &discrimination.check;
            validate_machine_check(self, worktree_root, check, &promise_ids, &mut check_ids)?;
            if check.promise_ids.iter().collect::<HashSet<_>>()
                != linked.promise_ids.iter().collect::<HashSet<_>>()
            {
                return Err(anyhow!(
                    "discrimination check `{}` must cover the same promises as `{}`",
                    check.id,
                    linked.id
                ));
            }
            if check.argv.first() != linked.argv.first() {
                return Err(anyhow!(
                    "discrimination check `{}` must exercise the same runner as `{}`",
                    check.id,
                    linked.id
                ));
            }
            if check.baseline_repeats != 1 {
                return Err(anyhow!(
                    "discrimination check `{}` must run exactly once",
                    check.id
                ));
            }
            if !check
                .fixture_paths
                .iter()
                .any(|fixture| fixture.root == ArtifactRoot::Evaluator)
            {
                return Err(anyhow!(
                    "discrimination check `{}` requires a frozen evaluator failure fixture",
                    check.id
                ));
            }
        }

        bounded_count(
            "required checks",
            self.acceptance.required_check_ids.len(),
            1,
            MAX_CHECKS,
        )?;
        for id in &self.acceptance.required_check_ids {
            if !selectable_check_ids.contains(id.as_str()) {
                return Err(anyhow!("acceptance rule references unknown check `{id}`"));
            }
            if let Some(model_check) = self.model_checks.iter().find(|check| check.id == *id)
                && !model_check.hard_gate
            {
                return Err(anyhow!(
                    "acceptance rule cannot require advisory model check `{id}`"
                ));
            }
        }
        if !self.comparison.minimum_delta.is_finite()
            || !self.comparison.tolerance.is_finite()
            || self.comparison.minimum_delta < 0.0
            || self.comparison.tolerance < 0.0
        {
            return Err(anyhow!(
                "comparison thresholds must be finite and non-negative"
            ));
        }
        match self.comparison.kind {
            ComparisonKind::AcceptanceTransition => {
                if self.comparison.check_id.is_some()
                    || self.comparison.direction.is_some()
                    || self.comparison.minimum_delta != 0.0
                    || self.comparison.tolerance != 0.0
                {
                    return Err(anyhow!(
                        "acceptance-transition comparison may not name a metric or numeric threshold"
                    ));
                }
            }
            ComparisonKind::Metric => {
                let id = self
                    .comparison
                    .check_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("metric comparison must name a machine check"))?;
                let check = self
                    .machine_checks
                    .iter()
                    .find(|check| check.id == id)
                    .ok_or_else(|| anyhow!("metric comparison references unknown check `{id}`"))?;
                if check.extract.kind != ExtractKind::JsonNumber
                    || self.comparison.direction.is_none()
                {
                    return Err(anyhow!(
                        "metric comparison requires a numeric extractor and direction"
                    ));
                }
            }
            ComparisonKind::Pairwise => {
                let id = self
                    .comparison
                    .check_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("pairwise comparison must name a model check"))?;
                let check = self
                    .model_checks
                    .iter()
                    .find(|check| check.id == id)
                    .ok_or_else(|| {
                        anyhow!("pairwise comparison references unknown check `{id}`")
                    })?;
                if !matches!(check.kind, ModelCheckKind::Pairwise)
                    || check.calibration_paths.len() < 2
                    || self.comparison.direction.is_some()
                    || self.comparison.minimum_delta != 0.0
                    || self.comparison.tolerance != 0.0
                {
                    return Err(anyhow!(
                        "pairwise comparison requires a calibrated pairwise model check and zero numeric thresholds"
                    ));
                }
            }
        }
        let mut decisive_machine_checks = self
            .acceptance
            .required_check_ids
            .iter()
            .filter(|id| {
                self.machine_checks
                    .iter()
                    .any(|check| check.id == id.as_str())
            })
            .map(String::as_str)
            .collect::<HashSet<_>>();
        if self.comparison.kind == ComparisonKind::Metric
            && let Some(id) = self.comparison.check_id.as_deref()
        {
            decisive_machine_checks.insert(id);
        }
        for id in decisive_machine_checks {
            if !self
                .discrimination_checks
                .iter()
                .any(|probe| probe.linked_check_id == id)
            {
                return Err(anyhow!(
                    "decisive machine check `{id}` has no frozen discrimination probe"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn path_is_candidate(&self, relative: &Path) -> bool {
        self.candidate_paths
            .iter()
            .any(|path| path.covers(relative))
    }

    pub(crate) fn snapshot_paths(&self) -> Vec<PathSpec> {
        self.candidate_paths
            .iter()
            .chain(&self.integrity_paths)
            .chain(&self.scratch_paths)
            .cloned()
            .collect()
    }
}

fn validate_machine_check(
    contract: &EvaluatorContract,
    worktree_root: &Path,
    check: &MachineCheck,
    promise_ids: &HashSet<&str>,
    check_ids: &mut HashSet<String>,
) -> Result<()> {
    validate_check_identity(&check.id, &check.promise_ids, promise_ids, check_ids)?;
    bounded_count("check arguments", check.argv.len(), 1, MAX_ARGUMENTS)?;
    for argument in &check.argv {
        validate_text("check argument", argument, false)?;
        if argument.contains('\0') {
            return Err(anyhow!("check arguments may not contain NUL bytes"));
        }
    }
    validate_relative_path(&check.cwd, false)?;
    validate_check_cwd(worktree_root, &check.cwd)?;
    bounded_count("check input paths", check.input_paths.len(), 1, MAX_PATHS)?;
    if check.fixture_paths.len() > MAX_PATHS {
        return Err(anyhow!(
            "machine check `{}` declares too many fixtures",
            check.id
        ));
    }
    for input in &check.input_paths {
        validate_artifact_path(contract, worktree_root, input, false)?;
    }
    for fixture in &check.fixture_paths {
        validate_artifact_path(contract, worktree_root, fixture, true)?;
    }
    if check.env.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(anyhow!(
            "machine check `{}` declares too much environment",
            check.id
        ));
    }
    for (name, value) in &check.env {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.as_bytes()[0].is_ascii_digit()
        {
            return Err(anyhow!(
                "machine check `{}` has invalid environment name",
                check.id
            ));
        }
        validate_text("environment value", value, true)?;
    }
    if !(1..=MAX_TIMEOUT_SECONDS).contains(&check.timeout_seconds) {
        return Err(anyhow!(
            "machine check `{}` has an invalid timeout",
            check.id
        ));
    }
    if !(1..=MAX_BASELINE_REPEATS).contains(&check.baseline_repeats) {
        return Err(anyhow!(
            "machine check `{}` has an invalid repeat count",
            check.id
        ));
    }
    if check.side_effects == SideEffects::DeclaredScratch && contract.scratch_paths.is_empty() {
        return Err(anyhow!(
            "machine check `{}` declares scratch side effects without a scratch path",
            check.id
        ));
    }
    validate_text("resource budget", &check.resource_budget, false)?;
    bounded_count(
        "expected exit codes",
        check.expected_exit_codes.len(),
        1,
        16,
    )?;
    if check.extract.kind == ExtractKind::JsonNumber {
        let pointer = check
            .extract
            .json_pointer
            .as_deref()
            .ok_or_else(|| anyhow!("machine check `{}` needs a JSON pointer", check.id))?;
        if !pointer.starts_with('/') || pointer.len() > MAX_PATH_BYTES {
            return Err(anyhow!(
                "machine check `{}` has an invalid JSON pointer",
                check.id
            ));
        }
    } else if check.extract.json_pointer.is_some() {
        return Err(anyhow!(
            "machine check `{}` supplies a JSON pointer for a non-JSON extractor",
            check.id
        ));
    }
    Ok(())
}

fn validate_artifact_path(
    contract: &EvaluatorContract,
    worktree_root: &Path,
    artifact: &ArtifactPath,
    fixture: bool,
) -> Result<()> {
    validate_relative_path(&artifact.path, false)?;
    if artifact.root == ArtifactRoot::Evaluator {
        if !artifact.path.starts_with("evaluator/workspace/") {
            return Err(anyhow!(
                "evaluator artifact `{}` is outside evaluator/workspace",
                artifact.path
            ));
        }
        return Ok(());
    }
    let relative = Path::new(&artifact.path);
    let candidate = contract
        .candidate_paths
        .iter()
        .any(|path| path.covers(relative));
    let integrity = contract
        .integrity_paths
        .iter()
        .any(|path| path.covers(relative));
    if fixture && !integrity {
        return Err(anyhow!(
            "worktree fixture `{}` is not protected by the integrity boundary",
            artifact.path
        ));
    }
    if !candidate && !integrity {
        return Err(anyhow!(
            "worktree input `{}` is outside the candidate and integrity boundaries",
            artifact.path
        ));
    }
    validate_no_symlink_escape(worktree_root, &PathSpec(artifact.path.clone()))?;
    match std::fs::symlink_metadata(worktree_root.join(relative)) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "worktree evaluator input `{}` is a symlink",
            artifact.path
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !fixture => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(anyhow!(
            "worktree fixture `{}` does not exist",
            artifact.path
        )),
        Err(error) => Err(error.into()),
    }
}

fn validate_path_sets(contract: &EvaluatorContract) -> Result<()> {
    for path in &contract.candidate_paths {
        if path.root() == "."
            || path.covers(Path::new(".bcodex/loops"))
            || PathSpec(".bcodex/loops/**".to_string()).overlaps(path)
        {
            return Err(anyhow!("candidate boundary overlaps loop control state"));
        }
    }
    for (left_name, left, right_name, right) in [
        (
            "candidate",
            contract.candidate_paths.as_slice(),
            "integrity",
            contract.integrity_paths.as_slice(),
        ),
        (
            "candidate",
            contract.candidate_paths.as_slice(),
            "scratch",
            contract.scratch_paths.as_slice(),
        ),
        (
            "integrity",
            contract.integrity_paths.as_slice(),
            "scratch",
            contract.scratch_paths.as_slice(),
        ),
    ] {
        for left_path in left {
            for right_path in right {
                if left_path.overlaps(right_path) {
                    return Err(anyhow!(
                        "{left_name} path `{}` overlaps {right_name} path `{}`",
                        left_path.as_str(),
                        right_path.as_str()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_check_identity(
    id: &str,
    references: &[String],
    promises: &HashSet<&str>,
    checks: &mut HashSet<String>,
) -> Result<()> {
    validate_id("check", id)?;
    if !checks.insert(id.to_string()) {
        return Err(anyhow!("duplicate check ID `{id}`"));
    }
    bounded_count("promise references", references.len(), 1, MAX_PROMISES)?;
    for reference in references {
        if !promises.contains(reference.as_str()) {
            return Err(anyhow!(
                "check `{id}` references unknown promise `{reference}`"
            ));
        }
    }
    Ok(())
}

fn validate_loop_name(name: &str) -> Result<()> {
    if !(1..=2).contains(&name.split_whitespace().count())
        || name.chars().any(char::is_control)
        || name.contains('│')
        || UnicodeWidthStr::width(name) > 32
    {
        return Err(anyhow!(
            "loop name must be one or two control-free words using at most 32 display cells"
        ));
    }
    Ok(())
}

fn validate_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!("invalid {kind} ID `{id}`"));
    }
    Ok(())
}

fn validate_text(kind: &str, value: &str, allow_empty: bool) -> Result<()> {
    if (!allow_empty && value.trim().is_empty()) || value.chars().count() > MAX_FIELD_CHARS {
        return Err(anyhow!("{kind} is empty or exceeds the field limit"));
    }
    if value.contains('\0') {
        return Err(anyhow!("{kind} contains a NUL byte"));
    }
    Ok(())
}

fn validate_text_list(kind: &str, values: &[String]) -> Result<()> {
    if values.len() > MAX_PROMISES {
        return Err(anyhow!("{kind} contains too many entries"));
    }
    for value in values {
        validate_text(kind, value, false)?;
    }
    Ok(())
}

fn bounded_count(kind: &str, actual: usize, minimum: usize, maximum: usize) -> Result<()> {
    if !(minimum..=maximum).contains(&actual) {
        return Err(anyhow!(
            "{kind} count {actual} is outside the supported {minimum}..={maximum} range"
        ));
    }
    Ok(())
}

fn validate_relative_path(value: &str, allow_tree: bool) -> Result<()> {
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.contains('\0') {
        return Err(anyhow!("path is empty or exceeds the path limit"));
    }
    if value.contains('\\') {
        return Err(anyhow!("paths must use `/` separators"));
    }
    let normalized = if allow_tree {
        value.strip_suffix("/**").unwrap_or(value)
    } else {
        if value.ends_with("/**") {
            return Err(anyhow!("tree suffix is not valid for this path"));
        }
        value
    };
    let path = Path::new(normalized);
    if path.is_absolute()
        || normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.ends_with('/')
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) || matches!(component, Component::CurDir) && normalized != "."
        })
    {
        return Err(anyhow!("path `{value}` is not a normalized relative path"));
    }
    Ok(())
}

fn validate_no_symlink_escape(root: &Path, spec: &PathSpec) -> Result<()> {
    let mut current = root.to_path_buf();
    let components = spec.path();
    let count = components.components().count();
    for (index, component) in components.components().enumerate() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = current.canonicalize().with_context(|| {
                    format!("path `{}` names an unresolved symlink", spec.as_str())
                })?;
                if !target.starts_with(root) {
                    return Err(anyhow!(
                        "path `{}` escapes the worktree through symlink {}",
                        spec.as_str(),
                        current.display()
                    ));
                }
                if index + 1 < count || spec.is_tree() {
                    return Err(anyhow!(
                        "path `{}` traverses symlink {}",
                        spec.as_str(),
                        current.display()
                    ));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn validate_check_cwd(root: &Path, relative: &str) -> Result<()> {
    let root = root.canonicalize()?;
    let path = root.join(relative);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("machine-check working directory `{relative}` is unavailable"))?;
    if !canonical.starts_with(&root) {
        return Err(anyhow!(
            "machine-check working directory `{relative}` escapes the candidate root"
        ));
    }
    let metadata = std::fs::metadata(&canonical)?;
    if !metadata.is_dir() {
        return Err(anyhow!(
            "machine-check working directory `{relative}` is not a directory"
        ));
    }
    Ok(())
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            Component::CurDir => None,
            _ => Some(component.as_os_str().to_string_lossy()),
        })
        .collect::<Vec<_>>()
        .join("/")
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "bettercodex-loop-contract-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    fn valid(root: &Path) -> EvaluatorContract {
        let contract: EvaluatorContract = serde_json::from_value(json!({
            "version": 1,
            "loop_name": "Parser speed",
            "promises": [{
                "id": "behavior",
                "class": "acceptance",
                "statement": "the parser accepts valid input",
                "failure_mode": "valid input is rejected",
                "method": "local command",
                "required_evidence": ["command result"]
            }],
            "candidate_paths": ["src/**"],
            "fixed_constraints": ["no network"],
            "integrity_paths": [],
            "scratch_paths": [],
            "machine_checks": [{
                "id": "behavior-check",
                "promise_ids": ["behavior"],
                "argv": ["/bin/true"],
                "cwd": ".",
                "env": {},
                "input_paths": [{"root": "worktree", "path": "src"}],
                "fixture_paths": [],
                "timeout_seconds": 5,
                "resource_budget": "one local process",
                "side_effects": "none",
                "approval": "none",
                "expected_exit_codes": [0],
                "extract": {"kind": "pass"},
                "baseline_repeats": 1
            }],
            "discrimination_checks": [{
                "linked_check_id": "behavior-check",
                "check": {
                    "id": "behavior-known-failure",
                    "promise_ids": ["behavior"],
                    "argv": ["/bin/true"],
                    "cwd": ".",
                    "env": {},
                    "input_paths": [{"root": "worktree", "path": "src"}],
                    "fixture_paths": [{
                        "root": "evaluator",
                        "path": "evaluator/workspace/known-failure.txt"
                    }],
                    "timeout_seconds": 5,
                    "resource_budget": "one local process",
                    "side_effects": "none",
                    "approval": "none",
                    "expected_exit_codes": [0],
                    "extract": {"kind": "pass"},
                    "baseline_repeats": 1
                }
            }],
            "model_checks": [],
            "acceptance": {"required_check_ids": ["behavior-check"]},
            "comparison": {
                "kind": "acceptance_transition",
                "check_id": null,
                "direction": null,
                "minimum_delta": 0.0,
                "tolerance": 0.0,
                "ties": "discard",
                "inconclusive": "discard"
            },
            "environment": ["local fixture"],
            "uncovered": ["none"],
            "known_loopholes": ["none"]
        }))
        .unwrap();
        contract.validate(root).unwrap();
        contract
    }

    #[test]
    fn valid_contract_is_accepted_and_unknown_fields_are_rejected() {
        let root = fixture();
        let contract = valid(&root);
        assert_eq!(contract.loop_name, "Parser speed");
        let mut value = serde_json::to_value(contract).unwrap();
        value["unexpected"] = json!(true);
        assert!(serde_json::from_value::<EvaluatorContract>(value).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_sets_are_closed_and_disjoint() {
        let root = fixture();
        let mut contract = valid(&root);
        contract.candidate_paths = vec![PathSpec::try_from(".".to_string()).unwrap()];
        assert!(contract.validate(&root).is_err());

        let mut contract = valid(&root);
        contract.integrity_paths = vec![PathSpec::try_from("src/oracle.rs".to_string()).unwrap()];
        assert!(contract.validate(&root).is_err());

        let mut contract = valid(&root);
        contract.scratch_paths = vec![PathSpec::try_from("src/cache/**".to_string()).unwrap()];
        assert!(contract.validate(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn check_working_directory_cannot_follow_a_symlink_outside_the_worktree() {
        let root = fixture();
        symlink("/tmp", root.join("outside")).unwrap();
        let mut contract = valid(&root);
        contract.machine_checks[0].cwd = "outside".to_string();
        assert!(contract.validate(&root).is_err());

        let mut contract = valid(&root);
        contract.candidate_paths = vec![PathSpec::try_from("outside".to_string()).unwrap()];
        assert!(contract.validate(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uncalibrated_model_opinion_cannot_become_a_gate() {
        let root = fixture();
        let mut contract = valid(&root);
        contract.model_checks = serde_json::from_value(json!([{
            "id": "judge",
            "promise_ids": ["behavior"],
            "kind": "pass_fail",
            "rubric_path": "evaluator/workspace/rubric.md",
            "required_artifacts": ["rendered output"],
            "calibration_paths": [],
            "output_shape": "boolean verdict with artifact citations",
            "hard_gate": true
        }]))
        .unwrap();
        contract
            .acceptance
            .required_check_ids
            .push("judge".to_string());
        assert!(contract.validate(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_decisive_machine_gate_requires_a_linked_frozen_failure_probe() {
        let root = fixture();
        let mut contract = valid(&root);
        contract.discrimination_checks.clear();
        assert!(contract.validate(&root).is_err());

        let mut contract = valid(&root);
        contract.discrimination_checks[0].check.argv = vec!["/bin/false".to_string()];
        assert!(contract.validate(&root).is_err());

        let mut contract = valid(&root);
        contract.discrimination_checks[0]
            .check
            .fixture_paths
            .clear();
        assert!(contract.validate(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
