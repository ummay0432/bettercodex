use super::contract::ComparisonKind;
use super::contract::ExtractKind;
use super::contract::MachineCheck;
use super::contract::MetricDirection;
use crate::quality_loop::EvaluatorContract;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::spawn_pipe_process_no_stdin;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_CHECK_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECISIVE_CHARS: usize = 512;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct EvaluationReport {
    pub(crate) state: String,
    pub(crate) accepted: bool,
    pub(crate) decisive: String,
    pub(crate) checks: BTreeMap<String, CheckReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CheckReport {
    pub(crate) passed: bool,
    pub(crate) values: Vec<CheckValue>,
    pub(crate) evidence: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum CheckValue {
    Text(String),
    Number(f64),
    Pass(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ImprovementDecision {
    Better(String),
    NotBetter(String),
    Inconclusive(String),
}

enum CommandTermination {
    Status(i32),
    TimedOut,
    Cancelled,
}

pub(crate) async fn run_machine_evaluation(
    contract: &EvaluatorContract,
    worktree_root: &Path,
    state: &str,
    evidence_directory: &Path,
    command_environment: &HashMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Option<EvaluationReport>> {
    create_private_directory(evidence_directory)?;
    let mut reports = BTreeMap::new();
    for check in &contract.machine_checks {
        let Some(report) = run_check(
            check,
            worktree_root,
            evidence_directory,
            command_environment,
            cancellation,
        )
        .await?
        else {
            return Ok(None);
        };
        reports.insert(check.id.clone(), report);
    }
    // The harness does not relabel semantic model judgment as a reproduced
    // machine result. Advisory checks stay visible; hard gates fail closed until
    // a phase artifact supplies a calibrated judgment.
    for check in &contract.model_checks {
        reports.insert(
            check.id.clone(),
            CheckReport {
                passed: !check.hard_gate,
                values: vec![CheckValue::Text(
                    "model judgment not reproduced by harness".to_string(),
                )],
                evidence: Vec::new(),
            },
        );
    }
    let accepted = contract
        .acceptance
        .required_check_ids
        .iter()
        .all(|id| reports.get(id).is_some_and(|report| report.passed));
    let decisive = decisive_result(contract, &reports, accepted);
    let report = EvaluationReport {
        state: state.to_string(),
        accepted,
        decisive,
        checks: reports,
    };
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    super::state::atomic_private_write(&evidence_directory.join("evaluation.json"), &bytes)?;
    Ok(Some(report))
}

pub(crate) async fn run_discrimination_checks(
    contract: &EvaluatorContract,
    worktree_root: &Path,
    evidence_directory: &Path,
    command_environment: &HashMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Option<BTreeMap<String, CheckReport>>> {
    create_private_directory(evidence_directory)?;
    let mut reports = BTreeMap::new();
    for discrimination in &contract.discrimination_checks {
        let check = &discrimination.check;
        let Some(report) = run_check(
            check,
            worktree_root,
            evidence_directory,
            command_environment,
            cancellation,
        )
        .await?
        else {
            return Ok(None);
        };
        reports.insert(check.id.clone(), report);
    }
    let mut bytes = serde_json::to_vec_pretty(&reports)?;
    bytes.push(b'\n');
    super::state::atomic_private_write(&evidence_directory.join("evaluation.json"), &bytes)?;
    Ok(Some(reports))
}

pub(crate) fn compare_reports(
    contract: &EvaluatorContract,
    incumbent: &EvaluationReport,
    candidate: &EvaluationReport,
) -> ImprovementDecision {
    if !candidate.accepted {
        return ImprovementDecision::NotBetter("acceptance checks failed".to_string());
    }
    match contract.comparison.kind {
        ComparisonKind::AcceptanceTransition => {
            if !incumbent.accepted {
                ImprovementDecision::Better("acceptance transition".to_string())
            } else {
                ImprovementDecision::NotBetter("no acceptance transition".to_string())
            }
        }
        ComparisonKind::Metric => compare_metric(contract, incumbent, candidate),
        ComparisonKind::Pairwise => {
            let Some(id) = contract.comparison.check_id.as_deref() else {
                return ImprovementDecision::Inconclusive(
                    "pairwise comparison check is missing".to_string(),
                );
            };
            match candidate.checks.get(id) {
                Some(report) if report.passed => {
                    ImprovementDecision::Better(format!("{id} preferred the candidate"))
                }
                Some(_) => ImprovementDecision::NotBetter(format!(
                    "{id} did not prefer the candidate in both positions"
                )),
                None => ImprovementDecision::Inconclusive(format!(
                    "pairwise check `{id}` produced no judgment"
                )),
            }
        }
    }
}

pub(crate) fn validate_structured_artifact(
    contract: &EvaluatorContract,
    artifact_path: &Path,
    expected_state: &str,
) -> Result<()> {
    load_structured_artifact(contract, artifact_path, expected_state).map(|_| ())
}

pub(crate) fn apply_structured_artifact(
    contract: &EvaluatorContract,
    report: &mut EvaluationReport,
    artifact_path: &Path,
) -> Result<()> {
    let artifact = load_structured_artifact(contract, artifact_path, &report.state)?;
    let checks = artifact
        .get("checks")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("validated phase evidence lost its check results"))?;
    for check in &contract.machine_checks {
        let value = checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("validated phase evidence lost check `{}`", check.id))?;
        let claimed = judgment_passed(value).ok_or_else(|| {
            anyhow!(
                "validated machine evidence lost the verdict for `{}`",
                check.id
            )
        })?;
        let observed = report
            .checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("harness result omitted machine check `{}`", check.id))?;
        if observed.passed != claimed {
            return Err(anyhow!(
                "phase evidence contradicts harness result for `{}`",
                check.id
            ));
        }
    }
    for check in &contract.model_checks {
        let value = checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("validated phase evidence lost model check `{}`", check.id))?;
        let passed = judgment_passed(value).ok_or_else(|| {
            anyhow!(
                "validated model evidence lost the verdict for `{}`",
                check.id
            )
        })?;
        let artifacts = value
            .as_object()
            .and_then(|value| value.get("artifacts"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("validated model evidence lost artifacts for `{}`", check.id))?;
        report.checks.insert(
            check.id.clone(),
            CheckReport {
                passed,
                values: vec![CheckValue::Pass(passed)],
                evidence: artifacts
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            },
        );
    }
    report.accepted = contract
        .acceptance
        .required_check_ids
        .iter()
        .all(|id| report.checks.get(id).is_some_and(|check| check.passed));
    report.decisive = decisive_result(contract, &report.checks, report.accepted);
    Ok(())
}

fn load_structured_artifact(
    contract: &EvaluatorContract,
    artifact_path: &Path,
    expected_state: &str,
) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(artifact_path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(anyhow!("phase evidence must be a regular JSON file"));
    }
    if metadata.len() > MAX_CHECK_OUTPUT_BYTES as u64 {
        return Err(anyhow!("phase evidence exceeds the evidence size limit"));
    }
    let artifact: Value = serde_json::from_slice(&std::fs::read(artifact_path)?)
        .context("phase evidence is not valid JSON")?;
    let state = artifact
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("phase evidence omitted its state identity"))?;
    if state != expected_state {
        return Err(anyhow!(
            "phase evidence is bound to stale state `{state}` instead of `{expected_state}`"
        ));
    }
    let checks = artifact
        .get("checks")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("phase evidence omitted its check results"))?;
    let expected_ids = contract
        .machine_checks
        .iter()
        .map(|check| check.id.as_str())
        .chain(contract.model_checks.iter().map(|check| check.id.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let supplied_ids = checks
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if supplied_ids != expected_ids {
        return Err(anyhow!(
            "phase evidence check IDs do not exactly match the frozen contract"
        ));
    }
    for check in &contract.machine_checks {
        let value = checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("phase evidence omitted check `{}`", check.id))?;
        judgment_passed(value)
            .ok_or_else(|| anyhow!("machine check `{}` omitted a boolean verdict", check.id))?;
    }
    for check in &contract.model_checks {
        let value = checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("phase evidence omitted model check `{}`", check.id))?;
        judgment_passed(value)
            .ok_or_else(|| anyhow!("model check `{}` omitted a boolean verdict", check.id))?;
        let artifacts = value
            .as_object()
            .and_then(|value| value.get("artifacts"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("model check `{}` omitted artifact evidence", check.id))?;
        if artifacts.len() < check.required_artifacts.len()
            || artifacts.len() > 256
            || artifacts.iter().any(|artifact| {
                artifact.as_str().is_none_or(|artifact| {
                    artifact.trim().is_empty()
                        || artifact.chars().count() > 4_096
                        || artifact.chars().any(char::is_control)
                })
            })
        {
            return Err(anyhow!(
                "model check `{}` has incomplete artifact evidence",
                check.id
            ));
        }
        if (check.hard_gate || matches!(check.kind, super::contract::ModelCheckKind::Pairwise))
            && value
                .as_object()
                .and_then(|value| value.get("calibration_passed"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Err(anyhow!(
                "model check `{}` omitted successful calibration evidence",
                check.id
            ));
        }
        if matches!(check.kind, super::contract::ModelCheckKind::Pairwise) {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow!("pairwise check `{}` is not structured", check.id))?;
            let orders = object
                .get("orders")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("pairwise check `{}` omitted position orders", check.id))?;
            let observed = orders.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if observed != ["candidate_first", "incumbent_first"]
                || object.get("consistent").and_then(Value::as_bool) != Some(true)
            {
                return Err(anyhow!(
                    "pairwise check `{}` did not control position order consistently",
                    check.id
                ));
            }
        }
    }
    Ok(artifact)
}

