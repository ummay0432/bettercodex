# Compaction evaluation

`2026-08-06-live-matched-ab.json` preserves the complete grading record for a
matched live run of bettercodex and Codex across three native compaction
cadences. The durable fact was placed at the beginning of an 816,230-character
prompt, before enough filler that it could not survive the 64,000-character
retained-text tail. Both arms returned it exactly after the third compaction.

The JSON contains the exact prompt recipe, command configuration, every
user-visible assistant response, every cadence result, persisted rollout
counts, and lengths and SHA-256 hashes for all six opaque compaction payloads.
The encrypted payload bytes themselves remain in the isolated rollouts and are
not copied into Git: they are server-generated opaque state, not inspectable
semantic output, and their hashes make accidental omission or substitution
detectable.

This live run grades server-native semantic carry across repeated manual
cadences. Automatic triggering and client-side installation are covered by the
deterministic three-cadence Agent integration test at
`src/agent_tests.rs::repeated_auto_compaction_preserves_active_skill_and_cold_resume_state`.
That separation avoids pretending a timing- and token-dependent live trigger is
a deterministic regression test.

