# Subagents

## Session focus

This session is for brainstorming and rubberducking the subagent architecture. We are preparing the TUI and laying out the architecture, not implementing the agents yet.

## Implementation shards

Implementation is split into three sequential shards. Each shard is completed in a fresh context window and starts from the code and spec left by the previous shard.

The approved `$deepwork` orchestrator prompt lives in `bundled-skills/deepwork/SKILL.md`. The approved `$acceptance`, `$manifest`, `$worker`, and `$reviewer` specialist definitions live under `subagents/`. These shards consume that material unchanged. They do not author, rewrite, expand, or polish the prompts.

Normal one-agent bettercodex behavior must remain unchanged whenever `$deepwork` is inactive.

### Shard 1 — Multi-session runtime and lifecycle

Build the runtime foundation without changing the visible TUI or exposing new model tools:

- introduce `SessionGroup`, stable session identity, and per-session `AgentSlot` ownership;
- move the current Main `Agent`, turn task, steering and cancellation handles, event receiver, transcript/view state, draft, status, and elapsed time into the Main slot;
- support independent child `Agent` construction with a fixed model and reasoning effort, the same working directory, and no parent conversation fork;
- route events by slot at channel ingress instead of adding session IDs to every `AgentEvent`;
- implement internal start, send, wait, cancel, retire, revive, and replace lifecycle operations;
- persist enough group and child linkage for cold resume; and
- preserve every existing single-session behavior and saved-session path.

Stop before rendering the switcher, adding the structured question card, changing the tool catalogue, activating `$deepwork`, or loading specialist prompts.

Validation focuses on existing runtime, turn, interruption, saved-session, resume, and compaction behavior plus changed observable multi-session lifecycle behavior where coverage is needed.

### Shard 2 — TUI switcher and structured interview

Connect the Shard 1 session model to the terminal UI without exposing the pipeline to Main yet:

- render the agent switcher between the activity row and composer as a compact tree, with one empty terminal row between the activity row and tree and another between the tree and composer;
- keep the composer and status line bottom-anchored;
- implement measured role-first tree columns, fixed pipeline order, overflow, live status updates, a dim baseline only for `Queued` rows, a persistent bright baseline for Main and every started stage, and one unified row-wide shimmer for every row doing active work;
- support persistent presentation rows that may be queued, live, accepted, or skipped without manufacturing an `AgentSlot` for a stage that has not started, has been skipped, or has been retired;
- implement `Ctrl+Shift+Up` and `Ctrl+Shift+Down` selection across every fixed agent row and back to the composer, `Enter` session entry when the selected row has a live session, and `Esc` cancellation;
- switch among real per-session transcripts and preserve each session's draft;
- keep Main and every fixed specialist role in stable pipeline order while the tree is active;
- add the structured question-card component with single-select, multi-select, default-selected checkbox options, free text, previews, cancellation, and answer return plumbing; and
- keep raw child activity in presentation state rather than Main's context.

Do not create fake specialist sessions, model-visible question or coordination tools, pipeline policy, or prompt integration. The tree and question surfaces remain dormant until Shard 3 supplies real `$deepwork` orchestration state and question requests.

Validation uses rendered output and terminal key behavior, including the blank activity-to-switcher row, the blank tree-to-composer row, the persistent full pipeline tree, dim `Queued` rows, bright Main, skipped-stage, and completed-stage rows, `Working` and `Cancelling` shimmer, narrow terminals, measured row alignment, neutral selection state, browsing rows without sessions, returning focus to the composer with the navigation shortcuts, no-op `Enter` on unavailable rows, session switching, draft preservation, default-selected checkbox toggling, and question-card interaction.

### Shard 3 — `$deepwork` orchestration and integration

Wire the completed runtime and TUI foundation into the real one-shot pipeline:

- activate orchestration state only when the user explicitly invokes `$deepwork`; never select or activate it proactively or implicitly;
- consume the separately approved orchestrator and specialist prompts without changing their wording;
- add the approved structured-question and specialist-coordination tool schemas and descriptions;
- make the Responses tool catalogue mode- and role-aware while leaving normal sessions unchanged;
- prevent child specialists from receiving orchestration tools;
- construct each role with its fixed model and reasoning effort;
- create or recover the root `.deepwork/` container and the run's numbered workspace without treating that runtime action as Main implementation work;
- implement the request-directed repository preflight, guided interview gate, canonical pipeline state, strict sequential stage state machine, the explicit optional-manifest skip decision, and stage handoffs;
- implement event-driven wakeups, progress updates, follow-up direction, targeted cancellation, acceptance, retirement, revival, replacement, and cleanup;
- connect question requests to the Shard 2 TUI and specialist lifecycle operations to the Shard 1 coordinator;
- populate the persistent agent tree from canonical pipeline and session state, including queued, live, skipped, and accepted rows;
- restore active and retired pipeline state on resume; and
- bound every new model-visible event and tool result.

Do not redesign the Shard 1 coordinator or Shard 2 TUI unless integration exposes a concrete defect. Do not revise the prewritten prompts during implementation.

Validation drives the complete request, tool-call, history, event, interruption, persistence, cold-resume, and TUI paths. It also proves that ordinary sessions retain the existing four ordinary tools and behavior when `$deepwork` is inactive.

