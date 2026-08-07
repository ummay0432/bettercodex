use super::EvaluationReport;
use super::ImprovementDecision;
use super::LoopInvocation;
use super::LoopProgress;
use super::LoopRun;
use super::PackageManifest;
use super::PathSpec;
use super::RepositorySnapshot;
use super::RunPhase;
use super::SetupVerdict;
use super::WorkerEnvelope;
use super::WorkerVerdict;
use super::apply_structured_artifact;
use super::compare_reports;
use super::contract::ArtifactRoot;
use super::contract::ComparisonKind;
use super::contract::SideEffects;
use super::parse_setup_envelope;
use super::parse_worker_envelope;
use super::run_discrimination_checks;
use super::run_machine_evaluation;
use super::state::LedgerRow;
use crate::agent::Agent;
use crate::agent::SubmitOutcome;
use crate::agent::TurnControl;
use crate::context::FrozenLoopContext;
use crate::events::AgentEvent;
use crate::input::UserInput;
use crate::rollout::TurnOutcome;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeSet;
use std::future::Future;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const EVALUATOR_PROMPT: &str = include_str!("../../prompts/loop-evaluator.md");
const WORKER_PROMPT: &str = include_str!("../../prompts/loop-worker.md");
const CONTRACT_PROMPT: &str = include_str!("../../prompts/loop-contract.md");

#[derive(Default)]
struct OutcomeCounts {
    kept: usize,
    discarded: usize,
    crashed: usize,
    blocked: usize,
    interrupted: usize,
}

struct LoopSummary {
    setup: String,
    baseline: Option<EvaluationReport>,
    final_report: Option<EvaluationReport>,
    counts: OutcomeCounts,
    final_state: String,
    blind_spots: Vec<String>,
    unvalidated: Vec<String>,
    blocked: Option<String>,
    interrupted: bool,
}

struct PhaseOutput {
    response: String,
    session_id: String,
    usage: Option<TokenUsage>,
    duration_ms: u64,
}

trait PhaseRunner {
    fn run<'a>(
        &'a mut self,
        cwd: &'a Path,
        run: &'a LoopRun,
        frozen: &'a FrozenLoopContext,
        name: &'a str,
        prompt: &'a str,
        parent_control: &'a TurnControl,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PhaseOutput>>> + Send + 'a>>;
}

struct LivePhaseRunner;

impl PhaseRunner for LivePhaseRunner {
    fn run<'a>(
        &'a mut self,
        cwd: &'a Path,
        run: &'a LoopRun,
        frozen: &'a FrozenLoopContext,
        name: &'a str,
        prompt: &'a str,
        parent_control: &'a TurnControl,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PhaseOutput>>> + Send + 'a>> {
        Box::pin(run_live_phase(
            cwd,
            run,
            frozen,
            name,
            prompt,
            parent_control,
        ))
    }
}

#[derive(Serialize)]
struct IterationRecord<'a> {
    iteration: usize,
    session_id: &'a str,
    usage: Option<&'a TokenUsage>,
    session_duration_ms: Option<u64>,
    incumbent_state: &'a str,
    candidate_state: &'a str,
    model_verdict: &'a str,
    resolved_status: &'a str,
    description: &'a str,
    result: &'a str,
    evidence: &'a str,
    created: &'a [String],
    modified: &'a [String],
    deleted: &'a [String],
    unvalidated: &'a str,
}

pub(crate) async fn submit_with_control(
    agent: &mut Agent,
    input: UserInput,
    invocation: LoopInvocation,
    events: UnboundedSender<AgentEvent>,
    control: TurnControl,
) -> Result<SubmitOutcome> {
    let worktree = super::Worktree::discover(agent.cwd())?;
    let event_target = Some(events.clone());
    let frozen = agent.record_loop_invocation(input, &event_target)?;
    let turn_id = agent.start_loop_turn()?;
    let execution = execute(&worktree, frozen, invocation, &events, &control).await;
    let _ = events.send(AgentEvent::LoopProgressCleared);
    match execution {
        Ok((answer, interrupted)) => {
            agent.finish_loop_turn(
                &turn_id,
                Some(&answer),
                if interrupted {
                    TurnOutcome::Interrupted
                } else {
                    TurnOutcome::Completed
                },
            )?;
            Ok(SubmitOutcome::Completed(answer))
        }
        Err(error) => {
            agent.finish_loop_turn(&turn_id, None, TurnOutcome::Failed)?;
            Err(error)
        }
    }
}

async fn execute(
    worktree: &super::Worktree,
    frozen: FrozenLoopContext,
    invocation: LoopInvocation,
    events: &UnboundedSender<AgentEvent>,
    control: &TurnControl,
) -> Result<(String, bool)> {
    let mut run = LoopRun::create(
        worktree,
        &invocation,
        frozen.operator_inputs(),
        frozen.context_items(),
    )?;
    run.verify_runtime_identity()?;
    let original = load_snapshot(&run, &run.state.starting_snapshot)?;
    run.write_json("starting-state.json", &original.state)?;
    progress(events, &run, "eval", "building evaluator", None, None);

    let mut runner = LivePhaseRunner;
    let result = execute_phases(
        worktree,
        &frozen,
        &mut run,
        &original,
        events,
        control,
        &mut runner,
    )
    .await;
    let summary = match result {
        Ok(summary) => summary,
        Err(error) => {
            let incumbent = load_snapshot(&run, &run.state.incumbent_snapshot)
                .unwrap_or_else(|_| original.clone());
            let restoration = worktree.restore(run.root(), &incumbent);
            let blocker = match restoration {
                Ok(()) => format!("{error:#}"),
                Err(restore_error) => {
                    format!("{error:#}; incumbent restoration also failed: {restore_error:#}")
                }
            };
            run.update(|state| {
                state.phase = RunPhase::Blocked;
                state.blocker = Some(blocker.clone());
                state.final_result = None;
            })?;
            progress(events, &run, "eval", "blocked", Some(&blocker), None);
            LoopSummary {
                setup: "blocked".to_string(),
                baseline: None,
                final_report: None,
                counts: OutcomeCounts {
                    blocked: 1,
                    ..OutcomeCounts::default()
                },
                final_state: incumbent.state.digest,
                blind_spots: Vec::new(),
                unvalidated: Vec::new(),
                blocked: Some(blocker),
                interrupted: false,
            }
        }
    };
    let answer = format_final(&run, &summary);
    Ok((answer, summary.interrupted))
}