fn judgment_passed(value: &Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.as_object()?.get("passed")?.as_bool())
}

fn compare_metric(
    contract: &EvaluatorContract,
    incumbent: &EvaluationReport,
    candidate: &EvaluationReport,
) -> ImprovementDecision {
    let Some(id) = contract.comparison.check_id.as_deref() else {
        return ImprovementDecision::Inconclusive("comparison metric is missing".to_string());
    };
    let old = metric_value(incumbent, id);
    let new = metric_value(candidate, id);
    let (Some(old), Some(new)) = (old, new) else {
        return ImprovementDecision::Inconclusive(format!(
            "metric `{id}` did not produce comparable finite values"
        ));
    };
    let delta = match contract.comparison.direction {
        Some(MetricDirection::Higher) => new - old,
        Some(MetricDirection::Lower) => old - new,
        None => {
            return ImprovementDecision::Inconclusive("metric direction is missing".to_string());
        }
    };
    let threshold = contract
        .comparison
        .minimum_delta
        .max(contract.comparison.tolerance);
    if delta > contract.comparison.tolerance && delta >= threshold {
        ImprovementDecision::Better(format!("{id} improved by {}", concise_number(delta)))
    } else if delta.abs() <= contract.comparison.tolerance {
        ImprovementDecision::NotBetter(format!("{id} tied within tolerance"))
    } else {
        ImprovementDecision::NotBetter(format!(
            "{id} changed by {}, below the required improvement",
            concise_number(delta)
        ))
    }
}

