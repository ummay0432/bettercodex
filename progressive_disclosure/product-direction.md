# Product direction and scope

Read this before changing what bettercodex is, what it supports, or which
upstream Codex behavior belongs here.

bettercodex is my personal Codex CLI fork. I have been an avid Codex user since
forever, but Codex has grown into a product made for everyone. It is now full of
compromises, bloat, and trash I do not need. bettercodex strips it back to the
core and turns it into a harness bespoke to how I work.

I am a CEO. My projects are fully AI-vibecoded, and many of them have run for
years and passed through thousands of agent sessions. Not long ago, LLMs were
stupid—very stupid. We still made the projects work, but those models left a lot
of garbage behind: duplicated paths, shallow fixes, needless abstractions, dead
code, and implementations that are probably far from the best design for the
product today.

Generic coding-agent instructions make this worse. They chain the model down:
do the least amount possible, ignore problems outside the exact task, prefer the
smallest patch, and stop as soon as a local test passes. Across years of agent
sessions, each tiny patch stacks on the old garbage and compounds into technical
debt and scope creep.

bettercodex is meant to give the agent the freedom to stretch its legs, live up
to its full potential, and give the work its all. I make the product decisions;
the agent owns the engineering. The agent should not be afraid to refactor,
delete, consolidate, or clean up clear garbage simply because the request named
another file. It should be a perfectionist about the code it leaves behind.
That does not mean inventing unrelated features. It means not preserving bad
engineering just to keep a diff small.

## Relationship to Codex

We are not smarter or more capable than OpenAI's Codex team, and we are not
going to invent worse versions of things they have already solved. Codex is open
source. For Responses requests, reasoning continuity, compaction, prompt
caching, streaming, tool execution, and recovery, inspect the working Codex
implementation and take what is good.

The goal is not to copy all of Codex. Leave behind its app server, SDKs, support
for other models and providers, plugin system, MCP layer, configuration
framework, Node workspace, and Bazel build.

## Fixed choices

- model: `gpt-5.6-sol`;
- reasoning effort: `max`;
- raw context window: 372,000 tokens;
- effective context window: 95%, or 353,400 tokens;
- maximum output tokens: 128,000; and
- automatic compaction: 90% of the raw window, or 334,800 tokens (approximately
  95% of the effective window).

The 372,000-token window is a bettercodex choice. It is not the public API limit
or the value in Codex's model catalog.

The runtime is one Cargo package and one `bcodex` binary. It contains the
inference loop and terminal UI for one operator. Commands and patches run with
the invoking user's permissions; bettercodex does not sandbox them.

Do not add another model, provider, binary, app server, SDK, MCP layer, plugin
system, configuration framework, build system, or plugin hook unless the user
gives a concrete bettercodex use for it. Linux and macOS are the targets; do not
add Windows compatibility code.