async fn execute_phases<R: PhaseRunner>(
    worktree: &super::Worktree,
    frozen: &FrozenLoopContext,
    run: &mut LoopRun,
    original: &RepositorySnapshot,
    events: &UnboundedSender<AgentEvent>,
    control: &TurnControl,
    runner: &mut R,
) -> Result<LoopSummary> {
    let evaluator_workspace = run.root().join("evaluator/workspace");
    let phase_prompt = evaluator_prompt(run, worktree, &evaluator_workspace)?;
    let control_before = immutable_control_digest(run.root(), None)?;
    let setup = runner
        .run(
            worktree.root(),
            run,
            frozen,
            "setup",
            &phase_prompt,
            control,
        )
        .await?;
    if setup.is_none() {
        worktree.restore(run.root(), original)?;
        run.update(|state| state.phase = RunPhase::Interrupted)?;
        return Ok(interrupted_setup_summary(original));
    }
    let setup = setup.expect("checked phase output");
    verify_control_digest(run.root(), None, &control_before)?;
    run.write_json(
        "evaluator/session.json",
        &serde_json::json!({
            "session_id": &setup.session_id,
            "usage": &setup.usage,
            "duration_ms": setup.duration_ms,
        }),
    )?;
    let envelope = parse_setup_envelope(&setup.response);
    worktree.restore(run.root(), original)?;
    PackageManifest::make_private(&evaluator_workspace)?;
    let envelope = envelope?;
    if envelope.verdict == SetupVerdict::Blocked {
        run.update(|state| {
            state.phase = RunPhase::Blocked;
            state.blocker = Some(envelope.blocker.clone());
        })?;
        progress(
            events,
            run,
            "eval",
            "blocked",
            Some(&envelope.blocker),
            None,
        );
        return Ok(LoopSummary {
            setup: "blocked".to_string(),
            baseline: None,
            final_report: None,
            counts: OutcomeCounts {
                blocked: 1,
                ..OutcomeCounts::default()
            },
            final_state: original.state.digest.clone(),
            blind_spots: Vec::new(),
            unvalidated: Vec::new(),
            blocked: Some(envelope.blocker),
            interrupted: false,
        });
    }

    let contract_path = run.resolve_existing_file(&envelope.contract, &evaluator_workspace)?;
    let declared_baseline_path =
        run.resolve_existing_file(&envelope.baseline, &evaluator_workspace)?;
    let contract = super::EvaluatorContract::load(&contract_path, worktree.root())?;
    validate_contract_artifacts(run, &contract, &evaluator_workspace)?;
    let package = PackageManifest::capture(&evaluator_workspace)?;
    package.save(&run.root().join("evaluator-integrity.json"))?;
    run.update(|state| {
        state.phase = RunPhase::Baselining;
        state.display_name = contract.loop_name.clone();
    })?;
    progress(events, run, "eval", "baselining", None, None);

    let snapshot_paths = contract.snapshot_paths();
    let baseline_snapshot = worktree.capture(run.root(), &snapshot_paths)?;
    let initial_delta = worktree.delta(original, &baseline_snapshot);
    if !initial_delta.is_empty() {
        return Err(anyhow!(
            "restored setup state does not match the captured starting state"
        ));
    }
    let baseline_snapshot_path = worktree.save_snapshot(run.root(), &baseline_snapshot)?;
    let baseline_snapshot_relative = run.relative(&baseline_snapshot_path)?;
    run.update(|state| {
        state.starting_snapshot = baseline_snapshot_relative.clone();
        state.incumbent_snapshot = baseline_snapshot_relative.clone();
    })?;
    run.write_json("starting-state.json", &baseline_snapshot.state)?;
    let baseline_directory = run.create_directory("baseline")?;
    let cancellation = control.cancellation();
    let discrimination_directory = run.create_directory("baseline/discrimination")?;
    let Some(discrimination) = run_discrimination_checks(
        &contract,
        worktree.root(),
        &discrimination_directory,
        &cancellation,
    )
    .await?
    else {
        worktree.restore(run.root(), &baseline_snapshot)?;
        run.update(|state| state.phase = RunPhase::Interrupted)?;
        return Ok(interrupted_setup_summary(&baseline_snapshot));
    };
    let discrimination_after = worktree.capture(run.root(), &snapshot_paths)?;
    let discrimination_side_effects = verify_evaluator_side_effects(
        worktree,
        &contract,
        &baseline_snapshot,
        &discrimination_after,
    );
    worktree.restore(run.root(), &baseline_snapshot)?;
    discrimination_side_effects
        .context("discrimination probe crossed its declared scratch boundary")?;
    let failed_probes = discrimination
        .iter()
        .filter(|(_, report)| !report.passed)
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    if !failed_probes.is_empty() {
        return Err(anyhow!(
            "evaluator discrimination probes did not catch their frozen failures: {}",
            failed_probes.join(", ")
        ));
    }
    let Some(mut baseline) = run_machine_evaluation(
        &contract,
        worktree.root(),
        &baseline_snapshot.state.digest,
        &baseline_directory,
        &cancellation,
    )
    .await?
    else {
        worktree.restore(run.root(), &baseline_snapshot)?;
        run.update(|state| state.phase = RunPhase::Interrupted)?;
        return Ok(interrupted_setup_summary(&baseline_snapshot));
    };
    let baseline_after = worktree.capture(run.root(), &snapshot_paths)?;
    let side_effects =
        verify_evaluator_side_effects(worktree, &contract, &baseline_snapshot, &baseline_after);
    worktree.restore(run.root(), &baseline_snapshot)?;
    side_effects.context("baseline evaluator crossed its declared scratch boundary")?;
    apply_structured_artifact(&contract, &mut baseline, &declared_baseline_path)?;
    ensure_comparable_baseline(&contract, &baseline)?;
    run.write_json("baseline/evaluation.json", &baseline)?;
    package.verify(&evaluator_workspace)?;
    let baseline_evidence = "baseline/evaluation.json";
    run.append_ledger(&LedgerRow {
        iteration: 0,
        state: &baseline_snapshot.state.digest,
        result: &baseline.decisive,
        status: "baseline",
        description: "starting state",
        evidence: baseline_evidence,
    })?;
    run.update(|state| {
        state.baseline_result = Some(baseline.decisive.clone());
        state.final_result = Some(baseline.decisive.clone());
    })?;
    progress(
        events,
        run,
        "eval",
        "baseline complete",
        Some(&baseline.decisive),
        None,
    );

    let mut incumbent_snapshot = baseline_snapshot.clone();
    let mut incumbent_report = baseline.clone();
    let mut counts = OutcomeCounts::default();
    let mut unvalidated = Vec::new();
    let mut blocker = None;
    let total = run.state.requested_iterations;

    for iteration in 1..=total {
        let phase = format!("{iteration}/{total}");
        let evidence_relative = format!("iterations/{iteration}");
        let evidence_directory = run.create_directory(&evidence_relative)?;
        run.write_json(
            &format!("{evidence_relative}/incumbent-state.json"),
            &incumbent_snapshot.state,
        )?;
        run.update(|state| {
            state.phase = RunPhase::Iteration;
            state.active_iteration = Some(iteration);
            state.active_candidate_snapshot = None;
            state.prepared_iteration = None;
        })?;
        let incumbent_diff = worktree
            .text_diff_counts(run.root(), &baseline_snapshot, &incumbent_snapshot)
            .ok();
        progress(events, run, &phase, "exploring", None, incumbent_diff);
        let worker_prompt =
            worker_prompt(run, iteration, total, &evidence_directory, &contract_path)?;
        let control_before = immutable_control_digest(run.root(), Some(iteration))?;
        let worker = runner
            .run(
                worktree.root(),
                run,
                frozen,
                &format!("worker-{iteration}"),
                &worker_prompt,
                control,
            )
            .await;

        if control.is_cancelled() {
            restore_iteration_snapshot(worktree, run, &incumbent_snapshot)?;
            counts.interrupted += 1;
            let description = "operator interrupted the active iteration";
            let resolved = ResolvedIteration {
                incumbent_state: incumbent_snapshot.state.digest.clone(),
                status: "interrupted".to_string(),
                pulse: "interrupted",
                result: "interrupted".to_string(),
                description: description.to_string(),
                unvalidated: "none".to_string(),
                snapshot: incumbent_snapshot.clone(),
                report: None,
            };
            let phase_output = worker.as_ref().ok().and_then(|output| output.as_ref());
            publish_iteration(
                run,
                iteration,
                &evidence_relative,
                &resolved,
                phase_output,
                None,
            )?;
            run.update(|state| {
                state.phase = RunPhase::Interrupted;
                state.active_iteration = None;
                state.active_candidate_snapshot = None;
            })?;
            return Ok(LoopSummary {
                setup: "ready".to_string(),
                baseline: Some(baseline),
                final_report: Some(incumbent_report),
                counts,
                final_state: incumbent_snapshot.state.digest,
                blind_spots: blind_spots(&contract),
                unvalidated,
                blocked: None,
                interrupted: true,
            });
        }

        let resolved = resolve_iteration(
            worktree,
            run,
            &contract,
            &package,
            &incumbent_snapshot,
            &incumbent_report,
            worker,
            &control_before,
            iteration,
            total,
            &evidence_directory,
            &evidence_relative,
            events,
            &baseline_snapshot,
            &cancellation,
        )
        .await?;
        run.update(|state| {
            state.active_iteration = None;
            state.active_candidate_snapshot = None;
        })?;
        let resolved_diff = worktree
            .text_diff_counts(run.root(), &baseline_snapshot, &resolved.snapshot)
            .ok();
        match resolved.status.as_str() {
            "keep" => {
                counts.kept += 1;
                incumbent_snapshot = resolved.snapshot;
                incumbent_report = resolved.report.unwrap_or_else(|| incumbent_report.clone());
            }
            "discard" => counts.discarded += 1,
            "crash" => counts.crashed += 1,
            "blocked" => {
                counts.blocked += 1;
                blocker = Some(resolved.description.clone());
            }
            "interrupted" => {
                counts.interrupted += 1;
                if !resolved.unvalidated.eq_ignore_ascii_case("none") {
                    unvalidated.push(resolved.unvalidated.clone());
                }
                run.update(|state| {
                    state.phase = RunPhase::Interrupted;
                    state.active_iteration = None;
                    state.active_candidate_snapshot = None;
                })?;
                progress(
                    events,
                    run,
                    &phase,
                    resolved.pulse,
                    Some(&resolved.result),
                    resolved_diff,
                );
                return Ok(LoopSummary {
                    setup: "ready".to_string(),
                    baseline: Some(baseline),
                    final_report: Some(incumbent_report),
                    counts,
                    final_state: incumbent_snapshot.state.digest,
                    blind_spots: blind_spots(&contract),
                    unvalidated,
                    blocked: None,
                    interrupted: true,
                });
            }
            _ => unreachable!("iteration resolver returned a known status"),
        }
        if !resolved.unvalidated.eq_ignore_ascii_case("none") {
            unvalidated.push(resolved.unvalidated.clone());
        }
        progress(
            events,
            run,
            &phase,
            resolved.pulse,
            Some(&resolved.result),
            resolved_diff,
        );
        if resolved.status == "blocked" {
            run.update(|state| {
                state.phase = RunPhase::Blocked;
                state.active_iteration = None;
                state.blocker = blocker.clone();
            })?;
            break;
        }
    }

    if blocker.is_none() {
        run.update(|state| {
            state.phase = RunPhase::Completed;
            state.active_iteration = None;
            state.final_result = Some(incumbent_report.decisive.clone());
        })?;
    }
    Ok(LoopSummary {
        setup: "ready".to_string(),
        baseline: Some(baseline),
        final_report: Some(incumbent_report),
        counts,
        final_state: incumbent_snapshot.state.digest,
        blind_spots: blind_spots(&contract),
        unvalidated,
        blocked: blocker,
        interrupted: false,
    })
}

