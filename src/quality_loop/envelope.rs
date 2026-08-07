use anyhow::Result;
use anyhow::anyhow;

const MAX_ENVELOPE_VALUE_CHARS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupVerdict {
    Ready,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupEnvelope {
    pub(crate) verdict: SetupVerdict,
    pub(crate) contract: String,
    pub(crate) baseline: String,
    pub(crate) blocker: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerVerdict {
    Keep,
    Discard,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerEnvelope {
    pub(crate) verdict: WorkerVerdict,
    pub(crate) description: String,
    pub(crate) evidence: String,
    pub(crate) unvalidated: String,
}

pub(crate) fn parse_setup_envelope(response: &str) -> Result<SetupEnvelope> {
    let values = terminal_envelope(response, &["SETUP", "CONTRACT", "BASELINE", "BLOCKER"])?;
    let verdict = match values[0].as_str() {
        "READY" => SetupVerdict::Ready,
        "BLOCKED" => SetupVerdict::Blocked,
        value => return Err(anyhow!("invalid evaluator setup verdict `{value}`")),
    };
    let contract = values[1].clone();
    let baseline = values[2].clone();
    let blocker = values[3].clone();
    match verdict {
        SetupVerdict::Ready => {
            if contract.eq_ignore_ascii_case("none")
                || baseline.eq_ignore_ascii_case("none")
                || !blocker.eq_ignore_ascii_case("none")
            {
                return Err(anyhow!(
                    "READY setup requires contract and baseline paths and `BLOCKER: none`"
                ));
            }
        }
        SetupVerdict::Blocked => {
            if blocker.eq_ignore_ascii_case("none") || blocker.trim().is_empty() {
                return Err(anyhow!("BLOCKED setup requires a concise blocker"));
            }
        }
    }
    Ok(SetupEnvelope {
        verdict,
        contract,
        baseline,
        blocker,
    })
}

pub(crate) fn parse_worker_envelope(response: &str) -> Result<WorkerEnvelope> {
    let values = terminal_envelope(
        response,
        &["VERDICT", "DESCRIPTION", "EVIDENCE", "UNVALIDATED"],
    )?;
    let verdict = match values[0].as_str() {
        "KEEP" => WorkerVerdict::Keep,
        "DISCARD" => WorkerVerdict::Discard,
        "BLOCKED" => WorkerVerdict::Blocked,
        value => return Err(anyhow!("invalid worker verdict `{value}`")),
    };
    if values[1].trim().is_empty() || values[1].eq_ignore_ascii_case("none") {
        return Err(anyhow!("worker envelope requires a factual description"));
    }
    if values[2].trim().is_empty() || values[2].eq_ignore_ascii_case("none") {
        return Err(anyhow!("worker envelope requires an evidence path"));
    }
    Ok(WorkerEnvelope {
        verdict,
        description: values[1].clone(),
        evidence: values[2].clone(),
        unvalidated: values[3].clone(),
    })
}

fn terminal_envelope(response: &str, fields: &[&str; 4]) -> Result<[String; 4]> {
    let response = response.trim_end_matches(['\r', '\n']);
    let lines = response.lines().collect::<Vec<_>>();
    if lines.len() < fields.len() {
        return Err(anyhow!("response omitted the terminal four-line envelope"));
    }
    let envelope = &lines[lines.len() - fields.len()..];
    let mut values = Vec::with_capacity(fields.len());
    for (line, field) in envelope.iter().zip(fields) {
        let prefix = format!("{field}: ");
        let value = line
            .strip_prefix(&prefix)
            .ok_or_else(|| anyhow!("terminal envelope expected `{field}: <value>`"))?;
        validate_value(field, value)?;
        values.push(value.to_string());
    }
    values
        .try_into()
        .map_err(|_| anyhow!("terminal envelope has the wrong field count"))
}

fn validate_value(field: &str, value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.chars().count() > MAX_ENVELOPE_VALUE_CHARS
        || value.chars().any(char::is_control)
        || value.contains('│')
    {
        return Err(anyhow!("terminal envelope field `{field}` is invalid"));
    }
    Ok(())
}