async fn run_check(
    check: &MachineCheck,
    worktree_root: &Path,
    evidence_directory: &Path,
    command_environment: &HashMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Option<CheckReport>> {
    let mut values = Vec::new();
    let mut evidence = Vec::new();
    let mut passed = true;
    for repeat in 1..=check.baseline_repeats {
        let command_result =
            run_command(check, worktree_root, command_environment, cancellation).await?;
        let Some((status, stdout, stderr, exceeded)) = command_result else {
            return Ok(None);
        };
        let log_name = format!("{}-{repeat}.log", check.id);
        let log_path = evidence_directory.join(&log_name);
        let log = format!(
            "status: {}\nstdout-bytes: {}\nstderr-bytes: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
            status,
            stdout.len(),
            stderr.len(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );
        super::state::atomic_private_write(&log_path, log.as_bytes())?;
        evidence.push(log_name);
        let expected = check.expected_exit_codes.contains(&status);
        if exceeded {
            passed = false;
            values.push(CheckValue::Text(
                "timeout or output safety limit exceeded".to_string(),
            ));
            continue;
        }
        if !expected {
            passed = false;
            values.push(CheckValue::Pass(false));
            continue;
        }
        match extract(check, &stdout, true) {
            Ok(extracted) => {
                if matches!(extracted, CheckValue::Pass(false)) {
                    passed = false;
                }
                values.push(extracted);
            }
            Err(error) => {
                passed = false;
                values.push(CheckValue::Text(bound_decisive(&format!(
                    "result extraction failed: {error:#}"
                ))));
            }
        }
    }
    Ok(Some(CheckReport {
        passed,
        values,
        evidence,
    }))
}

async fn run_command(
    check: &MachineCheck,
    worktree_root: &Path,
    command_environment: &HashMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Option<(i32, Vec<u8>, Vec<u8>, bool)>> {
    let program = check
        .argv
        .first()
        .ok_or_else(|| anyhow!("evaluator check `{}` has no command", check.id))?;
    let cwd = worktree_root.join(&check.cwd);
    let mut environment = std::env::vars().collect::<HashMap<_, _>>();
    for (name, value) in &check.env {
        environment.insert(name.clone(), value.clone());
    }
    environment.extend(command_environment.clone());
    let SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        mut exit_rx,
    } = spawn_pipe_process_no_stdin(program, &check.argv[1..], &cwd, &environment, &None, &[])
        .await
        .with_context(|| format!("failed to start evaluator check `{}`", check.id))?;
    let stdout_task = tokio::spawn(read_bounded(stdout_rx));
    let stderr_task = tokio::spawn(read_bounded(stderr_rx));
    let timer = tokio::time::sleep(Duration::from_secs(check.timeout_seconds));
    tokio::pin!(timer);
    let termination = tokio::select! {
        status = &mut exit_rx => CommandTermination::Status(status.unwrap_or(-1)),
        _ = &mut timer => CommandTermination::TimedOut,
        _ = cancellation.cancelled() => CommandTermination::Cancelled,
    };
    let timed_out = matches!(termination, CommandTermination::TimedOut);
    let cancelled = matches!(termination, CommandTermination::Cancelled);
    // The root process can exit while descendants retain the output pipes or continue mutating
    // the worktree. The pinned upstream process runtime retains the process-group identity after
    // root exit, so this always terminates the entire owned group before evidence is consumed.
    session.request_terminate();
    let status = match termination {
        CommandTermination::Status(status) => status,
        CommandTermination::TimedOut | CommandTermination::Cancelled => {
            tokio::time::timeout(Duration::from_secs(2), &mut exit_rx)
                .await
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or(-1)
        }
    };
    let (stdout, stdout_exceeded) = stdout_task.await??;
    let (stderr, stderr_exceeded) = stderr_task.await??;
    if cancelled {
        return Ok(None);
    }
    Ok(Some((
        status,
        stdout,
        stderr,
        stdout_exceeded || stderr_exceeded || timed_out,
    )))
}

async fn read_bounded(mut receiver: mpsc::Receiver<Vec<u8>>) -> Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut exceeded = false;
    while let Some(buffer) = receiver.recv().await {
        let read = buffer.len();
        let remaining = MAX_CHECK_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    Ok((kept, exceeded))
}

fn extract(check: &MachineCheck, stdout: &[u8], passed: bool) -> Result<CheckValue> {
    match check.extract.kind {
        ExtractKind::Pass => Ok(CheckValue::Pass(passed)),
        ExtractKind::LastLine => {
            let text = String::from_utf8_lossy(stdout);
            let line = text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            Ok(CheckValue::Text(bound_decisive(line)))
        }
        ExtractKind::JsonNumber => {
            let value: Value = serde_json::from_slice(stdout)
                .with_context(|| format!("check `{}` did not emit JSON", check.id))?;
            let pointer = check
                .extract
                .json_pointer
                .as_deref()
                .ok_or_else(|| anyhow!("check `{}` has no JSON pointer", check.id))?;
            let number = value
                .pointer(pointer)
                .and_then(Value::as_f64)
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    anyhow!("check `{}` did not emit a finite numeric result", check.id)
                })?;
            Ok(CheckValue::Number(number))
        }
    }
}