struct ResolvedIteration {
    incumbent_state: String,
    status: String,
    pulse: &'static str,
    result: String,
    description: String,
    unvalidated: String,
    snapshot: RepositorySnapshot,
    report: Option<EvaluationReport>,
}

#[allow(clippy::too_many_arguments)]
async fn resolve_iteration(
    worktree: &super::Worktree,
    run: &mut LoopRun,
    contract: &super::EvaluatorContract,
    package: &PackageManifest,
    incumbent: &RepositorySnapshot,
    incumbent_report: &EvaluationReport,
    worker: Result<Option<PhaseOutput>>,
    control_before: &str,
    iteration: usize,
    total: usize,
    evidence_directory: &Path,
    evidence_relative: &str,
    events: &UnboundedSender<AgentEvent>,
    starting: &RepositorySnapshot,
    cancellation: &CancellationToken,
) -> Result<ResolvedIteration> {
    let crash = |description: String| ResolvedIteration {
        incumbent_state: incumbent.state.digest.clone(),
        status: "crash".to_string(),
        pulse: "crashed",
        result: "session crash".to_string(),
        description,
        unvalidated: "none".to_string(),
        snapshot: incumbent.clone(),
        report: None,
    };
    let worker = match worker {
        Ok(Some(worker)) => worker,
        Ok(None) => {
            restore_iteration_snapshot(worktree, run, incumbent)?;
            let resolved = crash("worker ended without a terminal result".to_string());
            publish_iteration(run, iteration, evidence_relative, &resolved, None, None)?;
            return Ok(resolved);
        }
        Err(error) => {
            restore_iteration_snapshot(worktree, run, incumbent)?;
            let resolved = crash(format!("worker session failed: {error:#}"));
            publish_iteration(run, iteration, evidence_relative, &resolved, None, None)?;
            return Ok(resolved);
        }
    };
    PackageManifest::make_private(evidence_directory)?;
    if let Err(error) = verify_control_digest(run.root(), Some(iteration), control_before) {
        return block_integrity_raw(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            Some(&worker),
            "none",
            format!("worker modified harness-owned loop state: {error:#}"),
        );
    }
    if let Err(error) = package.verify(&run.root().join("evaluator/workspace")) {
        return block_integrity_raw(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            Some(&worker),
            "none",
            format!("worker modified the frozen evaluator: {error:#}"),
        );
    }
    let envelope = match parse_worker_envelope(&worker.response) {
        Ok(envelope) => envelope,
        Err(error) => {
            restore_iteration_snapshot(worktree, run, incumbent)?;
            let resolved = crash(format!("malformed worker result: {error:#}"));
            publish_iteration(
                run,
                iteration,
                evidence_relative,
                &resolved,
                Some(&worker),
                None,
            )?;
            return Ok(resolved);
        }
    };
    let evidence_path = match run.resolve_existing_file(&envelope.evidence, evidence_directory) {
        Ok(path) => path,
        Err(error) => {
            restore_iteration_snapshot(worktree, run, incumbent)?;
            let resolved = crash(format!("invalid worker evidence: {error:#}"));
            publish_iteration(
                run,
                iteration,
                evidence_relative,
                &resolved,
                Some(&worker),
                None,
            )?;
            return Ok(resolved);
        }
    };

    let raw = match worktree.capture(run.root(), &contract.snapshot_paths()) {
        Ok(raw) => raw,
        Err(error) => {
            return block_integrity_raw(
                worktree,
                run,
                incumbent,
                iteration,
                evidence_relative,
                Some(&worker),
                &envelope.unvalidated,
                format!("candidate state could not be captured safely: {error:#}"),
            );
        }
    };
    let raw_delta = worktree.delta(incumbent, &raw);
    let mut cleanup = contract.scratch_paths.clone();
    let mut boundary_violation = Vec::new();
    for path in raw_delta.paths() {
        if contract
            .integrity_paths
            .iter()
            .any(|spec| spec.covers(Path::new(path)))
        {
            return block_integrity(
                worktree,
                run,
                incumbent,
                iteration,
                evidence_relative,
                &worker,
                &envelope,
                format!("worker modified protected evaluator path `{path}`"),
            );
        }
        if contract.path_is_candidate(Path::new(path))
            || contract
                .scratch_paths
                .iter()
                .any(|spec| spec.covers(Path::new(path)))
        {
            continue;
        }
        if worktree.is_ignored(path)? {
            let cleanup_path = match PathSpec::try_from(path.to_string()) {
                Ok(path) => path,
                Err(error) => {
                    return block_integrity(
                        worktree,
                        run,
                        incumbent,
                        iteration,
                        evidence_relative,
                        &worker,
                        &envelope,
                        format!("ignored side effect could not be restored safely: {error}"),
                    );
                }
            };
            cleanup.push(cleanup_path);
        } else {
            boundary_violation.push(path.to_string());
        }
    }
    if !boundary_violation.is_empty() {
        return block_integrity(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            &worker,
            &envelope,
            format!(
                "worker changed paths outside the candidate boundary: {}",
                boundary_violation.join(", ")
            ),
        );
    }
    if let Err(error) = worktree.restore_paths(run.root(), incumbent, &cleanup) {
        return block_integrity(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            &worker,
            &envelope,
            format!("evaluator scratch restoration failed: {error:#}"),
        );
    }
    let candidate = worktree.capture(run.root(), &contract.snapshot_paths())?;
    let delta = match worktree.verify_candidate_boundary(
        incumbent,
        &candidate,
        &contract.candidate_paths,
    ) {
        Ok(delta) => delta,
        Err(error) => {
            return block_integrity(
                worktree,
                run,
                incumbent,
                iteration,
                evidence_relative,
                &worker,
                &envelope,
                format!("candidate crossed a protected repository boundary: {error:#}"),
            );
        }
    };
    let candidate_snapshot_path = worktree.save_snapshot(run.root(), &candidate)?;
    let candidate_snapshot_relative = run.relative(&candidate_snapshot_path)?;
    run.update(|state| {
        state.active_candidate_snapshot = Some(candidate_snapshot_relative.clone());
    })?;
    progress(
        events,
        run,
        &format!("{iteration}/{total}"),
        "validating",
        None,
        worktree
            .text_diff_counts(run.root(), starting, &candidate)
            .ok(),
    );
    if let Err(error) = validate_evidence_state(&evidence_path, &candidate.state.digest) {
        restore_iteration_snapshot(worktree, run, incumbent)?;
        let resolved = crash(format!("invalid worker evidence: {error:#}"));
        publish_iteration(
            run,
            iteration,
            evidence_relative,
            &resolved,
            Some(&worker),
            Some((&candidate, &delta, &envelope)),
        )?;
        return Ok(resolved);
    }

    if envelope.verdict == WorkerVerdict::Blocked {
        let accepted = supported_blocker(&evidence_path).unwrap_or(false);
        restore_iteration_snapshot(worktree, run, incumbent)?;
        let resolved = if accepted {
            ResolvedIteration {
                incumbent_state: incumbent.state.digest.clone(),
                status: "blocked".to_string(),
                pulse: "blocked",
                result: "blocked".to_string(),
                description: envelope.description.clone(),
                unvalidated: envelope.unvalidated.clone(),
                snapshot: incumbent.clone(),
                report: None,
            }
        } else {
            crash(format!(
                "unsupported BLOCKED verdict: {}",
                envelope.description
            ))
        };
        publish_iteration(
            run,
            iteration,
            evidence_relative,
            &resolved,
            Some(&worker),
            Some((&candidate, &delta, &envelope)),
        )?;
        return Ok(resolved);
    }

    if envelope.verdict == WorkerVerdict::Discard {
        restore_iteration_snapshot(worktree, run, incumbent)?;
        let resolved = ResolvedIteration {
            incumbent_state: incumbent.state.digest.clone(),
            status: "discard".to_string(),
            pulse: "restored",
            result: "worker discarded candidate".to_string(),
            description: envelope.description.clone(),
            unvalidated: envelope.unvalidated.clone(),
            snapshot: incumbent.clone(),
            report: None,
        };
        publish_iteration(
            run,
            iteration,
            evidence_relative,
            &resolved,
            Some(&worker),
            Some((&candidate, &delta, &envelope)),
        )?;
        return Ok(resolved);
    }

    let validation_directory = run.create_directory(&format!("{evidence_relative}/harness"))?;
    let evaluation = run_machine_evaluation(
        contract,
        worktree.root(),
        &candidate.state.digest,
        &validation_directory,
        cancellation,
    )
    .await;
    let observed = worktree.capture(run.root(), &contract.snapshot_paths());
    let side_effects = observed
        .as_ref()
        .map_err(|error| anyhow!("{error:#}"))
        .and_then(|after| verify_evaluator_side_effects(worktree, contract, &candidate, after));
    restore_iteration_snapshot(worktree, run, &candidate)?;
    let exact = worktree.capture(run.root(), &contract.snapshot_paths())?;
    if exact.state != candidate.state {
        return block_integrity_raw(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            Some(&worker),
            &envelope.unvalidated,
            "candidate state changed while evaluator side effects were restored".to_string(),
        );
    }
    if let Err(error) = side_effects {
        return block_integrity_raw(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            Some(&worker),
            &envelope.unvalidated,
            format!("frozen evaluator crossed its declared scratch boundary: {error:#}"),
        );
    }
    if let Err(error) = package.verify(&run.root().join("evaluator/workspace")) {
        return block_integrity_raw(
            worktree,
            run,
            incumbent,
            iteration,
            evidence_relative,
            Some(&worker),
            &envelope.unvalidated,
            format!("frozen evaluator modified its own package: {error:#}"),
        );
    }
    let mut candidate_report = match evaluation {
        Ok(Some(report)) => report,
        Ok(None) => {
            restore_iteration_snapshot(worktree, run, incumbent)?;
            let resolved = ResolvedIteration {
                incumbent_state: incumbent.state.digest.clone(),
                status: "interrupted".to_string(),
                pulse: "interrupted",
                result: "operator interrupted candidate validation".to_string(),
                description: "operator interrupted candidate validation".to_string(),
                unvalidated: envelope.unvalidated.clone(),
                snapshot: incumbent.clone(),
                report: None,
            };
            publish_iteration(
                run,
                iteration,
                evidence_relative,
                &resolved,
                Some(&worker),
                Some((&candidate, &delta, &envelope)),
            )?;
            return Ok(resolved);
        }
        Err(error) => {
            return block_integrity_raw(
                worktree,
                run,
                incumbent,
                iteration,
                evidence_relative,
                Some(&worker),
                &envelope.unvalidated,
                format!("frozen evaluator could not run: {error:#}"),
            );
        }
    };
    let evaluation_result =
        apply_structured_artifact(contract, &mut candidate_report, &evidence_path)
            .map(|()| compare_reports(contract, incumbent_report, &candidate_report));
    let decision = match evaluation_result {
        Ok(decision) => decision,
        Err(error) => ImprovementDecision::Inconclusive(format!("validation failed: {error:#}")),
    };
    let keep = !delta.is_empty() && matches!(decision, ImprovementDecision::Better(_));
    let decision_text = match &decision {
        ImprovementDecision::Better(reason)
        | ImprovementDecision::NotBetter(reason)
        | ImprovementDecision::Inconclusive(reason) => reason.clone(),
    };
    let resolved = if keep {
        ResolvedIteration {
            incumbent_state: incumbent.state.digest.clone(),
            status: "keep".to_string(),
            pulse: "kept",
            result: candidate_report.decisive.clone(),
            description: envelope.description.clone(),
            unvalidated: envelope.unvalidated.clone(),
            snapshot: candidate.clone(),
            report: Some(candidate_report),
        }
    } else {
        restore_iteration_snapshot(worktree, run, incumbent)?;
        ResolvedIteration {
            incumbent_state: incumbent.state.digest.clone(),
            status: "discard".to_string(),
            pulse: "restored",
            result: decision_text,
            description: envelope.description.clone(),
            unvalidated: envelope.unvalidated.clone(),
            snapshot: incumbent.clone(),
            report: Some(candidate_report),
        }
    };
    publish_iteration(
        run,
        iteration,
        evidence_relative,
        &resolved,
        Some(&worker),
        Some((&candidate, &delta, &envelope)),
    )?;
    Ok(resolved)
}