## Core direction

Subagents are not a global automatic-routing feature. They belong to one user-invoked `$deepwork` pipeline.

When the user invokes `$deepwork`, Main becomes the orchestrator and receives the pipeline's specialist descriptions and coordination tools. Outside `$deepwork`, Main does not see or invoke these specialists.

The useful combination remains:

- strong specialist descriptions and isolated contexts;
- ordinary independent agent sessions with proper lifecycle control; and
- coordination outside Main's agent loop.

This is one fixed pipeline, not a general-purpose agent framework.

## Purpose

`$deepwork` is a one-shot quality pipeline. The user throws a task at it and expects a substantially cleaner and more polished outcome than giving the same task directly to a normal Sol XHigh agent.

The intended improvement is concrete when it applies to the task and survives the user's criteria approval:

- cleaner and tidier code;
- a simpler solution with a smaller footprint;
- better performance and responsiveness; and
- more efficient and optimized use of computational and I/O throughput, memory, CPU, and bandwidth.

`$deepwork` does not repeat the entire pipeline in an autonomous loop. There is no default iteration count. Main may send a specialist additional corrective turns inside its current stage, but the pipeline itself runs through the fixed chain once.

The user and orchestrator agree on task-specific success criteria during the interview. Main does not make the user invent that list from a blank page. It inspects the request and repository, proposes a calibrated set of likely criteria, explains the choices that matter, and lets the user accept, disable, edit, or add to them.

When applicable to the task, the simpler, faster, and more resource-efficient quality criteria are proposed as enabled defaults. They are defaults, not hidden policy: the user can turn off any of them before approving the interview contract.

The accepted criteria become canonical pipeline state for every later stage. After the initial interview contract is approved, the pipeline continues without another routine confirmation gate.

## Pipeline

```text
$deepwork activation
  0. Create or recover .deepwork/<run-index>/
  1. Main performs a request-directed repository preflight
  2. Main runs one guided acceptance-and-manifest interview
  3. Sol XHigh · $acceptance
  4. Sol XHigh · $manifest when materially useful; otherwise persist `Skipped`
  5. Sol XHigh · $worker
  6. Sol Max   · $reviewer
```

The pipeline is strictly sequential. Main owns the task, decides what each stage needs, reviews each result, and moves work forward.

A later stage cannot start merely because an earlier specialist returned an answer. Main must inspect and accept the current stage first. `$worker` cannot start until `$acceptance` is finished and accepted and `$manifest` is either finished and accepted or explicitly skipped with a persisted reason. `$reviewer` cannot start until `$worker` is finished and accepted for review.

Only one stage owns repository mutation at a time. Main does not advance while the preceding specialist is still working or awaiting review.

## Fixed specialist models

- `$acceptance` — `gpt-5.6-sol` at `xhigh`
- `$manifest` — `gpt-5.6-sol` at `xhigh`
- `$worker` — `gpt-5.6-sol` at `xhigh`
- `$reviewer` — `gpt-5.6-sol` at `max`

These model and reasoning-effort choices are fixed by specialist role and do not inherit Main's current `/model` selection.

## User preflight and interview gate

Invoking `$deepwork` first creates or recovers the run's numbered workspace under the root `.deepwork/` container and turns Main into an interviewer. No specialist starts until Main and the user have a shared understanding of the task.

### Request-directed repository preflight

Before asking questions, Main performs a read-only preflight over the repository, directed by the user's prompt. This is not a generic audit and it is not permission to wander through the whole project. Main inspects enough to understand what the request is actually about:

- the project type, stack, frameworks, package and build surfaces, and repository instructions;
- the likely mutable and protected scope;
- existing conventions, validation paths, and nearby implementations;
- the APIs, services, platforms, versions, or technical documentation the task may depend on; and
- which apparent questions are technical facts Main can answer itself instead of pushing them back onto the user.

It might discover that it is working on a Shopify theme, a manga website, a Rust CLI, or something else entirely. That context changes which success criteria and verification surfaces are sensible and whether `$manifest` would add material value. Aside from the coordinator creating the `.deepwork/` container and numbered run workspace, the preflight makes no repository changes.

### Guided success-criteria interview

The interview hand-holds the user through defining success. Main must not ask a vague `What are your success criteria?` question and make the user design the completion contract. It uses the request and preflight to make educated proposals about what the user probably wants, including important criteria the user implied but did not spell out. Those proposals are visible suggestions, not silent assumptions.

When applicable, Main starts with these criteria enabled by default:

- make it simpler, with a smaller footprint;
- make it faster, more responsive, and better performing; and
- make it more efficient and optimized across computational and I/O throughput, memory, CPU, and bandwidth.

A criterion that plainly does not apply should not be forced into the list just to satisfy the template. An applicable default is shown as a selected checkbox, and the user can disable it. Main adds task-specific proposed criteria when the request and repository support them, and the user can accept, disable, edit, or add criteria before approval.

The interview is iterative rather than one large questionnaire:

1. Main reads the request and completes the focused repository preflight.
2. Main drafts the objective, scope, non-goals, constraints, likely documentation needs, and a proposed success-criteria set.
3. Main presents the proposed criteria through a default-selected multi-select question instead of asking the user to start from nothing.
4. Main asks a small batch of focused questions only for unresolved choices that would materially change behavior, scope, acceptance, or documentation routing.
5. The user's selections and answers are recorded in canonical pipeline state, preserving their original wording where it matters.
6. Main asks follow-up questions only where an answer exposed another material ambiguity.
7. Main summarizes the resulting task contract and asks whether the user is satisfied that the specialist chain may begin.

This is one interview for both acceptance-contract and manifest preparation. Do not build separate acceptance and manifest interviews, side-by-side questionnaire columns, or another UI mode. Main uses the same preflight and conversation to establish what evidence `$acceptance` must require and whether any technical surfaces still need `$manifest` routing. It researches discoverable technical facts itself and asks the user only when documentation scope depends on intent, such as a pinned version, target platform, or explicitly protected integration.

The interview must be thorough without becoming exhausting. Main asks in small batches, recommends a concrete option or default when it can, does not repeat answered questions, does not ask the user for facts available in the repository or official documentation, and stops when the remaining uncertainty would not materially change the work. Final confirmation is required even when no follow-up question was necessary.

The approved task contract carries success criteria in this exact simple shape:

```text
SUCCESS CRITERIA
- first accepted criterion
- second accepted criterion
- third accepted criterion
```

The uppercase `SUCCESS CRITERIA` label and plain bullet list are stable handoff structure. Main passes that accepted block to `$acceptance` and every later specialist without relabeling it or paraphrasing away the user's meaning.

The interview gate is the pipeline's only routine confirmation phase. Once the user approves the initial task contract, they can walk away while Main supervises the complete specialist chain. After `$acceptance` is accepted and `$manifest` is accepted or explicitly skipped, Main synthesizes the completion contract, exact `SUCCESS CRITERIA` block, scope, documentation constraints, available artifact paths, persisted manifest skip reason when applicable, and remaining risks into the `$worker` handoff and starts `$worker` without presenting the contract again or asking for another general approval.

If `$acceptance` recommends adding, removing, or changing a criterion, that recommendation does not become canonical until Main shows that specific material change to the user and the user accepts it. A genuine new intent question may still pause the affected stage, but it must remain a narrow exception rather than recreating a second confirmation interview.

## Structured question TUI

bettercodex should inherit the useful shape of Claude Code's `AskUserQuestion` interaction without inheriting its broader framework.

A question card supports:

- one to four related questions at a time;
- a short header and complete question for each;
- two to six concise options with descriptions;
- single-select or multi-select answers;
- checkbox-style multi-select options that may be selected by default;
- a free-text answer when the proposed options do not fit; and
- optional preview text when a choice needs more explanation than one line can hold.

The success-criteria card uses the checkbox-style multi-select state. Applicable standard criteria and Main's recommended task-specific criteria may start selected, but the user must be able to toggle each one before submitting. A default selection communicates Main's recommendation; it is not approval until the user submits the card.

The orchestrator uses this surface for genuine user-intent decisions, not facts it can research itself. Questions should be narrow, concrete, and easy to answer. It must not turn the interview into bureaucracy or ask the user to design the solution for it.

Answers return to Main as structured data and are also preserved in their original wording in canonical pipeline state. Cancelling the question card leaves the pipeline paused; it must never silently choose a default and continue.

Specialists do not interview the user directly. They return unresolved intent to Main, and Main decides whether the answer already exists, can be researched, or must be asked through the structured question TUI.

## Orchestrator role

Main's role is to orchestrate and babysit the pipeline. It is not a passive dispatcher that invokes a specialist once and trusts whatever comes back, and it is not another implementation agent.

The orchestrator does not make repository changes. It leads, inspects, communicates, and delegates. When work needs correction, it sends the responsible specialist back in rather than fixing the work itself.

The orchestrator:

- performs a prompt-directed repository preflight before interviewing the user;
- proposes and explains a calibrated success-criteria set instead of making the user invent one;
- keeps the interview concise while still resolving every material intent question;
- gives each specialist a clear, scoped assignment and handoff;
- watches progress and remains aware of every live specialist's status;
- inspects the actual artifacts, repository changes, acceptance evidence, and handoffs instead of trusting specialist claims;
- catches omissions, mistakes, scope drift, sloppiness, and overengineering;
- proactively sends concrete follow-up direction when work is not good enough;
- keeps a specialist alive for as many turns as needed to fulfill its purpose;
- handles failures, stalls, retries, replacements, and blockers;
- decides when a stage is accepted and may advance, and whether `$manifest` is materially useful or should be explicitly skipped;
- retires specialists promptly once their job is genuinely done;
- revives or replaces a retired specialist when later user feedback reopens its stage;
- keeps the full pipeline aligned with the user's task and accepted success criteria; and
- makes it its responsibility to keep the deepwork run moving smoothly.

The orchestrator keeps the user informed with concise progress updates, stage transitions, important findings, issues, and blockers. It asks the user whenever unresolved intent would otherwise be turned into an assumption.

The orchestrator owns coordination and acceptance. Specialists own all focused stage work and repository changes. They do not decide for themselves that the pipeline is finished or that their output is acceptable.

## The shit can theory

If you whisper something into one agent's ear and that agent whispers it into another agent's ear, the final agent has not heard what the user told the initial agent. Every handoff can lose wording, priorities, exclusions, and intent. Agents then fill those gaps themselves and overengineer the hell out of everything.