fn decisive_result(
    contract: &EvaluatorContract,
    reports: &BTreeMap<String, CheckReport>,
    accepted: bool,
) -> String {
    if contract.comparison.kind == ComparisonKind::Metric
        && let Some(id) = contract.comparison.check_id.as_deref()
        && let Some(number) = metric_from_reports(reports, id)
    {
        return format!("{id}={}", concise_number(number));
    }
    if contract.comparison.kind == ComparisonKind::Metric
        && let Some(id) = contract.comparison.check_id.as_deref()
        && let Some(value) = reports.get(id).and_then(|report| report.values.last())
    {
        return match value {
            CheckValue::Number(number) => format!("{id}={}", concise_number(*number)),
            CheckValue::Text(text) => bound_decisive(text),
            CheckValue::Pass(value) => {
                format!("{id}={}", if *value { "pass" } else { "fail" })
            }
        };
    }
    if contract.comparison.kind == ComparisonKind::Pairwise
        && let Some(id) = contract.comparison.check_id.as_deref()
        && let Some(report) = reports.get(id)
    {
        return format!(
            "{id}={}",
            if report.passed {
                "candidate preferred"
            } else {
                "candidate not preferred"
            }
        );
    }
    let passed = contract
        .acceptance
        .required_check_ids
        .iter()
        .filter(|id| reports.get(*id).is_some_and(|report| report.passed))
        .count();
    format!(
        "{passed}/{} checks{}",
        contract.acceptance.required_check_ids.len(),
        if accepted { "" } else { " passing" }
    )
}

