use super::contract::ComparisonKind;
use super::contract::ExtractKind;
use super::contract::MachineCheck;
use super::contract::MetricDirection;
use crate::quality_loop::EvaluatorContract;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
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
    Status(std::process::ExitStatus),
    TimedOut,
    Cancelled,
}

pub(crate) async fn run_machine_evaluation(
    contract: &EvaluatorContract,
    worktree_root: &Path,
    state: &str,
    evidence_directory: &Path,
    cancellation: &CancellationToken,
) -> Result<Option<EvaluationReport>> {
    create_private_directory(evidence_directory)?;
    let mut reports = BTreeMap::new();
    for check in &contract.machine_checks {
        let Some(report) =
            run_check(check, worktree_root, evidence_directory, cancellation).await?
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
    cancellation: &CancellationToken,
) -> Result<Option<BTreeMap<String, CheckReport>>> {
    create_private_directory(evidence_directory)?;
    let mut reports = BTreeMap::new();
    for discrimination in &contract.discrimination_checks {
        let check = &discrimination.check;
        let Some(report) =
            run_check(check, worktree_root, evidence_directory, cancellation).await?
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

pub(crate) fn apply_structured_artifact(
    contract: &EvaluatorContract,
    report: &mut EvaluationReport,
    artifact_path: &Path,
) -> Result<()> {
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
    if state != report.state {
        return Err(anyhow!(
            "phase evidence is bound to stale state `{state}` instead of `{}`",
            report.state
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
        let claimed = judgment_passed(value)
            .ok_or_else(|| anyhow!("machine check `{}` omitted a boolean verdict", check.id))?;
        if report
            .checks
            .get(&check.id)
            .is_some_and(|observed| observed.passed != claimed)
        {
            return Err(anyhow!(
                "phase evidence contradicts harness result for `{}`",
                check.id
            ));
        }
    }
    for check in &contract.model_checks {
        let value = checks
            .get(&check.id)
            .ok_or_else(|| anyhow!("phase evidence omitted model check `{}`", check.id))?;
        let passed = judgment_passed(value)
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
    cancellation: &CancellationToken,
) -> Result<Option<CheckReport>> {
    let mut values = Vec::new();
    let mut evidence = Vec::new();
    let mut passed = true;
    for repeat in 1..=check.baseline_repeats {
        let command_result = run_command(check, worktree_root, cancellation).await?;
        let Some((status, stdout, stderr, exceeded)) = command_result else {
            return Ok(None);
        };
        let log_name = format!("{}-{repeat}.log", check.id);
        let log_path = evidence_directory.join(&log_name);
        let log = format!(
            "status: {}\nstdout-bytes: {}\nstderr-bytes: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
            stdout.len(),
            stderr.len(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );
        super::state::atomic_private_write(&log_path, log.as_bytes())?;
        evidence.push(log_name);
        let expected = status
            .code()
            .is_some_and(|code| check.expected_exit_codes.contains(&code));
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
    cancellation: &CancellationToken,
) -> Result<Option<(std::process::ExitStatus, Vec<u8>, Vec<u8>, bool)>> {
    let program = check.argv.first().expect("validated non-empty argv");
    let cwd = worktree_root.join(&check.cwd);
    let mut command = Command::new(program);
    command
        .args(&check.argv[1..])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &check.env {
        command.env(name, value);
    }
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start evaluator check `{}`", check.id))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow!("evaluator process has no ID"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("check stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("check stderr missing"))?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let timer = tokio::time::sleep(Duration::from_secs(check.timeout_seconds));
    tokio::pin!(timer);
    let termination = tokio::select! {
        status = child.wait() => CommandTermination::Status(status?),
        _ = &mut timer => CommandTermination::TimedOut,
        _ = cancellation.cancelled() => CommandTermination::Cancelled,
    };
    let timed_out = matches!(termination, CommandTermination::TimedOut);
    let cancelled = matches!(termination, CommandTermination::Cancelled);
    let status = match termination {
        CommandTermination::Status(status) => status,
        CommandTermination::TimedOut | CommandTermination::Cancelled => {
            terminate_process_group(pid).await;
            child.wait().await?
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

async fn read_bounded(mut reader: impl AsyncRead + Unpin) -> Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CHECK_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    Ok((kept, exceeded))
}

async fn terminate_process_group(pid: u32) {
    let group = -(pid as i32);
    unsafe {
        libc::kill(group, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    unsafe {
        libc::kill(group, libc::SIGKILL);
    }
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
                .expect("validated JSON pointer");
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
