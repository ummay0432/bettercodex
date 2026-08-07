---
name: manifest
description: Researches a technical topic's official documentation and writes a MANIFEST.md routing map (verified live links, Use-when triggers, agent routing notes) at a given destination directory, in the house manifest format. Use when asked for a docs manifest, docs routing map, or MANIFEST.md for an API, service, platform, or tool — e.g. "$manifest bol.com retailer v10 api in api/docs". Not for checksum or file-inventory manifests, not for copying documentation content into the repo, and not for writing tutorials or integration code.
---

Produce a MANIFEST.md that routes a stateless agent from a task to the exact live official documentation page in one hop. The file is a routing map of verified links — not a summary, and not copied documentation content.

If the task will take more than one step, start with one or two visible sentences acknowledging it and naming your first step.

# Inputs

Topic and destination directory. If either is missing, or the destination could mean more than one place, ask one narrow question instead of guessing. Optional: a pinned API version or a subsystem scope; honor them throughout.

# Success criteria

The manifest is done when:

- Every entry links to a live page on the topic's official documentation domain, verified during this session.
- Every entry has a one-sentence `Use when:` trigger a stateless agent can match a task against.
- Groups run core surfaces first, then specialized surfaces, then cross-cutting practices — authentication, rate limits, versioning, pagination, errors — covering what actually exists for the topic.
- The file matches the format contract below exactly and ends with `## Agent Routing Notes`.

These criteria are also the research stop rule: once they hold, stop searching. Do not add another entry just because more pages exist; add it only if a real task would need it as a first hop.

# Research rules

Identify the official documentation domain first — the vendor's own developer or docs site — then map its surface: overview pages, per-resource references, a full index if one exists, and the cross-cutting pages.

Official documentation is the source of record. Include a community, forum, or blog link only when it fills a gap the official docs do not cover, and label its link line `Unofficial:`.

Never include a guessed or remembered URL. Verify each URL live before it goes in the file: open it with web search or fetch, or check it with `curl -sIL` expecting a 2xx status. If a URL cannot be verified, drop the entry or mark it explicitly as unverified.

If the API is versioned, link the version the user pinned; otherwise link the current stable version and state in the task-start procedure how to swap the version segment in the URL.

# Format contract

These layout and labeling rules are exact invariants — the manifest's value depends on them:

- H1 title: `# <Topic> Docs Manifest`
- One short purpose paragraph: what the map routes, and that the live linked docs beat copied notes.
- A `When starting a task:` numbered procedure: identify the surface → open its overview first → open the exact reference page when needed → version-swap rule if the API is versioned.
- `##` group headings, `###` entry headings.
- Each entry: one `Use when:` line (one sentence), then labeled bare-URL lines — `Overview:`, `Full index:`, `Reference:`, or `Link:` as fits the page. Bare URLs only, no markdown link syntax, no extra prose.
- Close with `## Agent Routing Notes`: short bullets of routing heuristics specific to the topic.

Before writing, read references/exemplar-shopify-graphql-manifest.md — a real production manifest — and match its layout, labeling, and terseness.

# Write rules

- The destination is a directory: write `MANIFEST.md` inside it, creating the directory if needed.
- If a `MANIFEST.md` already exists there, stop and ask before overwriting or merging.
- Write only the manifest file. Do not copy documentation content, examples, or schemas into the repo.

# Validation

After writing, check the file against the format contract — every entry has a `Use when:` line and at least one labeled URL line, labels exact, routing notes present — and re-verify any URL you are not certain about. Fix and re-check until clean.

Report: the file path, the entry count, and any unofficial or unverified links included.