fn metric_from_reports(reports: &BTreeMap<String, CheckReport>, id: &str) -> Option<f64> {
    let values = reports
        .get(id)?
        .values
        .iter()
        .filter_map(|value| match value {
            CheckValue::Number(number) => Some(*number),
            _ => None,
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

pub(crate) fn metric_value(report: &EvaluationReport, id: &str) -> Option<f64> {
    metric_from_reports(&report.checks, id)
}

fn concise_number(value: f64) -> String {
    let rendered = format!("{value:.6}");
    rendered
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn bound_decisive(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() && *character != '│')
        .take(MAX_DECISIVE_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn create_private_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(anyhow!("unsafe evaluator evidence directory"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality_loop::EvaluatorContract;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "bettercodex-loop-evaluator-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("candidate.txt"), "10\n").unwrap();
            Self { root }
        }

        fn script(&self, name: &str, source: &str) -> PathBuf {
            let path = self.root.join(name);
            std::fs::write(&path, source).unwrap();
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn contract(root: &Path, program: &Path, repeats: u8) -> EvaluatorContract {
        let contract: EvaluatorContract = serde_json::from_value(json!({
            "version": 1,
            "loop_name": "Metric speed",
            "promises": [{
                "id": "metric",
                "class": "improvement",
                "statement": "the measured value increases",
                "failure_mode": "the value ties or decreases",
                "method": "local metric command",
                "required_evidence": ["numeric output"]
            }],
            "candidate_paths": ["candidate.txt"],
            "fixed_constraints": ["local only"],
            "integrity_paths": [],
            "scratch_paths": [],
            "machine_checks": [{
                "id": "metric-check",
                "promise_ids": ["metric"],
                "argv": [program],
                "cwd": ".",
                "env": {},
                "input_paths": [{"root": "worktree", "path": "candidate.txt"}],
                "fixture_paths": [],
                "timeout_seconds": 2,
                "resource_budget": "one process",
                "side_effects": "none",
                "approval": "none",
                "expected_exit_codes": [0],
                "extract": {"kind": "json_number", "json_pointer": "/value"},
                "baseline_repeats": repeats
            }],
            "discrimination_checks": [{
                "linked_check_id": "metric-check",
                "check": {
                    "id": "metric-known-failure",
                    "promise_ids": ["metric"],
                    "argv": [program],
                    "cwd": ".",
                    "env": {},
                    "input_paths": [{"root": "worktree", "path": "candidate.txt"}],
                    "fixture_paths": [{
                        "root": "evaluator",
                        "path": "evaluator/workspace/known-failure.txt"
                    }],
                    "timeout_seconds": 2,
                    "resource_budget": "one process",
                    "side_effects": "none",
                    "approval": "none",
                    "expected_exit_codes": [0],
                    "extract": {"kind": "pass"},
                    "baseline_repeats": 1
                }
            }],
            "model_checks": [],
            "acceptance": {"required_check_ids": ["metric-check"]},
            "comparison": {
                "kind": "metric",
                "check_id": "metric-check",
                "direction": "higher",
                "minimum_delta": 2.0,
                "tolerance": 0.5,
                "ties": "discard",
                "inconclusive": "discard"
            },
            "environment": ["fixture"],
            "uncovered": ["none"],
            "known_loopholes": ["none"]
        }))
        .unwrap();
        contract.validate(root).unwrap();
        contract
    }

    #[tokio::test]
    async fn harness_runs_repeats_extracts_metrics_and_writes_bounded_evidence() {
        let fixture = Fixture::new();
        let script = fixture.script(
            "metric.sh",
            "#!/bin/sh\nprintf '{\"value\": %s}\\n' \"$(cat candidate.txt)\"\n",
        );
        let contract = contract(&fixture.root, &script, 2);
        let evidence = fixture.root.join("evidence");
        let report = run_machine_evaluation(
            &contract,
            &fixture.root,
            "state-1",
            &evidence,
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(report.accepted);
        assert_eq!(report.decisive, "metric-check=10");
        assert_eq!(report.checks["metric-check"].values.len(), 2);
        assert!(evidence.join("metric-check-1.log").is_file());
        assert!(evidence.join("metric-check-2.log").is_file());
    }

    #[tokio::test]
    async fn harness_environment_overrides_contract_environment() {
        let fixture = Fixture::new();
        let script = fixture.script(
            "environment.sh",
            "#!/bin/sh\nif [ \"$CARGO_TARGET_DIR\" = \"$1\" ]; then value=10; else value=0; fi\nprintf '{\"value\": %s}\\n' \"$value\"\n",
        );
        let target = fixture.root.join("runtime/cargo-target");
        let mut contract = contract(&fixture.root, &script, 1);
        contract.machine_checks[0].argv =
            vec![script.display().to_string(), target.display().to_string()];
        contract.machine_checks[0].env.insert(
            "CARGO_TARGET_DIR".to_string(),
            "contract-controlled-target".to_string(),
        );
        let command_environment =
            HashMap::from([("CARGO_TARGET_DIR".to_string(), target.display().to_string())]);

        let report = run_machine_evaluation(
            &contract,
            &fixture.root,
            "state-1",
            &fixture.root.join("evidence"),
            &command_environment,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.decisive, "metric-check=10");
    }

    #[tokio::test]
    async fn harness_reproduces_frozen_known_failure_discrimination() {
        let fixture = Fixture::new();
        let known_failure = fixture.root.join("evaluator/workspace/known-failure.txt");
        std::fs::create_dir_all(known_failure.parent().unwrap()).unwrap();
        std::fs::write(&known_failure, "known failure\n").unwrap();
        let script = fixture.script(
            "metric.sh",
            "#!/bin/sh\nif [ \"$1\" = --probe ]; then\n  grep -qx 'known failure' \"$2\"\n  exit\nfi\nprintf '{\"value\": %s}\\n' \"$(cat candidate.txt)\"\n",
        );
        let mut contract = contract(&fixture.root, &script, 1);
        contract.discrimination_checks[0].check.argv = vec![
            script.display().to_string(),
            "--probe".to_string(),
            known_failure.display().to_string(),
        ];
        contract.validate(&fixture.root).unwrap();

        let reports = run_discrimination_checks(
            &contract,
            &fixture.root,
            &fixture.root.join("discrimination-pass"),
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(reports["metric-known-failure"].passed);

        std::fs::write(&known_failure, "healthy fixture\n").unwrap();
        let reports = run_discrimination_checks(
            &contract,
            &fixture.root,
            &fixture.root.join("discrimination-fail"),
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!reports["metric-known-failure"].passed);
    }

    #[tokio::test]
    async fn unexpected_exit_is_a_failed_check_instead_of_a_harness_crash() {
        let fixture = Fixture::new();
        let script = fixture.script("fail.sh", "#!/bin/sh\nexit 7\n");
        let contract = contract(&fixture.root, &script, 1);
        let report = run_machine_evaluation(
            &contract,
            &fixture.root,
            "state-1",
            &fixture.root.join("evidence"),
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!report.accepted);
        assert!(!report.checks["metric-check"].passed);
    }

    #[tokio::test]
    async fn timeout_terminates_the_phase_process_group_and_fails_closed() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("late-marker");
        let script = fixture.script(
            "timeout.sh",
            &format!(
                "#!/bin/sh\n(sleep 3; printf late > '{}') &\nwait\n",
                marker.display()
            ),
        );
        let mut contract = contract(&fixture.root, &script, 1);
        contract.machine_checks[0].timeout_seconds = 1;
        let report = run_machine_evaluation(
            &contract,
            &fixture.root,
            "state-1",
            &fixture.root.join("evidence"),
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!report.accepted);
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn successful_root_exit_still_terminates_background_descendants() {
        let fixture = Fixture::new();
        let child_record = fixture.root.join("child.pid");
        let script = fixture.script(
            "background.sh",
            &format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nprintf '{{\"value\": 10}}\\n'\n",
                child_record.display()
            ),
        );
        let contract = contract(&fixture.root, &script, 1);
        let evaluation = tokio::time::timeout(
            Duration::from_secs(2),
            run_machine_evaluation(
                &contract,
                &fixture.root,
                "state-1",
                &fixture.root.join("evidence"),
                &HashMap::new(),
                &CancellationToken::new(),
            ),
        )
        .await;
        let child = std::fs::read_to_string(&child_record)
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        if evaluation.is_err() {
            unsafe {
                libc::kill(child, libc::SIGKILL);
            }
        }
        let report = evaluation
            .expect("evaluator hung on a descendant retaining its output pipes")
            .unwrap()
            .unwrap();
        assert!(report.accepted);

        let exited = tokio::time::timeout(Duration::from_secs(2), async {
            while process_exists(child) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        if !exited {
            unsafe {
                libc::kill(child, libc::SIGKILL);
            }
        }
        assert!(exited, "background evaluator descendant survived cleanup");
    }

    fn process_exists(process_id: libc::pid_t) -> bool {
        if unsafe { libc::kill(process_id, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    #[test]
    fn structured_artifact_must_match_state_check_set_and_machine_truth() {
        let fixture = Fixture::new();
        let script = fixture.script("metric.sh", "#!/bin/sh\nprintf '{\"value\":10}'\n");
        let contract = contract(&fixture.root, &script, 1);
        let mut report = EvaluationReport {
            state: "candidate-state".to_string(),
            accepted: true,
            decisive: "metric-check=10".to_string(),
            checks: BTreeMap::from([(
                "metric-check".to_string(),
                CheckReport {
                    passed: true,
                    values: vec![CheckValue::Number(10.0)],
                    evidence: vec!["metric.log".to_string()],
                },
            )]),
        };
        let artifact = fixture.root.join("artifact.json");

        std::fs::write(
            &artifact,
            serde_json::to_vec(&json!({
                "state": "stale",
                "checks": {"metric-check": {"passed": true}}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(apply_structured_artifact(&contract, &mut report, &artifact).is_err());

        std::fs::write(
            &artifact,
            serde_json::to_vec(&json!({
                "state": "candidate-state",
                "checks": {"metric-check": {"passed": false}}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(apply_structured_artifact(&contract, &mut report, &artifact).is_err());

        std::fs::write(
            &artifact,
            serde_json::to_vec(&json!({
                "state": "candidate-state",
                "checks": {
                    "metric-check": {"passed": true},
                    "invented": {"passed": true}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(apply_structured_artifact(&contract, &mut report, &artifact).is_err());
    }

    #[test]
    fn metric_comparison_applies_acceptance_delta_tolerance_and_inconclusive_rules() {
        let fixture = Fixture::new();
        let script = fixture.script("metric.sh", "#!/bin/sh\nprintf '{\"value\":10}'\n");
        let contract = contract(&fixture.root, &script, 1);
        let report = |value: Option<f64>, accepted| EvaluationReport {
            state: "state".to_string(),
            accepted,
            decisive: "result".to_string(),
            checks: BTreeMap::from([(
                "metric-check".to_string(),
                CheckReport {
                    passed: accepted,
                    values: value.into_iter().map(CheckValue::Number).collect(),
                    evidence: Vec::new(),
                },
            )]),
        };
        assert!(matches!(
            compare_reports(
                &contract,
                &report(Some(10.0), true),
                &report(Some(12.0), true)
            ),
            ImprovementDecision::Better(_)
        ));
        assert!(matches!(
            compare_reports(
                &contract,
                &report(Some(10.0), true),
                &report(Some(10.4), true)
            ),
            ImprovementDecision::NotBetter(_)
        ));
        assert!(matches!(
            compare_reports(&contract, &report(Some(10.0), true), &report(None, true)),
            ImprovementDecision::Inconclusive(_)
        ));
        assert!(matches!(
            compare_reports(
                &contract,
                &report(Some(10.0), true),
                &report(Some(20.0), false)
            ),
            ImprovementDecision::NotBetter(_)
        ));
    }

    #[test]
    fn calibrated_pairwise_artifacts_require_both_position_orders() {
        let fixture = Fixture::new();
        let script = fixture.script("metric.sh", "#!/bin/sh\nprintf '{\"value\":10}'\n");
        let base = contract(&fixture.root, &script, 1);
        let mut value = serde_json::to_value(base).unwrap();
        value["model_checks"] = json!([{
            "id": "quality-judge",
            "promise_ids": ["metric"],
            "kind": "pairwise",
            "rubric_path": "evaluator/workspace/rubric.md",
            "required_artifacts": ["candidate and incumbent evidence"],
            "calibration_paths": [
                "evaluator/workspace/calibration-pass.json",
                "evaluator/workspace/calibration-fail.json"
            ],
            "output_shape": "two-order boolean pairwise result",
            "hard_gate": false
        }]);
        value["comparison"] = json!({
            "kind": "pairwise",
            "check_id": "quality-judge",
            "direction": null,
            "minimum_delta": 0.0,
            "tolerance": 0.0,
            "ties": "discard",
            "inconclusive": "discard"
        });
        let contract: EvaluatorContract = serde_json::from_value(value).unwrap();
        contract.validate(&fixture.root).unwrap();
        let mut candidate = EvaluationReport {
            state: "candidate".to_string(),
            accepted: true,
            decisive: "metric-check=10".to_string(),
            checks: BTreeMap::from([(
                "metric-check".to_string(),
                CheckReport {
                    passed: true,
                    values: vec![CheckValue::Number(10.0)],
                    evidence: Vec::new(),
                },
            )]),
        };
        let artifact = fixture.root.join("pairwise.json");
        let write = |orders: Value| {
            std::fs::write(
                &artifact,
                serde_json::to_vec(&json!({
                    "state": "candidate",
                    "checks": {
                        "metric-check": {"passed": true},
                        "quality-judge": {
                            "passed": true,
                            "artifacts": ["candidate.json and incumbent.json"],
                            "calibration_passed": true,
                            "orders": orders,
                            "consistent": true
                        }
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        };
        write(json!(["candidate_first"]));
        assert!(apply_structured_artifact(&contract, &mut candidate, &artifact).is_err());
        write(json!(["candidate_first", "incumbent_first"]));
        apply_structured_artifact(&contract, &mut candidate, &artifact).unwrap();
        assert!(matches!(
            compare_reports(&contract, &candidate.clone(), &candidate),
            ImprovementDecision::Better(_)
        ));
    }

    #[tokio::test]
    async fn malformed_numeric_output_is_an_inconclusive_failed_check() {
        let fixture = Fixture::new();
        let script = fixture.script("invalid-json.sh", "#!/bin/sh\nprintf not-json\n");
        let contract = contract(&fixture.root, &script, 1);
        let report = run_machine_evaluation(
            &contract,
            &fixture.root,
            "state-1",
            &fixture.root.join("evidence"),
            &HashMap::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!report.accepted);
        assert!(metric_value(&report, "metric-check").is_none());
        assert!(matches!(
            compare_reports(&contract, &report, &report),
            ImprovementDecision::NotBetter(_)
        ));
    }
}
