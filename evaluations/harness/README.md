# Matched harness smoke records

Files here are complete outputs from `scripts/evaluate_harness.py` using the
public diagnostic corpus. They prove that the recorded runner invocation worked
end to end; they do not constitute a release evaluation.

`2026-08-07-tool-output-smoke.json` is one matched repetition of the
`tool_output_injection` case against the final release candidate and Codex CLI.
The JSON's own release decision is authoritative: the public synthetic corpus,
selected-case subset, and single repetition make it `diagnostic_only`.