fn restore_iteration_snapshot(
    worktree: &super::Worktree,
    run: &mut LoopRun,
    snapshot: &RepositorySnapshot,
) -> Result<()> {
    run.update(|state| state.phase = RunPhase::Restoring)?;
    worktree.restore(run.root(), snapshot)?;
    run.update(|state| state.phase = RunPhase::Iteration)
}

fn block_integrity(
    worktree: &super::Worktree,
    run: &mut LoopRun,
    incumbent: &RepositorySnapshot,
    iteration: usize,
    evidence_relative: &str,
    worker: &PhaseOutput,
    envelope: &WorkerEnvelope,
    description: String,
) -> Result<ResolvedIteration> {
    block_integrity_raw(
        worktree,
        run,
        incumbent,
        iteration,
        evidence_relative,
        Some(worker),
        &envelope.unvalidated,
        description,
    )
}

#[allow(clippy::too_many_arguments)]
fn block_integrity_raw(
    worktree: &super::Worktree,
    run: &mut LoopRun,
    incumbent: &RepositorySnapshot,
    iteration: usize,
    evidence_relative: &str,
    phase: Option<&PhaseOutput>,
    unvalidated: &str,
    description: String,
) -> Result<ResolvedIteration> {
    restore_iteration_snapshot(worktree, run, incumbent)?;
    let resolved = ResolvedIteration {
        incumbent_state: incumbent.state.digest.clone(),
        status: "blocked".to_string(),
        pulse: "blocked",
        result: "state integrity failure".to_string(),
        description,
        unvalidated: unvalidated.to_string(),
        snapshot: incumbent.clone(),
        report: None,
    };
    publish_iteration(run, iteration, evidence_relative, &resolved, phase, None)?;
    Ok(resolved)
}

