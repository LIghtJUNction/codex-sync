# Changelog

## Unreleased

- Replace the flat module layout with Rust 2018+ `module.rs` plus `module/child.rs` structure, add a reusable library entry point, split CLI parsing from dispatch, and prohibit `mod.rs`.
- Extract one reusable snapshot core for manifest generation, validation, hashing, incremental import, conflict handling, and atomic writes.
- Implement HTTP/Web and USB/SMB/Relay as thin adapters over the shared `SnapshotSource` trait.
- Separate SSH transport from the reusable Codex rollout, SQLite, provider, backup, and session-index merge engine.
- Rebuild the sidebar index from valid, non-archived top-level threads only; drop stale archived/subagent entries and report missing rollout files separately.
- Add read-only `history doctor` diagnostics and a deliberately narrow `--apply` mode with a SQLite writer lock, synced same-directory atomic replacement, pre-change index backup, post-write verification, and automatic rollback.
- Allow an explicitly selected, strictly validated backup JSONL to restore a missing rollout atomically with path, symlink, size, JSONL, filename, and thread-ID checks.
- Add one-rollout-at-a-time atomic `session_meta` deduplication that keeps the metadata matching the database thread ID, preserves all event bytes, detects concurrent changes, backs up the original, and rolls back on failure.

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