This pipeline makes that risk worse because work passes through an orchestrator and several specialists. A clean architecture does not fix lossy intent by itself. Overengineering and assumption drift must be fought through strict prompting at every layer.

## Anti-assumption prompting

The orchestrator prompt must explicitly require Main to:

- inspect the user's prompt and perform a focused repository preflight before asking questions;
- interview the user before the specialist chain begins;
- use the structured question TUI rather than burying several unresolved decisions in ordinary prose;
- propose a repository- and task-calibrated success-criteria set instead of asking the user to invent one from scratch;
- enable the applicable standard quality criteria by default while making every proposed criterion individually removable;
- treat educated guesses as visible recommendations the user can reject, not as silently accepted requirements;
- cover acceptance and documentation-routing intent in one concise interview rather than duplicating questions;
- treat the user's stated intent as the source of truth;
- identify open questions before turning them into stage assignments;
- ask the user when an unresolved choice would materially change scope, behavior, architecture, acceptance, or documentation routing;
- never silently pick the more elaborate interpretation;
- preserve the user's wording when it carries intent, emphasis, exclusions, or success criteria;
- distinguish a technical unknown that can be researched from a user-intent question only the user can answer;
- pause the affected pipeline stage while a blocking intent question remains open;
- record the user's answer in canonical pipeline state; and
- carry that answer through every later handoff without paraphrasing away its meaning.

The orchestrator must not ask the user questions that the repository, accepted completion contract, or official documentation can answer. It researches technical facts and asks the user about intent.

Every specialist prompt must explicitly require the child to:

- stay inside the handed-off task and accepted scope;
- avoid features, abstractions, fallbacks, validation, and extensibility that were not requested;
- not design for hypothetical future requirements;
- surface ambiguity or missing intent to the orchestrator instead of guessing;
- state any unavoidable assumption clearly; and
- prefer the minimum complexity needed for the current task.

An unresolved intent question must travel upward to the user, not sideways through more specialists. Main may answer from already recorded user decisions, but it must not invent an answer merely to keep the pipeline moving.

The specialist role labels retain the dollar-sign presentation, but `$acceptance`, `$manifest`, `$worker`, and `$reviewer` are not skills. Their instructions are embedded directly in their specialist prompts. They do not use `SkillSelection`, runtime skill discovery, or skill injection.

`$deepwork` itself is the user-invoked skill at `bundled-skills/deepwork/SKILL.md` that activates the pipeline and reveals its orchestration context. Only the user may invoke it; Main and the runtime must never select or activate it proactively or implicitly.

## Specialist definitions

Each fixed specialist definition contains:

- a stable specialist ID;
- its model and reasoning effort;
- its dollar-sign role label;
- a concise description for the orchestrator;
- its embedded specialist prompt; and
- its allowed tools.

Main sees the concise descriptions needed to coordinate the pipeline. The full specialist prompts are given only to their children and do not occupy Main's context.

Children never receive the orchestration tools. Delegation depth is one.

## Independent sessions

Each specialist runs as a fresh independent `Agent`, not a fork of Main's conversation.

A child receives:

- its fixed model and reasoning effort;
- its embedded specialist prompt;
- the same working directory and repository instructions;
- its allowed ordinary tools; and
- the scoped task, handoff, and relevant context Main explicitly sends.

It does not inherit Main's complete conversation history. Its transcript, context window, composer draft, active turn, cancellation handle, status, and elapsed time are independent.

These are real sessions. While active, the user can navigate into them through the TUI and interact with the same session Main is coordinating.

## Orchestrator communication

Main coordinates specialists through the event-driven start, optional-manifest skip, send, wait, cancel, and retire responsibilities defined below.

A completed child turn does not immediately kill the specialist. The child remains available and visible while Main reviews its work.

- If Main thinks the specialist missed something, Main sends concrete feedback to the same active session.
- The follow-up continues the existing specialist context rather than starting over.
- Main may repeat this second or third time, or however many times are needed.
- When Main is satisfied that the specialist fulfilled its purpose, Main retires it and advances the pipeline.

Progress and lifecycle events drive the coordinator and TUI, but raw activity is not automatically injected into Main's model context. Only bounded meaningful events wake Main.

## Specialist lifecycle proposal

```text
Absent
  → Working
      → Cancelling                 when Main or the user interrupts the turn
          → Paused                 when interruption settles
              → Working            when Main sends a continuation
              → Replaced           when the stage needs a fresh redo
      → Awaiting orchestrator review
          → Working                when Main sends follow-up feedback
          → Retired                when Main accepts the stage
  → Revived or Replaced            when later user feedback reopens the stage
```

Retiring is not destructive deletion:

- remove the specialist from the live session group while keeping its fixed pipeline row visible as `Accepted` for the remainder of the active run;
- drop its in-memory runtime, event stream, and cancellation state;
- keep its saved rollout and stage handoff;
- record that Main accepted and retired it; and
- allow it to be recovered if later feedback requires more work.

If the user later says something was forgotten, the acceptance contract is bad, or the result is too overengineered, Main chooses between:

- **Revive:** restore the retired session when the feedback is a direct amendment and its prior reasoning remains useful.
- **Replace:** start a fresh session when the stage needs an independent redo, stale assumptions caused the problem, or the old context would bias the repair.

A revived specialist's existing pipeline row changes from `Accepted` to its live lifecycle status and becomes enterable again. A replacement keeps the same stable role row, gets a new session ID, and receives the original stage brief, accepted handoff, current repository state, and new user feedback.

This lets agents be killed off operationally without throwing away the continuity needed to apply later feedback.

## Progress without polling

Main must not check on specialists every 30 seconds or on any other timer. Periodic model-driven checkups would bloat Main's context, spam the TUI, waste tokens, and still provide mostly meaningless `still working` observations.

Progress supervision is event-driven:

- the coordinator already receives each specialist's live agent and tool events;
- those events update the specialist's TUI row and internal status without entering Main's context;
- specialists send Main a message only for a meaningful milestone, blocker, question, failure, or completed turn;
- the coordinator automatically wakes Main for those events rather than requiring Main to poll;
- repeated low-value activity updates are coalesced into the latest state; and
- elapsed time and live activity are presentation state, not model messages.

The TUI can remain visually current even while Main is waiting. Rendering `Working`, elapsed time, the latest activity, or a changed status does not require another Main inference turn.

## Event-driven supervision

Use a blocking wait operation that sleeps until a meaningful specialist event exists. It is not timer polling and produces no model-visible result while nothing has changed.

The event-driven orchestration flow is:

1. Main tells the user which stage is starting and what the specialist was asked to do.
2. Main starts or continues the specialist.
3. The TUI follows the child's ordinary events directly.
4. Main waits on the coordinator for the next meaningful event.
5. A blocker, question, failure, interruption, completed turn, or materially changed phase wakes Main with one bounded structured result.
6. Main updates the user only when the event is useful to them.
7. Main answers a technical coordination issue, asks the user about unresolved intent, sends corrective direction, or accepts and retires the specialist.
8. Main waits again when more work remains.

The required coordination operations are conceptually:

```text
start_specialist(specialist, task)
skip_manifest(reason)
send_specialist(session, message)
wait_specialist(session)
cancel_specialist(session)
retire_specialist(session)
```

These are responsibilities, not yet a commitment to separate function tools. The eventual interface should remain as narrow as possible.

### Specialist cancellation

Cancelling Main's blocking `wait` cancels only that wait call. It never implies authority to stop the child. Specialist cancellation is a separate, session-targeted coordinator operation so ownership cannot be confused across independent turns.

Cancellation has these semantics:

- Main calls `cancel_specialist(session)` for a live specialist whose turn is still running.
- The coordinator persists `Cancelling` before signalling that slot's existing turn cancellation handle.
- Repeating cancellation while the slot is `Cancelling` or `Paused` is harmless.
- The TUI shows `Cancelling` while the turn unwinds, then `Paused`; the row remains visible because the session remains live.
- A user cancels the same underlying turn by entering the specialist session and pressing `Esc`. This does not require or cancel a Main inference turn.
- The specialist's interrupted completion wakes any current or later Main wait with one bounded `interrupted` event and persisted `Paused` status.
- Cancellation pauses only the current stage. It does not accept the stage, advance the pipeline, retire the specialist, or abort the whole deepwork run.
- Main may continue the paused session with `send_specialist`, or replace it when the interrupted context or partial work would bias a redo. A paused session cannot be accepted and retired until a later turn completes and reaches `Awaiting orchestrator review`.
- If terminal completion wins a race with cancellation, the completed result remains `Awaiting orchestrator review`; cancellation does not discard an already completed turn.
- Cold resume converts a persisted `Cancelling` slot to `Paused` and emits the interruption event once the session group is recovered.

Cancellation is not rollback. Ordinary turn and tool cancellation must terminate owned processes and clean task-owned temporary staging through the existing interruption paths. Already committed repository mutations and retained `.deepwork/<run-index>/` artifacts remain in place because their effects may be partial or valid. Main must inspect the workspace before continuing the same session or replacing it, and later specialists must not assume an interrupted operation had no effect.

A specialist event delivered to Main contains only what coordination needs:

- specialist and stage identity;
- event kind;
- one concise message or question;
- current status; and
- final result only when the turn completed.

Blockers, failures, interruptions, and completed turns always wake Main. A phase milestone wakes Main only when the stage has materially changed; timer-driven or `still working` milestones are forbidden. Repeated events are coalesced before delivery.

Raw tool output, throbber ticks, elapsed-time changes, and repeated status snapshots never enter Main's context.

## User progress updates

Main communicates progress at meaningful boundaries rather than narrating activity continuously:

- pipeline and stage start;
- an important milestone that changes what the user should know;
- a blocker or open intent question;
- a failed approach and the corrective action Main is taking;
- stage review, rejection, follow-up, or acceptance; and
- pipeline completion.

A progress update says what changed, why it matters, and what happens next. `Still working` is not an update.

## Stage handoffs

Each stage receives an explicit handoff rather than Main's whole transcript. A handoff contains only what that specialist needs, but it must not compress away user intent. It includes:

- the user's task, preserving exact wording where it matters;
- the current criteria under the literal uppercase `SUCCESS CRITERIA` label as a plain bullet list;
- accepted decisions and the user's answers to earlier questions;
- explicit non-goals and things that were not requested;
- relevant constraints and repository scope;
- outputs accepted from earlier stages and any persisted optional-stage skip reason;
- current diff or implementation state;
- any open question the specialist must return rather than answer itself; and
- the exact question or deliverable for this stage.

Before advancing, Main checks the returned work against the canonical user intent rather than only against the previous specialist's handoff. This prevents each stage from treating the last stage's interpretation as the new source of truth.

Main owns the canonical pipeline state and decides which stage output is accepted. Specialist claims are not acceptance by themselves.

## `$acceptance`

`$acceptance` receives the approved task contract, including the literal `SUCCESS CRITERIA` block, and turns it into a durable, evidence-backed completion contract. It sets the finish line before implementation: what must be true, what evidence can establish it, what must remain intact, what actions and scope are permitted, and how Main distinguishes complete, partial, blocked, and uncertain outcomes.

The role is task-agnostic. It chooses the strongest proportionate verification surface for each criterion, which may be automated checks, data reconciliation, controlled measurement, artifact or source inspection, sampling, visual or behavioral review, or a narrow evidence-based rubric. It must not manufacture automation, numerical thresholds, or pass/fail certainty when clear inspection or an honest partial, blocked, or uncertain finding is more appropriate. Verification is repeatable where practical and clear and inspectable otherwise.

`$acceptance` preserves every accepted criterion verbatim and visibly maps it to required evidence, a verification surface, a procedure, and a completion condition. It records relevant constraints, boundaries, baselines, limitations, and known blind spots in `.deepwork/<run-index>/ACCEPTANCE.md`, with only necessary supporting artifacts under `.deepwork/<run-index>/acceptance/`. Supporting scripts or checks are created only when they materially improve confidence.

`$acceptance` does not invent the user's goal or silently rewrite the accepted criteria. If a criterion is vague, contradictory, unverifiable, or missing an apparently essential decision, it reports the exact gap to Main. It may recommend a concrete clarification or additional criterion, but Main must take any user-intent change back to the user before it becomes canonical. The orchestrator reviews the actual completion contract and supporting evidence before the pipeline advances; later specialists must not weaken or silently rewrite it.

## `$manifest`

`$manifest` is a situational specialist stage. After `$acceptance`, Main starts it only when current official documentation routing would materially help the worker or reviewer. If the repository, preflight, accepted completion contract, and existing infrastructure already provide sufficient technical routing, Main explicitly skips the stage with a concrete persisted reason. Skipping creates no `AgentSlot`, child session, or manifest artifact; the fixed tree row becomes bright, static `Skipped`, and `$worker` may start immediately. If documentation routing becomes necessary before a worker session starts, starting `$manifest` removes the skip decision, reopens the manifest stage, and creates its first session normally.

When started, `$manifest` researches the official technical documentation the task actually needs and writes `.deepwork/<run-index>/MANIFEST.md` as the routing handoff for the worker. The first run writes `.deepwork/0/MANIFEST.md`, the next writes `.deepwork/1/MANIFEST.md`, and so on. Its scope comes from the user's prompt, Main's repository preflight, the accepted task and completion contracts, and the evidence and technical surfaces they identify. It does not perform a second user interview.

The specialist prompt embeds the complete approved `$manifest` skill instructions directly. The child does not invoke `SkillSelection`, discover a runtime skill, or depend on the external skill file. Its embedded behavior includes:

- identify the official documentation domain and current stable version unless the user pinned another version;
- map task-relevant core surfaces first, then specialized surfaces, then the cross-cutting authentication, rate-limit, versioning, pagination, and error pages that actually exist;
- verify every URL live during the stage and never include a guessed or remembered URL;
- write a routing map rather than copied documentation, a tutorial, schemas, or implementation code;
- give every entry a one-sentence `Use when:` trigger and at least one correctly labeled bare URL; and
- finish with `## Agent Routing Notes` in the approved house manifest format.

When started, `$manifest` writes only its manifest artifact. It must not alter product files. If it discovers a documentation-scope ambiguity that depends on user intent, it returns that ambiguity to Main instead of guessing. Main must not start it merely to fill the fixed pipeline row.

## `$worker`

`$worker` performs the implementation work against the accepted task, completion contract, constraints, and the documentation handoff when one was produced.

It uses `gpt-5.6-sol` at `xhigh`. Its exact worker prompt remains undecided.

## `$reviewer`

`$reviewer` takes the worker agent's work and surgically reviews it under a microscope. It refactors against the accepted `SUCCESS CRITERIA` block and completion contract rather than an unconditional built-in checklist.

The following standard criteria remain review targets only when they were applicable and the user kept them enabled during the interview:

- make it simpler, with a smaller footprint;
- make it faster, more responsive, and better performing; and
- make it more efficient and optimized across computational and I/O throughput, memory, CPU, and bandwidth.

If the user disabled one of these criteria, `$reviewer` must not silently restore it as an optimization goal.

After refactoring the worker's work, it takes a step back and performs a second pass over its own changes:

- Does the implemented solution have features beyond what was asked for? If yes, remove them.
- Was error handling, fallbacks, or validation added for scenarios that cannot happen? If yes, remove them.

The second-pass principle is:

> Don't design for hypothetical future requirements. The right amount of complexity is the minimum needed for the current task.

`$reviewer` does not broaden the task, change the accepted success criteria, or weaken the completion contract. Its job is to improve the worker's implementation, not redesign the assignment.

## Session coordinator

Coordination belongs outside Main's `Agent` loop. During `$deepwork`, the interactive runtime owns a group shaped roughly like:

```text
SessionGroup
  active_session
  Main AgentSlot
  active pipeline specialist AgentSlot(s)
  retired specialist session references
```

Each live `AgentSlot` owns or tracks its own:

- `Agent` and saved session identity;
- pipeline role and stage attempt;
- turn task, steering, and cancellation handle;
- event stream;
- transcript and session-specific view state;
- composer draft; and
- status and elapsed time.

The coordinator routes tagged events and completions to the correct slot. Do not add a session ID to every `AgentEvent` merely to recover ownership after events have entered one shared stream.

Today `Runtime` owns one `Agent`, one turn, and one `View`. Preparing the architecture means separating group-global pipeline navigation from per-session agent and view state.

## Pipeline workspace and artifacts

Activating `$deepwork` creates or recovers a hidden `.deepwork/` directory at the repository root. This is the container for numbered run workspaces, not a regular file and not one shared workspace for every run.

A new run receives the next monotonically increasing non-negative integer directory:

```text
.deepwork/
  0/
  1/
  2/
  3/
```

The first run uses `.deepwork/0/`. Each later new run takes one more than the highest existing numeric run directory, so old runs build up without being overwritten. Non-numeric entries do not participate in allocation. Resuming an existing run reuses its persisted run index and directory instead of allocating another one.

Every support file generated by one pipeline run belongs under that run's numbered directory: acceptance-contract artifacts, the optional documentation manifest, stage reports, and any other retained coordination output. The manifest path is `.deepwork/<run-index>/MANIFEST.md`. Temporary diagnostics still follow normal cleanup rules.

This does not mean implementation files requested by the user are redirected into the numbered workspace. `$worker` and `$reviewer` edit or create the actual product files in their correct repository locations. `.deepwork/<run-index>/` contains the pipeline's own artifacts, not the requested product output.

The coordinator reserves the run index and creates the container and run directory as pipeline plumbing before Main's read-only preflight. Main itself remains a non-implementation orchestrator. Specialists may write only the task files their stage owns and their declared artifacts inside the current numbered run workspace.

## Persistence

Each specialist remains an ordinary saved rollout. Persist enough pipeline linkage to recover:

- the Main session and deepwork run ID;
- the root `.deepwork/` container, assigned numeric run index, numbered run workspace, and retained artifact paths;
- the stable specialist role and stage attempt;
- the child session ID;
- whether the child is active, working, cancelling, paused, awaiting review, retired, revived, or replaced;
- whether `$manifest` was started and accepted or skipped without a session, including the persisted skip reason;
- the accepted stage handoff; and
- the embedded prompt revision if compatibility requires it.

Cold resume accepts the legacy persisted `evals` role and stage names, legacy child linkage, and an existing run's `EVALUATOR.md` artifact as the predecessor of `$acceptance`. Recovered state is exposed and reserialized with the new `acceptance` identity; newly started stages use `ACCEPTANCE.md` and `acceptance/`.

Retired specialists are absent from the live session group and cannot be entered, but their stable role rows remain visible as bright, selectable `Accepted` presentation rows while the `$deepwork` tree is active. Pressing `Enter` on such a row is a no-op unless the specialist is later revived or replaced. The saved session remains recoverable. Stable session IDs, not visible row labels, are runtime identity.

The pipeline ends after Main accepts `$reviewer`, inspects the final repository state and acceptance evidence, reports every known limitation or unresolved risk, and retires the reviewer. No specialist session remains live after completion. The numbered run workspace, canonical contract, accepted artifact paths, retired specialist references, and saved rollouts remain persisted and recoverable for resume or later user feedback.

## TUI vocabulary

The lower TUI cluster is the **bottom pane**. Its existing pieces are:

- the **activity row** — the `• Working (...)` line and throbber;
- the **composer** — the chatbox where the user types; and
- the **status line** — the model, repository, branch, and context row below the composer.

The new area between the activity row and composer remains the **agent switcher** in this spec. While `$deepwork` is active, it renders as a persistent **agent tree** rooted at `$deepwork`. When the tree is visible and terminal height permits, one completely empty terminal row separates the activity row from the tree and another completely empty terminal row separates the tree from the composer.

## Image 1

The screenshot is a cropped terminal view of bettercodex's lower TUI. The transcript ends with `Read src/agent.rs`. Below it is the activity row, showing `• Working (14m 15s • esc to interrupt)`. A thick red hand-drawn horizontal line marks the currently blank strip directly below the activity row and directly above the composer. The composer is a dark rectangular input area with a `›` prompt and cursor. The bottom status line shows `gpt-5.6-sol max │ bettercodex / main │ 44% of 258K`.

The red-marked strip is where the agent tree appears.

## Tree visibility and order