fn publish_iteration(
    run: &mut LoopRun,
    iteration: usize,
    evidence_relative: &str,
    resolved: &ResolvedIteration,
    phase: Option<&PhaseOutput>,
    candidate: Option<(
        &RepositorySnapshot,
        &super::repository::SnapshotDelta,
        &WorkerEnvelope,
    )>,
) -> Result<()> {
    let resolved_path = format!("{evidence_relative}/resolved.json");
    let (candidate_state, created, modified, deleted, model_verdict, evidence) =
        if let Some((candidate, delta, envelope)) = candidate {
            (
                candidate.state.digest.as_str(),
                delta.created.as_slice(),
                delta.modified.as_slice(),
                delta.deleted.as_slice(),
                match envelope.verdict {
                    WorkerVerdict::Keep => "KEEP",
                    WorkerVerdict::Discard => "DISCARD",
                    WorkerVerdict::Blocked => "BLOCKED",
                },
                envelope.evidence.as_str(),
            )
        } else {
            ("none", &[][..], &[][..], &[][..], "none", "none")
        };
    let record = IterationRecord {
        iteration,
        session_id: phase.map_or("none", |phase| phase.session_id.as_str()),
        usage: phase.and_then(|phase| phase.usage.as_ref()),
        session_duration_ms: phase.map(|phase| phase.duration_ms),
        incumbent_state: &resolved.incumbent_state,
        candidate_state,
        model_verdict,
        resolved_status: &resolved.status,
        description: &resolved.description,
        result: &resolved.result,
        evidence,
        created,
        modified,
        deleted,
        unvalidated: &resolved.unvalidated,
    };
    run.write_json(&resolved_path, &record)?;
    run.publish_iteration(&LedgerRow {
        iteration,
        state: &resolved.snapshot.state.digest,
        result: &resolved.result,
        status: &resolved.status,
        description: &resolved.description,
        evidence: &resolved_path,
    })
}

async fn run_live_phase(
    cwd: &Path,
    run: &LoopRun,
    frozen: &FrozenLoopContext,
    name: &str,
    prompt: &str,
    parent_control: &TurnControl,
) -> Result<Option<PhaseOutput>> {
    let session_root = run.session_root(name)?;
    let mut agent = Agent::new_frozen_loop_session(cwd, &session_root, frozen, prompt)?;
    let session_id = agent.session_id().to_string();
    let processes = agent.background_processes();
    let started = Instant::now();
    let result = agent
        .submit_preloaded_with_control(None, parent_control.child_non_steerable())
        .await;
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let usage = agent.latest_usage();
    processes.stop_all_background_processes();
    if !processes.list_background_processes().is_empty() {
        return Err(anyhow!(
            "phase-owned background processes could not be contained"
        ));
    }
    drop(agent);
    std::fs::remove_dir_all(&session_root).with_context(|| {
        format!(
            "failed to remove private internal session state {}",
            session_root.display()
        )
    })?;
    match result? {
        SubmitOutcome::Completed(response) => Ok(Some(PhaseOutput {
            response,
            session_id,
            usage,
            duration_ms,
        })),
        SubmitOutcome::Cancelled => Ok(None),
    }
}

fn evaluator_prompt(run: &LoopRun, worktree: &super::Worktree, workspace: &Path) -> Result<String> {
    let home = crate::paths::bettercodex_home()
        .ok_or_else(|| anyhow!("cannot locate the installed loop evaluator manifest"))?;
    let manifest = crate::system_skills::root(&home).join("loop/references/evals-manifest.md");
    #[cfg(test)]
    let manifest = if manifest.is_file() {
        manifest
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/evals/MANIFEST.md")
    };
    if !manifest.is_file() {
        return Err(anyhow!(
            "installed loop evaluator manifest is unavailable at {}",
            manifest.display()
        ));
    }
    let addendum = CONTRACT_PROMPT
        .replace("{{run_root}}", &run.root().display().to_string())
        .replace("{{worktree_root}}", &worktree.root().display().to_string())
        .replace("{{evaluator_workspace}}", &workspace.display().to_string())
        .replace("{{eval_manifest}}", &manifest.display().to_string())
        .replace(
            "{{starting_state_file}}",
            &run.root().join("starting-state.json").display().to_string(),
        );
    Ok(format!(
        "{}\n\n{}",
        EVALUATOR_PROMPT.trim(),
        addendum.trim()
    ))
}

fn worker_prompt(
    run: &LoopRun,
    iteration: usize,
    total: usize,
    evidence_directory: &Path,
    contract_path: &Path,
) -> Result<String> {
    let prompt = WORKER_PROMPT
        .replace("{{iteration}}", &iteration.to_string())
        .replace("{{total_iterations}}", &total.to_string());
    let executable =
        std::env::current_exe().context("failed to locate the running bettercodex binary")?;
    Ok(format!(
        "{}\n\n# Harness-owned loop locations\n\nFrozen contract: `{}`\nBaseline and experiment ledger: `{}` and `{}`\nWritable evidence directory for this iteration: `{}`\nIncumbent state record: `{}`\nCandidate state identity argv: `{:?}`\n\nThe state command is the literal argument vector shown above: run it after restoring evaluator scratch and before writing evidence, then copy its JSON `digest` into the artifact's `state` field. The `EVIDENCE` field must name a regular JSON file under that evidence directory using a run-relative path. It must contain that current candidate `state` identity and a `checks` object naming every frozen check. Each machine result may include `passed`; each model result must include boolean `passed` and concrete `artifacts`. Run the frozen checks and bind the artifact to the candidate state before returning the terminal envelope.",
        prompt.trim(),
        contract_path.display(),
        run.root().join("baseline/evaluation.json").display(),
        run.root().join("results.tsv").display(),
        evidence_directory.display(),
        evidence_directory.join("incumbent-state.json").display(),
        [
            executable.as_os_str(),
            std::ffi::OsStr::new("--internal-loop-state"),
            run.root().as_os_str(),
            contract_path.as_os_str(),
        ],
    ))
}

fn validate_contract_artifacts(
    run: &LoopRun,
    contract: &super::EvaluatorContract,
    workspace: &Path,
) -> Result<()> {
    let rationale = run.resolve_existing_file("evaluator/workspace/RATIONALE.md", workspace)?;
    if std::fs::metadata(&rationale)?.len() == 0 {
        return Err(anyhow!("evaluator rationale is empty"));
    }
    for check in &contract.model_checks {
        run.resolve_existing_file(&check.rubric_path, workspace)?;
        for path in &check.calibration_paths {
            run.resolve_existing_file(path, workspace)?;
        }
    }
    for artifact in contract
        .machine_checks
        .iter()
        .chain(
            contract
                .discrimination_checks
                .iter()
                .map(|probe| &probe.check),
        )
        .flat_map(|check| check.input_paths.iter().chain(&check.fixture_paths))
        .filter(|artifact| artifact.root == ArtifactRoot::Evaluator)
    {
        run.resolve_existing_file(&artifact.path, workspace)?;
    }
    Ok(())
}

