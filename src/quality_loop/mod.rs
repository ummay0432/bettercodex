mod contract;
mod engine;
mod envelope;
mod evaluator;
mod integrity;
mod parser;
mod progress;
mod repository;
mod state;

pub(crate) use contract::EvaluatorContract;
pub(crate) use contract::PathSpec;
pub(crate) use engine::submit_with_control;
pub(crate) use envelope::SetupVerdict;
pub(crate) use envelope::WorkerEnvelope;
pub(crate) use envelope::WorkerVerdict;
pub(crate) use envelope::parse_setup_envelope;
pub(crate) use envelope::parse_worker_envelope;
pub(crate) use evaluator::EvaluationReport;
pub(crate) use evaluator::ImprovementDecision;
pub(crate) use evaluator::apply_structured_artifact;
pub(crate) use evaluator::compare_reports;
pub(crate) use evaluator::run_machine_evaluation;
pub(crate) use integrity::PackageManifest;

pub(crate) use parser::LoopInvocation;
#[cfg(test)]
pub(crate) use parser::parse_invocation;
pub(crate) use parser::parse_invocation_with_mode;
pub(crate) use progress::LoopProgress;
pub(crate) use progress::truncate_width;
pub(crate) use repository::RepositorySnapshot;
pub(crate) use repository::Worktree;
pub(crate) use state::LoopRun;
pub(crate) use state::RunPhase;

pub(crate) const DEFAULT_ITERATIONS: usize = 3;

pub(crate) fn capture_state_identity(
    cwd: &std::path::Path,
    run_root: &std::path::Path,
    contract_path: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let worktree = Worktree::discover(cwd)?;
    let loops = worktree.root().join(".bcodex/loops");
    let canonical_loops = loops.canonicalize()?;
    let canonical_run = run_root.canonicalize()?;
    if !canonical_run.starts_with(&canonical_loops)
        || canonical_run.parent() != Some(canonical_loops.as_path())
    {
        anyhow::bail!("internal loop state helper received an invalid run directory");
    }
    let canonical_contract = contract_path.canonicalize()?;
    if !canonical_contract.starts_with(canonical_run.join("evaluator/workspace")) {
        anyhow::bail!("internal loop state helper received an invalid contract path");
    }
    let state: state::RunState =
        serde_json::from_slice(&std::fs::read(canonical_run.join("state.json"))?)?;
    state::verify_runtime_state(&state)?;
    let iteration = state.active_iteration.ok_or_else(|| {
        anyhow::anyhow!("internal loop state helper is available only during an active iteration")
    })?;
    if state.phase != state::RunPhase::Iteration {
        anyhow::bail!("internal loop state helper is unavailable outside a working iteration");
    }
    let contract = EvaluatorContract::load(&canonical_contract, worktree.root())?;
    let capture_root = canonical_run
        .join("iterations")
        .join(iteration.to_string())
        .join("identity-capture");
    let snapshot = worktree.capture(&capture_root, &contract.snapshot_paths())?;
    Ok(serde_json::to_value(snapshot.state)?)
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod parser_tests;

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod envelope_tests;