- The tree appears only while a `$deepwork` run is active.
- A non-selectable `$deepwork` root appears first.
- Main and every fixed specialist role remain visible beneath it in stable sequential order: Main, `$acceptance`, `$manifest`, `$worker`, `$reviewer`.
- Rows never reorder or disappear while the run remains active, including the row for the session currently being viewed.
- A stage that has not started displays `Queued`; this is presentation state and does not create an `AgentSlot` or child session. Its row can still be highlighted for browsing.
- A live stage displays its real lifecycle status, with elapsed time where applicable.
- When Main explicitly skips `$manifest`, its row displays `Skipped`; this is persisted presentation state, creates no `AgentSlot` or child session, remains highlightable, and is not enterable.
- After Main accepts and retires a stage, its row remains visible as `Accepted`; this does not keep its runtime alive or make it enterable, but the row remains highlightable.
- Reviving or replacing a stage reuses its stable role row and restores a live, enterable status.
- Displayed states must respect strict pipeline sequencing. A later role cannot be `Working` while an earlier role is still `Awaiting review`.

The composer and status line remain anchored at the bottom. The tree expands upward, moving the activity row higher. One dedicated blank row separates the activity row from the tree, and a second dedicated blank row separates the final visible tree row from the composer. Both rows are completely empty: they contain no selector, connector, status text, or styling. In an extremely short terminal, these spacing rows yield before tree navigation or the minimum composer height. All fixed rows remain in presentation state even when the viewport must scroll around the current selection.

## Tree rows

Use one compact tree row per agent:

```text
  $deepwork
  ├── Main        | sol xhigh · Waiting
  ├── $acceptance | sol xhigh · Accepted
  ├── $manifest   | sol xhigh · Skipped
  ├── $worker     | sol xhigh · Working (2m 14s)
  └── $reviewer   | sol max   · Queued
```

With this persistent tree, accepting and retiring a finished specialist removes its live runtime without removing its stable row. The `Accepted` row stays visible and bright, while the saved session reference remains available for a later revive or replacement.

A fixed selector gutter precedes the tree connector. After the connector, the role comes first, followed by ` | `, the lowercase model and effort profile, ` · `, and the written status. Role and model fields are measured columns, not independently padded strings, so the vertical separator and status dot remain aligned across all rows.

Main keeps its `Main` label rather than inventing a `$main` role and displays its actual model and effort profile. Specialist rows display their fixed role profiles even while `Queued`, `Skipped`, or `Accepted`. Every fixed role row, including the current session and rows without a live session, participates in keyboard highlighting. Only a row backed by a live session can actually switch sessions. Each live session retains its own unsent composer draft when the user switches away.

## Switcher interaction

The composer keeps normal keyboard focus until the user presses `Ctrl+Shift+Up` or `Ctrl+Shift+Down`. From the composer, `Ctrl+Shift+Down` highlights Main and `Ctrl+Shift+Up` highlights `$reviewer`.

While a row is selected:

- `Ctrl+Shift+Up` and `Ctrl+Shift+Down` move through every fixed agent row in visual order, including the current session, `Queued` rows, `Skipped` rows, and `Accepted` rows;
- moving above Main or below `$reviewer` returns focus to the composer, so repeated navigation cycles through the tree and composer rather than trapping focus in the tree;
- `Enter` enters the selected row's live session when one exists;
- `Enter` on a `Queued`, `Skipped`, `Accepted`, or otherwise unavailable row does nothing and leaves the selection in place; and
- `Esc` cancels selection and returns focus to the composer.

Entering, moving, or cancelling selection must not alter the current composer draft.

## Color and motion direction

The tree uses one restrained gunmetal-gray family with two baselines. `Queued` rows use the dim baseline because their stage is unresolved. Main, every stage that has started, and an explicitly `Skipped` manifest row use the brighter baseline and never fall back to dim when they stop working, pause, await review, or become accepted.

- Main always uses the brighter baseline, including while its written status is `Waiting`.
- Once a specialist has started, its `Waiting`, `Paused`, `Awaiting review`, and `Accepted` presentations remain bright and static. A `Skipped` manifest row is also bright and static even though no specialist started, because it records a resolved pipeline decision.
- A row shimmers whenever its displayed status represents active work: `Working` or `Cancelling`. Resuming a paused specialist returns it to `Working`, so its shimmer returns.
- Strict sequential orchestration should ordinarily leave only one row doing active work at a time. Main shimmers whenever Main is `Working`, just like a specialist.
- The shimmer traverses left to right as one continuous effect across all rendered text cells from the tree connector through the role, separator, model profile, status, and elapsed time.
- The shimmer uses one row-level display-cell offset across the fully assembled row. It must not restart independently for the connector or for each styled text span or measured column.
- The shimmer affects rendered text, not the empty remainder of the terminal row.
- Switcher selection is independent from activity. It uses the leading `›` in the selector gutter and a neutral dark background without moving, restarting, or creating a shimmer.
- Written status labels remain present so brightness and motion are never the only status signals.
- The existing restrained shimmer on the activity row's leading `•` and `Working` label remains unchanged.

## Open decisions

- What are the exact deliverables and acceptance boundary for each stage?
- Does Main explicitly call a retire operation, or can acceptance of a handoff retire the specialist atomically?
- Should direct amendment feedback revive the latest retired session by default, unless Main gives a reason to replace it?
- What happens if the user steers a child in a way that conflicts with Main's current handoff?
- How large may a specialist's returned final result be?