fn validate_evidence_state(path: &Path, expected: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err(anyhow!("worker evidence exceeds the evidence size limit"));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)
        .context("worker evidence is not valid JSON")?;
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("worker evidence omitted its state identity"))?;
    if state != expected {
        return Err(anyhow!(
            "worker evidence names stale state `{state}` instead of `{expected}`"
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockerArtifact {
    #[serde(rename = "kind")]
    _kind: BlockerKind,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BlockerKind {
    TaskEvaluatorContradiction,
    MissingPrerequisite,
    MissingAuthority,
    StateIntegrity,
    ExternalConflict,
}

fn supported_blocker(path: &Path) -> Result<bool> {
    let document: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let Some(value) = document.get("blocker") else {
        return Ok(false);
    };
    let blocker: BlockerArtifact = serde_json::from_value(value.clone())?;
    let detail = blocker.detail.trim();
    let evidence_is_concrete = !blocker.evidence.is_empty()
        && blocker
            .evidence
            .iter()
            .all(|item| !item.trim().is_empty() && item.chars().count() <= 4_096);
    Ok(!detail.is_empty() && detail.chars().count() <= 4_096 && evidence_is_concrete)
}

fn immutable_control_digest(root: &Path, active_iteration: Option<usize>) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_control_tree(root, root, active_iteration, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_control_digest(root: &Path, active: Option<usize>, expected: &str) -> Result<()> {
    let actual = immutable_control_digest(root, active)?;
    if actual != expected {
        return Err(anyhow!(
            "worker or evaluator modified harness-owned loop evidence"
        ));
    }
    Ok(())
}

fn hash_control_tree(
    root: &Path,
    path: &Path,
    active_iteration: Option<usize>,
    hasher: &mut Sha256,
) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    if mutable_phase_path(relative, active_iteration) {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path)?;
    hasher.update(relative.as_os_str().as_encoded_bytes());
    hasher.update(metadata.permissions().mode().to_le_bytes());
    hasher.update(metadata.nlink().to_le_bytes());
    if metadata.file_type().is_symlink() {
        hasher.update(b"link");
        hasher.update(std::fs::read_link(path)?.as_os_str().as_encoded_bytes());
    } else if metadata.is_dir() {
        hasher.update(b"dir");
        let mut entries = std::fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            hash_control_tree(root, &entry.path(), active_iteration, hasher)?;
        }
    } else if metadata.is_file() {
        hasher.update(b"file");
        hasher.update(std::fs::read(path)?);
    }
    Ok(())
}

fn mutable_phase_path(relative: &Path, active_iteration: Option<usize>) -> bool {
    let evaluator_workspace = Path::new("evaluator/workspace");
    let allowed_session = active_iteration.map_or_else(
        || PathBuf::from("sessions/setup"),
        |iteration| PathBuf::from(format!("sessions/worker-{iteration}")),
    );
    let allowed_iteration =
        active_iteration.map(|iteration| PathBuf::from(format!("iterations/{iteration}")));
    relative == allowed_session
        || relative.starts_with(&allowed_session)
        || active_iteration.is_none()
            && (relative == evaluator_workspace || relative.starts_with(evaluator_workspace))
        || allowed_iteration
            .as_ref()
            .is_some_and(|allowed| relative == allowed.as_path() || relative.starts_with(allowed))
}

fn load_snapshot(run: &LoopRun, relative: &str) -> Result<RepositorySnapshot> {
    super::Worktree::load_snapshot(&run.root().join(relative))
}

fn progress(
    events: &UnboundedSender<AgentEvent>,
    run: &LoopRun,
    phase: &str,
    pulse: &str,
    result: Option<&str>,
    diff: Option<(u64, u64)>,
) {
    let pulse = result.map_or_else(|| pulse.to_string(), |result| format!("{pulse} · {result}"));
    let (additions, deletions) = diff
        .map(|(additions, deletions)| (Some(additions), Some(deletions)))
        .unwrap_or((None, None));
    let _ = events.send(AgentEvent::LoopProgress(LoopProgress::new(
        &run.state.display_name,
        phase,
        additions,
        deletions,
        &pulse,
    )));
}

fn interrupted_setup_summary(snapshot: &RepositorySnapshot) -> LoopSummary {
    LoopSummary {
        setup: "interrupted".to_string(),
        baseline: None,
        final_report: None,
        counts: OutcomeCounts {
            interrupted: 1,
            ..OutcomeCounts::default()
        },
        final_state: snapshot.state.digest.clone(),
        blind_spots: Vec::new(),
        unvalidated: Vec::new(),
        blocked: None,
        interrupted: true,
    }
}

fn blind_spots(contract: &super::EvaluatorContract) -> Vec<String> {
    contract
        .uncovered
        .iter()
        .chain(&contract.known_loopholes)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn ensure_comparable_baseline(
    contract: &super::EvaluatorContract,
    report: &EvaluationReport,
) -> Result<()> {
    if contract.comparison.kind == ComparisonKind::Metric {
        let check_id = contract
            .comparison
            .check_id
            .as_deref()
            .ok_or_else(|| anyhow!("metric comparison omitted its check ID"))?;
        if super::evaluator::metric_value(report, check_id).is_none() {
            return Err(anyhow!(
                "baseline metric `{check_id}` did not produce comparable finite values"
            ));
        }
    }
    Ok(())
}

fn verify_evaluator_side_effects(
    worktree: &super::Worktree,
    contract: &super::EvaluatorContract,
    before: &RepositorySnapshot,
    after: &RepositorySnapshot,
) -> Result<()> {
    let delta = worktree.delta(before, after);
    if delta.git_changed {
        return Err(anyhow!("evaluator changed Git state"));
    }
    let scratch_is_writable = contract
        .machine_checks
        .iter()
        .chain(
            contract
                .discrimination_checks
                .iter()
                .map(|probe| &probe.check),
        )
        .any(|check| check.side_effects == SideEffects::DeclaredScratch);
    let outside = delta
        .paths()
        .filter(|path| {
            !scratch_is_writable
                || !contract
                    .scratch_paths
                    .iter()
                    .any(|spec| spec.covers(Path::new(path)))
        })
        .take(16)
        .collect::<Vec<_>>();
    if !outside.is_empty() {
        return Err(anyhow!(
            "evaluator changed paths outside declared scratch: {}",
            outside.join(", ")
        ));
    }
    Ok(())
}

fn format_final(run: &LoopRun, summary: &LoopSummary) -> String {
    let baseline = summary
        .baseline
        .as_ref()
        .map_or("unavailable", |report| report.decisive.as_str());
    let final_result = summary
        .final_report
        .as_ref()
        .map_or("unavailable", |report| report.decisive.as_str());
    let blind_spots = if summary.blind_spots.is_empty() {
        "none".to_string()
    } else {
        summary.blind_spots.join("; ")
    };
    let unvalidated = if summary.unvalidated.is_empty() {
        "none".to_string()
    } else {
        summary.unvalidated.join("; ")
    };
    let blocker = summary.blocked.as_deref().unwrap_or("none");
    format!(
        "Quality loop {}\n\nRun: {}\nEvaluator setup: {}\nBaseline: {}\nFinal: {}\nIterations: {} kept, {} discarded, {} crashed, {} blocked, {} interrupted\nRepository state: {}\nBlocker: {}\nEvaluator blind spots: {}\nUnvalidated: {}",
        run.run_id(),
        run.root().display(),
        summary.setup,
        baseline,
        final_result,
        summary.counts.kept,
        summary.counts.discarded,
        summary.counts.crashed,
        summary.counts.blocked,
        summary.counts.interrupted,
        summary.final_state,
        blocker,
        blind_spots,
        unvalidated,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality_loop::LoopInvocation;
    use crate::rollout::OperatorInputRecord;
    use serde_json::json;
    use std::collections::HashSet;
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use uuid::Uuid;

    #[derive(Clone, Copy)]
    enum ScriptedAction {
        Keep(i32),
        Discard(i32),
        KeepWithoutChange,
        Crash(i32),
        InvalidBlocked,
        ValidBlocked,
        TamperEvaluator,
        TamperControl,
        Interrupt(i32),
    }

    struct CapturedPhase {
        name: String,
        prompt: String,
        context: Vec<Value>,
        session_id: String,
    }

    struct ScriptedRunner {
        actions: VecDeque<ScriptedAction>,
        captured: Vec<CapturedPhase>,
    }

    impl ScriptedRunner {
        fn new(actions: impl IntoIterator<Item = ScriptedAction>) -> Self {
            Self {
                actions: actions.into_iter().collect(),
                captured: Vec::new(),
            }
        }

        fn setup(&mut self, cwd: &Path, run: &LoopRun, session_id: &str) -> Result<PhaseOutput> {
            let workspace = run.root().join("evaluator/workspace");
            let script = workspace.join("metric.sh");
            let known_failure = workspace.join("known-failure.txt");
            std::fs::write(&known_failure, "known failure\n")?;
            std::fs::write(
                &script,
                "#!/bin/sh\nif [ \"$1\" = --known-failure ]; then\n  [ \"$(cat \"$2\")\" = \"known failure\" ] || exit 1\n  exit 0\nfi\nmkdir -p scratch\nprintf check > scratch/check.log\nvalue=$(cat candidate.txt)\nif [ \"$value\" = 77 ]; then printf illegal > evaluator-side-effect.txt; fi\nprintf '{\"value\": %s}\\n' \"$value\"\n",
            )?;
            let mut permissions = std::fs::metadata(&script)?.permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&script, permissions)?;
            std::fs::write(
                workspace.join("RATIONALE.md"),
                "# Metric evaluator\n\nThe local numeric check measures the candidate directly.\n",
            )?;
            let contract = json!({
                "version": 1,
                "loop_name": "Metric speed",
                "promises": [{
                    "id": "metric",
                    "class": "improvement",
                    "statement": "the candidate metric increases",
                    "failure_mode": "the metric ties or decreases",
                    "method": "local numeric check",
                    "required_evidence": ["numeric result"]
                }],
                "candidate_paths": ["candidate.txt"],
                "fixed_constraints": ["preserve operator work"],
                "integrity_paths": [],
                "scratch_paths": ["scratch/**"],
                "machine_checks": [{
                    "id": "metric-check",
                    "promise_ids": ["metric"],
                    "argv": [script],
                    "cwd": ".",
                    "env": {},
                    "input_paths": [{"root": "worktree", "path": "candidate.txt"}],
                    "fixture_paths": [],
                    "timeout_seconds": 5,
                    "resource_budget": "one local process",
                    "side_effects": "declared_scratch",
                    "approval": "none",
                    "expected_exit_codes": [0],
                    "extract": {"kind": "json_number", "json_pointer": "/value"},
                    "baseline_repeats": 1
                }],
                "discrimination_checks": [{
                    "linked_check_id": "metric-check",
                    "check": {
                        "id": "metric-known-failure",
                        "promise_ids": ["metric"],
                        "argv": [script, "--known-failure", known_failure],
                        "cwd": ".",
                        "env": {},
                        "input_paths": [{"root": "worktree", "path": "candidate.txt"}],
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
                "acceptance": {"required_check_ids": ["metric-check"]},
                "comparison": {
                    "kind": "metric",
                    "check_id": "metric-check",
                    "direction": "higher",
                    "minimum_delta": 1.0,
                    "tolerance": 0.0,
                    "ties": "discard",
                    "inconclusive": "discard"
                },
                "environment": ["test fixture"],
                "uncovered": ["subjective qualities"],
                "known_loopholes": ["none"]
            });
            std::fs::write(
                workspace.join("contract.json"),
                serde_json::to_vec_pretty(&contract)?,
            )?;
            let starting: Value =
                serde_json::from_slice(&std::fs::read(run.root().join("starting-state.json"))?)?;
            std::fs::write(
                workspace.join("baseline.json"),
                serde_json::to_vec_pretty(&json!({
                    "state": starting["digest"],
                    "checks": {"metric-check": {"passed": true, "artifacts": []}}
                }))?,
            )?;
            std::fs::write(cwd.join("setup-leak.txt"), "must be restored\n")?;
            Ok(PhaseOutput {
                response: "SETUP: READY\nCONTRACT: evaluator/workspace/contract.json\nBASELINE: evaluator/workspace/baseline.json\nBLOCKER: none".to_string(),
                session_id: session_id.to_string(),
                usage: None,
                duration_ms: 1,
            })
        }

        fn worker(
            &mut self,
            cwd: &Path,
            run: &LoopRun,
            name: &str,
            session_id: &str,
            control: &TurnControl,
        ) -> Result<Option<PhaseOutput>> {
            let action = self
                .actions
                .pop_front()
                .ok_or_else(|| anyhow!("script omitted an iteration action"))?;
            let iteration = name
                .strip_prefix("worker-")
                .ok_or_else(|| anyhow!("invalid scripted worker name"))?
                .parse::<usize>()?;
            let (verdict, description, unvalidated, blocker) = match action {
                ScriptedAction::Keep(value) => {
                    std::fs::write(cwd.join("candidate.txt"), format!("{value}\n"))?;
                    ("KEEP", "raised the candidate metric", "none", None)
                }
                ScriptedAction::Discard(value) => {
                    std::fs::write(cwd.join("candidate.txt"), format!("{value}\n"))?;
                    (
                        "DISCARD",
                        "declined the provisional candidate",
                        "none",
                        None,
                    )
                }
                ScriptedAction::KeepWithoutChange => {
                    ("KEEP", "claimed an unchanged candidate", "none", None)
                }
                ScriptedAction::Crash(value) => {
                    std::fs::write(cwd.join("candidate.txt"), format!("{value}\n"))?;
                    return Err(anyhow!("scripted worker crash"));
                }
                ScriptedAction::InvalidBlocked => {
                    ("BLOCKED", "the task is already finished", "none", None)
                }
                ScriptedAction::ValidBlocked => (
                    "BLOCKED",
                    "a required prerequisite is unavailable",
                    "the fixture lacks the prerequisite",
                    Some(json!({
                        "kind": "missing_prerequisite",
                        "detail": "the fixture lacks the required local prerequisite",
                        "evidence": ["fixture preflight reported the missing prerequisite"]
                    })),
                ),
                ScriptedAction::TamperEvaluator => {
                    std::fs::write(
                        run.root().join("evaluator/workspace/RATIONALE.md"),
                        "weakened ruler\n",
                    )?;
                    ("KEEP", "tampered with the evaluator", "none", None)
                }
                ScriptedAction::TamperControl => {
                    std::fs::write(run.root().join("baseline/evaluation.json"), b"{}\n")?;
                    ("KEEP", "tampered with prior evidence", "none", None)
                }
                ScriptedAction::Interrupt(value) => {
                    std::fs::write(cwd.join("candidate.txt"), format!("{value}\n"))?;
                    control.cancellation().cancel();
                    return Ok(None);
                }
            };
            let contract = run.root().join("evaluator/workspace/contract.json");
            let state = crate::quality_loop::capture_state_identity(cwd, run.root(), &contract)?;
            let evidence_relative = format!("iterations/{iteration}/worker.json");
            let mut evidence = json!({
                "state": state["digest"],
                "checks": {"metric-check": {"passed": true}}
            });
            if let Some(blocker) = blocker {
                evidence["blocker"] = blocker;
            }
            std::fs::write(
                run.root().join(&evidence_relative),
                serde_json::to_vec_pretty(&evidence)?,
            )?;
            Ok(Some(PhaseOutput {
                response: format!(
                    "VERDICT: {verdict}\nDESCRIPTION: {description}\nEVIDENCE: {evidence_relative}\nUNVALIDATED: {unvalidated}"
                ),
                session_id: session_id.to_string(),
                usage: None,
                duration_ms: 1,
            }))
        }
    }

    impl PhaseRunner for ScriptedRunner {
        fn run<'a>(
            &'a mut self,
            cwd: &'a Path,
            run: &'a LoopRun,
            frozen: &'a FrozenLoopContext,
            name: &'a str,
            prompt: &'a str,
            parent_control: &'a TurnControl,
        ) -> Pin<Box<dyn Future<Output = Result<Option<PhaseOutput>>> + Send + 'a>> {
            Box::pin(async move {
                let session_id = Uuid::new_v4().to_string();
                self.captured.push(CapturedPhase {
                    name: name.to_string(),
                    prompt: prompt.to_string(),
                    context: frozen.context_items().to_vec(),
                    session_id: session_id.clone(),
                });
                if name == "setup" {
                    self.setup(cwd, run, &session_id).map(Some)
                } else {
                    self.worker(cwd, run, name, &session_id, parent_control)
                }
            })
        }
    }

    struct Fixture {
        root: PathBuf,
        worktree: super::super::Worktree,
        frozen: FrozenLoopContext,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("bettercodex-loop-engine-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            git(&root, &["init", "-q"]);
            git(&root, &["config", "user.name", "Loop Test"]);
            git(&root, &["config", "user.email", "loop@example.invalid"]);
            std::fs::write(root.join("candidate.txt"), "10\n").unwrap();
            std::fs::write(root.join("operator.txt"), "committed\n").unwrap();
            std::fs::write(root.join("AGENTS.md"), "Frozen loop instruction.\n").unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-qm", "base"]);
            std::fs::write(root.join("operator.txt"), "dirty operator work\n").unwrap();
            let record = OperatorInputRecord {
                message: json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "improve the metric $loop"}]
                }),
                prompt_text: "improve the metric $loop".to_string(),
                selected_skills: Vec::new(),
                skill_context: Vec::new(),
            };
            let (frozen, warnings) = FrozenLoopContext::capture(&root, &[record]).unwrap();
            assert!(warnings.is_empty());
            let worktree = super::super::Worktree::discover(&root).unwrap();
            Self {
                root,
                worktree,
                frozen,
            }
        }

        async fn run(
            &self,
            actions: Vec<ScriptedAction>,
        ) -> (LoopRun, LoopSummary, ScriptedRunner, Vec<AgentEvent>) {
            let invocation = LoopInvocation {
                iterations: actions.len(),
                triggers: Vec::new(),
                counts: Vec::new(),
            };
            let mut run = LoopRun::create(
                &self.worktree,
                &invocation,
                self.frozen.operator_inputs(),
                self.frozen.context_items(),
            )
            .unwrap();
            let original = load_snapshot(&run, &run.state.starting_snapshot).unwrap();
            run.write_json("starting-state.json", &original.state)
                .unwrap();
            let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
            let (_handle, control) = TurnControl::non_steerable_channel();
            let mut runner = ScriptedRunner::new(actions);
            let summary = execute_phases(
                &self.worktree,
                &self.frozen,
                &mut run,
                &original,
                &events_tx,
                &control,
                &mut runner,
            )
            .await
            .unwrap();
            drop(events_tx);
            let mut events = Vec::new();
            while let Ok(event) = events_rx.try_recv() {
                events.push(event);
            }
            (run, summary, runner, events)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(root: &Path, args: &[&str]) {
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
    }

    #[tokio::test]
    async fn scripted_loop_runs_one_evaluator_and_the_exact_sequential_worker_count() {
        let fixture = Fixture::new();
        let (run, summary, runner, events) = fixture
            .run(vec![
                ScriptedAction::Keep(12),
                ScriptedAction::Keep(11),
                ScriptedAction::Crash(99),
            ])
            .await;
        assert_eq!(
            (
                summary.counts.kept,
                summary.counts.discarded,
                summary.counts.crashed
            ),
            (1, 1, 1)
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("candidate.txt")).unwrap(),
            "12\n"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("operator.txt")).unwrap(),
            "dirty operator work\n"
        );
        assert!(!fixture.root.join("setup-leak.txt").exists());
        assert!(!fixture.root.join("scratch").exists());
        assert_eq!(
            runner
                .captured
                .iter()
                .map(|phase| phase.name.as_str())
                .collect::<Vec<_>>(),
            ["setup", "worker-1", "worker-2", "worker-3"]
        );
        assert_eq!(
            runner
                .captured
                .iter()
                .map(|phase| phase.session_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            4
        );
        assert!(
            runner.captured[0]
                .prompt
                .contains("# Build the task evaluator")
        );
        assert!(runner.captured[1].prompt.contains("# Beat the incumbent"));
        assert!(
            runner
                .captured
                .iter()
                .all(|phase| phase.context == runner.captured[0].context)
        );
        let ledger = std::fs::read_to_string(run.root().join("results.tsv")).unwrap();
        assert_eq!(ledger.lines().count(), 5);
        assert!(ledger.contains("\tkeep\t"));
        assert!(ledger.contains("\tdiscard\t"));
        assert!(ledger.contains("\tcrash\t"));
        let attempt: Value = serde_json::from_slice(
            &std::fs::read(run.root().join("iterations/1/resolved.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(attempt["session_duration_ms"], 1);
        assert!(attempt["usage"].is_null());
        assert!(
            attempt["session_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(
            std::fs::read_dir(run.root().join("sessions"))
                .unwrap()
                .count(),
            0
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::LoopProgress(progress)
                if progress.phase == "1/3" && progress.additions == Some(1) && progress.deletions == Some(1)
        )));
        drop(run);
    }

    #[tokio::test]
    async fn worker_verdicts_cannot_bypass_harness_comparison_or_exact_count() {
        let fixture = Fixture::new();
        let (_run, summary, runner, _) = fixture
            .run(vec![
                ScriptedAction::Discard(30),
                ScriptedAction::KeepWithoutChange,
                ScriptedAction::InvalidBlocked,
                ScriptedAction::Keep(13),
            ])
            .await;
        assert_eq!(summary.counts.kept, 1);
        assert_eq!(summary.counts.discarded, 2);
        assert_eq!(summary.counts.crashed, 1);
        assert_eq!(runner.captured.len(), 5);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("candidate.txt")).unwrap(),
            "13\n"
        );
    }

    #[tokio::test]
    async fn accepted_blocker_stops_later_workers_but_evaluator_tampering_blocks_first() {
        let fixture = Fixture::new();
        let (_run, summary, runner, _) = fixture
            .run(vec![ScriptedAction::ValidBlocked, ScriptedAction::Keep(20)])
            .await;
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(runner.captured.len(), 2);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("candidate.txt")).unwrap(),
            "10\n"
        );

        let second = Fixture::new();
        let (_run, summary, runner, _) = second
            .run(vec![
                ScriptedAction::TamperEvaluator,
                ScriptedAction::Keep(20),
            ])
            .await;
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(runner.captured.len(), 2);
        assert_eq!(
            std::fs::read_to_string(second.root.join("candidate.txt")).unwrap(),
            "10\n"
        );

        let third = Fixture::new();
        let (_run, summary, runner, _) = third
            .run(vec![
                ScriptedAction::TamperControl,
                ScriptedAction::Keep(20),
            ])
            .await;
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(runner.captured.len(), 2);
        assert_eq!(
            std::fs::read_to_string(third.root.join("candidate.txt")).unwrap(),
            "10\n"
        );
    }

    #[tokio::test]
    async fn harness_check_side_effects_block_without_retaining_the_candidate() {
        let fixture = Fixture::new();
        let (_run, summary, runner, _) = fixture
            .run(vec![ScriptedAction::Keep(77), ScriptedAction::Keep(20)])
            .await;
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(summary.counts.kept, 0);
        assert_eq!(runner.captured.len(), 2);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("candidate.txt")).unwrap(),
            "10\n"
        );
        assert!(!fixture.root.join("evaluator-side-effect.txt").exists());
    }

    #[tokio::test]
    async fn interruption_restores_the_incumbent_and_starts_no_later_worker() {
        let fixture = Fixture::new();
        let (run, summary, runner, _) = fixture
            .run(vec![
                ScriptedAction::Interrupt(50),
                ScriptedAction::Keep(60),
            ])
            .await;
        assert!(summary.interrupted);
        assert_eq!(summary.counts.interrupted, 1);
        assert_eq!(runner.captured.len(), 2);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("candidate.txt")).unwrap(),
            "10\n"
        );
        let ledger = std::fs::read_to_string(run.root().join("results.tsv")).unwrap();
        assert!(ledger.contains("\tinterrupted\t"));
    }
}
