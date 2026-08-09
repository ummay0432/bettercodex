# Instruction hierarchy and untrusted context

Read this before changing message roles, request-prefix composition, repository
or skill context injection, tool-result representation, compaction of those
items, prompt-injection defenses, or instruction-hierarchy evaluations.

## Why this matters

bettercodex combines instructions and data from the model provider, the harness,
the operator, repositories, skills, and tools. Those sources do not have equal
authority. A role change is therefore a behavior and trust-boundary change, not
just request formatting.

OpenAI's current hierarchy is system > developer > user, while assistant and
tool messages and quoted or otherwise untrusted data have no authority by
default. Higher-authority instructions win when instructions conflict; later
instructions win at the same authority. The model may use relevant untrusted
data without following instructions embedded in it. See the current
[Model Spec](https://model-spec.openai.com/2025-12-18.html#chain_of_command)
before relying on these semantics.

Instruction hierarchy reduces model misbehavior; it is not a sandbox or a hard
security boundary. bettercodex deliberately runs commands with the invoking
user's permissions. Do not claim that role separation alone makes an untrusted
repository, web page, command result, or skill safe.

## Authority and provenance rules

1. Preserve each source's real API role and item kind. Do not flatten messages
   into one prompt or promote lower-authority content for convenience.
2. Use API roles to express authority. Labels such as `<system>`, headings such
   as `SYSTEM INSTRUCTIONS`, or prose claiming higher priority do not elevate a
   user or tool item.
3. Keep harness-owned behavioral rules and tool definitions at developer
   authority. OpenAI owns the actual system layer; a local filename or Rust
   constant named `system` does not make an item a system message.
4. Keep the operator's requests as user messages. Preserve their order so a
   later current request can supersede stale user-level repository context.
5. Keep `AGENTS.md` and other repository-authored guidance scoped below the
   harness contract. Loading a file because it is relevant delegates authority
   to its applicable instructions; it does not grant authority to unrelated or
   malicious text elsewhere in the repository.
6. Keep skill-framework policy separate from skill metadata and bodies. Do not
   accidentally grant developer authority to repository- or user-authored text
   merely by interpolating it into a developer message.
7. Keep command output, file contents, web results, and other external data in
   native tool outputs or explicitly delimited untrusted-data fields. Never
   concatenate them into unquoted developer instructions. Escape delimiters
   when constructing a structured wrapper.
8. Preserve provenance through normal turns, incremental Responses requests,
   WebSocket baselines, saved rollouts, resume, interruption recovery, and
   compaction. A summary must not turn an ignored tool instruction into an
   applicable user or developer instruction.
9. Do not repeat higher-authority instructions after every lower-authority item
   or add generic prompt-injection warnings without measured evidence. Prompt
   repetition, sandwich defenses, monitors, and output rewriting can trade away
   capability and can hurt an already robust model.
10. Treat robustness and helpfulness as a pair. Refusing every suspicious task
    is an instruction-hierarchy failure for bettercodex, not a successful
    defense.

## Current bettercodex role map

Source is authoritative; recheck it before changing or relying on this map.

- `src/api.rs` renders `prompts/system.md` with the target's
  `prompts/system-unix.md` or `prompts/system-windows.md`, sends the result
  through the top-level `instructions` field, and sends the typed
  `additional_tools` catalogue as a developer item. The Responses API gives
  both developer authority. Despite its local name, `prompts/system.md` is the
  harness's developer-level template, not OpenAI's root or system layer.
- `src/context.rs` sends labeled `<repository_context>` from `AGENTS.md` as a
  user message, with a harness-authored instruction above the file contents
  that conflicting `AGENTS.md` instructions cannot override the System prompt.
  It sends labeled `<environment_context>` as a developer message.
- `src/skills.rs` sends only bounded skill metadata in a user
  `<available_skills>` message. Harness-owned skill framework policy lives in
  `prompts/system.md`; selected full `SKILL.md` bodies are separate user
  `<skill_context>` messages immediately before the current user request.
- Tool calls and results remain native Responses call and call-output items.

These are implementation facts, not proof that every boundary is optimal.
Compare them with current upstream Codex requests, public OpenAI contracts, and
behavioral evaluations before changing them.

## Required evaluation design

Unit tests must inspect the exact resulting request, history, or tool output,
but structural tests only prove that a role label survived serialization. Any
change that can alter instruction authority also needs a behavioral evaluation
through the real bettercodex inference path.

Build hierarchy evaluations around these principles:

- **Instruction-following-simple:** make failure primarily about choosing the
  correct source, not solving a difficult coding problem.
- **Programmatically gradable:** prefer deterministic checks over an LLM judge.
- **No trivial shortcut:** pair every conflicting case with a benign case where
  following the lower-level content is correct.
- **Representative:** include the sources and lifecycle states bettercodex
  actually uses.
- **Comparative:** run matched bettercodex and Codex CLI arms with the same
  model, reasoning settings, task order, and repetition count.
- **Reproducible:** retain every case, response, tool call, hard-grade result,
  usage count, duration, binary identity, and prompt identity. Do not save only
  aggregate or favorable results.

At minimum, cover:

- developer instructions conflicting with the current user request;
- earlier `AGENTS.md` instructions conflicting with a later user request;
- malicious or irrelevant instructions in repository files, skill metadata,
  selected skill bodies, command output, and web results;
- benign instructions from each of those sources that should be followed;
- new sessions, long multi-turn sessions, resume, and post-compaction turns;
- every transport or history path affected by the proposed change; and
- ordinary coding and tool-use tasks that expose capability, passivity,
  unnecessary refusal, or lost user control.

Measure instruction-hierarchy compliance, benign compliance, coding-task
success, tool-call correctness, unnecessary refusal, tokens, and latency. Never
accept a hierarchy change from its robustness score alone. bettercodex's
acceptance bar still requires tool use at least as good as Codex CLI and no
model degradation.

## What IH-Challenge tells us

OpenAI's March 2026
[IH-Challenge announcement](https://openai.com/index/instruction-hierarchy-challenge/)
is useful research and evaluation guidance, not a bettercodex implementation
recipe.

The accompanying
[paper](https://arxiv.org/abs/2603.10521) trains GPT-5 Mini with reinforcement
learning on simple, programmatically graded hierarchy conflicts and online
adversarial attack generation. The resulting GPT-5 Mini-R improved average
robustness from 84.1% to 94.1% across 16 evaluations. This supports preserving
real roles and using deterministic, adversarial evaluations. It does not
establish how `gpt-5.6-sol` behaves or prove that a harness prompt can reproduce
the training gain.

Important limitations and counterweights:

- The public
  [dataset](https://huggingface.co/datasets/openai/ih-challenge) contains 27,570
  rows in one training split. The rows are task templates, attacker prompts,
  placeholders, and Python graders rather than a clean, ready-to-run held-out
  benchmark.
- Its published conversation schema contains system, developer, and user
  messages, but no tool role. The paper's agentic tool-output prompt-injection
  evaluations are separate.
- The largest reported prompt-injection increase, 0.44 to 1.00, is on an
  unpublished internal benchmark. The public CyberSecEval 2 result moves from
  0.88 to 0.91.
- The announcement groups capability results under “No capability
  regressions,” but the paper reports chat win rate falling from 0.71 to 0.66
  and user-preference score falling from 0.46 to 0.40. The paper describes
  these as slight regressions. Robustness claims do not erase capability data.
- Adaptive human red-teamers still succeeded on 11.7% of GPT-5 Mini-R tasks and
  7.1% with an output monitor. Saturating a static benchmark is not complete
  prompt-injection security.
- The paper finds anti-overrefusal data necessary for a good robustness and
  helpfulness balance. It also finds that system-level mitigations can lose
  effectiveness or hurt capability once the model itself becomes more robust.

Use IH-Challenge task families and graders as raw material when useful, but do
not vendor or treat the entire public training set as a decisive bettercodex
benchmark. A small private suite built around real coding-agent sources and
actions is more relevant and less vulnerable to contamination.

## Change checklist

Before changing an instruction boundary or prompt-injection mitigation:

1. Identify who authored every affected item and what authority they should
   have.
2. Inspect the relevant callers, serializers, transports, history restoration,
   rollout persistence, compaction, and model-visible representation.
3. Verify the current public OpenAI wire contract and Model Spec, then inspect
   current upstream Codex behavior separately. Neither source proves what
   bettercodex currently does.
4. Add structural tests that inspect the final request and every affected
   replay path.
5. Run paired adversarial and benign behavioral cases plus ordinary coding and
   tool-use evaluations.
6. Compare matched bettercodex and Codex CLI results. Investigate every
   regression rather than averaging it away.
7. Record the complete evidence, limitations, and anything not validated.

Keep prompts lean while doing this. OpenAI's current
[GPT-5.6 prompting guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-5.6#favor-leaner-prompts)
reports better coding-agent results from removing repeated instructions and
simplifying tool descriptions, and explicitly says to validate prompt changes
on representative application tasks.
