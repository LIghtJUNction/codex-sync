# Changelog

## 0.4.0

- Add provider-independent `history status`, `history merge`, `history backup`, and `history restore` commands.
- Keep `state_5.sqlite`, rollout `session_meta`, and `session_index.jsonl` consistent when switching provider, model, or login method.
- Use SQLite online backups, write-lock waiting, atomic metadata writes, dry-run defaults, and pre-restore safety backups.
- Reconcile `session_index.jsonl` by thread ID after every SSH database merge so newly imported threads are immediately indexed.
- Keep exactly one `session_meta` record per rollout during copy/merge and repair duplicates left by older sync runs.
- Adapt the local-history recovery model from the MIT-licensed `GODGOD126/codex-history-sync-tool` reference implementation.

## 0.3.0

- Require authentication for every LAN API endpoint and use constant-time shared-key comparison.
- Add Web UI shared-key entry stored only for the current browser session.
- Validate remote manifests, file counts, file sizes, paths, and BLAKE3 hashes before writing data.
- Reject serving or overwriting symbolic links; use synced atomic writes for downloaded files.
- Add configurable file-count and file-size limits, with secure Unix permissions for newly created configuration files.
- Add cross-platform GitHub Actions checks for formatting, Clippy, and tests.
