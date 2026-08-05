# Papercuts
- `apply_patch` accepted absolute repository paths for earlier patch sections, then unexpectedly removed unrelated documentation in the same tree and failed to find a later absolute path. Use repository-relative paths with `apply_patch`; its interface does not document this constraint.
- Repeated identical `functions.exec` polling cells for a live `write_stdin` session intermittently fail before execution with `SyntaxError: Invalid or unexpected token`; the exec wrapper should make valid retryable polling cells deterministic or expose the offending generated source.
