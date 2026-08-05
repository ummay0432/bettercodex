# Papercuts
- `apply_patch` accepted absolute repository paths for earlier patch sections, then unexpectedly removed unrelated documentation in the same tree and failed to find a later absolute path. Use repository-relative paths with `apply_patch`; its interface does not document this constraint.
