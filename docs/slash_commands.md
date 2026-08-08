# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

bettercodex adds `/tmux`, which immediately moves the current live TUI into a
new detachable `c1`, `c2`, … tmux session. It remains available while an agent
turn is running and does not restart or interrupt that turn.

## Engineering review

Use `/review <target>` in the TUI to run bettercodex's active engineering
review on a specified product or system:

```text
/review the auto-updating logic
/review recovery after an interrupted WebSocket response
```

`$review` explicitly starts the same workflow in interactive and non-interactive
prompts and can target any repository scope:

```text
$review the authentication changes
$review src/tui for avoidable redraw work
```

This is intentionally not a read-only review. It first develops a deep
understanding of the target, then evaluates whether the affected system can be
simpler, faster, more resource-efficient, or easier to maintain. When
repository evidence supports a clear net improvement, the agent refactors as
deeply as needed and removes everything made obsolete. When the target already
meets those standards, it leaves the implementation unchanged and explains the
evidence for that conclusion.

Explicit review requests submitted during another turn queue as the next task
instead of steering the active implementation. The agent may also invoke the
skill proactively during implementation work when its engineering-review
criteria match the task; `$review` or `/review` is not required. A plain
read-only review request does not authorize edits.

## Quality loop

Use `/loop <task>` in the TUI when a task should get a task-specific evaluator
and repeated fresh working sessions. The default is three working sessions,
preceded by one evaluator session. Put a count immediately after the command to
override it:

```text
/loop improve startup time
/loop 5x improve startup time
```

The inline `$loop` trigger starts the same workflow in interactive or
non-interactive prompts, so it can appear anywhere in an otherwise ordinary
task. These count forms are equivalent:

```text
improve startup time $loop 5x
improve startup time $loop 5 times
improve startup time $loop 5 iterations
improve startup time 5x $loop
```

The trigger applies only to that submission. It is not a persistent mode. A
trigger without task text or an attachment is rejected, as are zero, signed,
fractional, overflowing, malformed, or conflicting counts. `/loop` is the TUI
command form; use `$loop` in a prompt argument or line-mode submission.

Submitting a loop request is confirmation to run it. bettercodex freezes the
operator messages, attachments, repository instructions, and selected skills
that define the task; builds and baselines a local evaluator; then runs the
requested number of fresh `gpt-5.6-sol` sessions at `max` reasoning effort.
Each candidate is kept only when the frozen evaluator establishes that it is
acceptable and genuinely better than the incumbent. Discarded, crashed, and
interrupted candidates are restored without cleaning pre-existing work.

While an ordinary TUI turn is active, submitting a loop request queues it as the
next task instead of steering the active turn. Inputs submitted while a loop is
active also wait until the loop ends; changing the frozen task requires
interrupting it and starting a new loop.

The TUI shows one graphite status row for evaluator and iteration progress,
while the active evaluator or worker streams its reasoning summary, commentary,
and tool activity into the ordinary transcript. Internal `SETUP` and `VERDICT`
envelopes remain private. Prompt-argument and line modes keep the final answer
on stdout and write sparse, unstyled progress lines to stderr. The final result
names the run and its evidence directory.

Every invocation requires a Git worktree and stores private, inspectable state
under `.bcodex/loops/<run-id>/`. bettercodex adds `/.bcodex/loops/` to the
repository-local Git exclude file without editing `.gitignore`. Completed runs
remain until the operator removes them. Only one loop can own a worktree at a
time; after an interrupted process, the next invocation first restores a known
durable incumbent or blocks without overwriting conflicting repository edits.
Loop-owned Cargo builds share a disposable target directory inside the private
run state. It is removed when the loop ends and during cold recovery, so those
builds do not populate the repository's `target/` directory.

As in upstream Codex, `/logout` removes stored authentication credentials and
exits the TUI. Sign in before starting a session with `bcodex login`.
