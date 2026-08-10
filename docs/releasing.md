# Releasing bettercodex

Release only when the user explicitly asks to prepare or publish one. Preparing
a draft does not authorize publication, and routine source changes never imply
release authorization.

## Contract

- Release one immutable full revision already present on public `main`.
- Read the version from that revision's `Cargo.toml`. It must be strictly newer
  than the latest published release and unused by every existing tag, draft,
  and release.
- Use tag `bcodex-v<version>-<40-character-revision>` and title
  `bettercodex <version>`.
- Build all targets from that same revision on standard GitHub-hosted runners.
- Create exactly these five compressed assets for the three supported binaries:

  - `bcodex-aarch64-apple-darwin.gz`
  - `bcodex-aarch64-apple-darwin.zst`
  - `bcodex-x86_64-pc-windows-msvc.exe.gz`
  - `bcodex-x86_64-unknown-linux-gnu.gz`
  - `bcodex-x86_64-unknown-linux-gnu.zst`

- Qualify the Linux asset on Ubuntu 22.04 and Debian 12. Do not rebuild a
  separate Debian binary.
- On Windows, run the native installer suite, update-path Rust tests, release
  build, and package smoke test. The complete native product matrix remains the
  promotion gate in [`SPEC-WINDOWS.md`](../SPEC-WINDOWS.md).
- Keep the zstd copies of the Unix binaries compatible with immutable 0.1.2
  clients; they are alternate encodings, not additional binaries.
- Keep every decompressed binary at or below 128 MiB, the limit enforced by
  both current installers and immutable 0.1.2 clients.
- GitHub's release API digest is the asset checksum. Do not add checksum,
  manifest, source, or installer assets.

The manual [release workflow](../.github/workflows/release.yml) validates the
revision and version, tests each native installer and binary, embeds the exact
release tag, creates the five assets, and opens a draft release. It never runs
on pushes, schedules, or version changes and never publishes the draft.

## Prepare a draft

1. Confirm GitHub Actions are enabled for the public repository and the release
   workflow is present on the default branch.
2. Select a full lowercase commit ID already on public `main`. Do not release
   working-tree content or silently follow `main` if it moves.
3. Confirm the candidate has a plain `major.minor.patch` package version newer
   than the latest published release. Remove or resolve any stale draft for
   that version before proceeding; never overwrite it automatically.
4. Dispatch the workflow with the pinned revision:

   ```sh
   gh workflow run release.yml --repo ummay0432/bettercodex \
     -f revision=<40-character-revision>
   ```

5. Monitor the exact run through completion. A retry is allowed only for a
   transient infrastructure failure and must use the same revision. Source,
   test, packaging, or compatibility failures block the release.

## Verify and publish

Before publication, query the draft and verify:

- its tag encodes the selected version and exact source revision;
- it is a draft, not a prerelease;
- all build and Linux qualification jobs passed;
- the five asset names above are present exactly once, with nonzero sizes and
  `sha256:` digests; and
- each decompressed binary reports the expected version and embedded tag and
  passed its isolated installation smoke test in Actions.

Do not edit or replace individual binaries. If the draft is invalid, delete it
only with explicit authorization, fix the source or workflow in a new revision,
and require a new release decision.

If publication was explicitly authorized, publish the verified draft as the
latest non-prerelease release. Then query the public release again and confirm
its tag, target revision, five-asset set, digests, and latest status. Published
releases must remain immutable.

Report the release URL, version, full revision, workflow run URL, three targets,
five assets, and final verification result. If any gate fails, leave the
previous release current and report the blocker.
