---
name: sync-codex-records
description: Safely synchronize, merge, repair, migrate, back up, export, import, or relay local Codex chat records and configuration with this repository's codex-sync CLI. Use when a user asks to sync Codex conversations between computers over LAN or SSH; move or back up ~/.codex data; merge CC-Switch accounts; restore histories hidden after switching provider, model, API, or login; repair state_5.sqlite, session_index.jsonl, or duplicate session_meta records; use the Web UI; or transfer records through USB, SMB, NAS, NFS, WebDAV, Syncthing, OneDrive, Dropbox, iCloud Drive, Google Drive, or another shared folder.
---

# Sync Codex Records

Use the repository's `codex-sync` Rust CLI to transfer Codex data without exposing credentials or silently overwriting conflicts.

## Default direction

Interpret “sync my local Codex records to DESTINATION” as a one-way local-to-destination incremental merge. Use only the local `~/.codex` records as the source. Add local records that are absent from the destination, preserve destination records, and never download them. Only use destructive mirroring when the user explicitly asks; for SSH, require `--mirror` and back up destination-only records before removing them.

Only pull, restore, or merge records from a remote host into the local computer when the user explicitly asks for that direction. Never infer bidirectional synchronization from the word “sync”.

## Prepare

1. Work from the repository root containing `Cargo.toml`.
2. Build with `cargo build --release` when `target/release/codex-sync` is absent or stale.
3. Run commands through `target/release/codex-sync`. Use `--config PATH` when operating on a non-default or test configuration.
4. Initialize with `codex-sync init` only when the default config is missing. Ask the user to set the same long random `shared_key` on LAN peers; never print or copy an existing key into chat.
5. Run `codex-sync status` before transfer to report the node, source directory, and file count.

Do not include `auth.json`, credential files, logs, caches, or temporary files. Preserve the configured exclusion list. Do not use `--overwrite` unless the user explicitly requests replacement and understands that local versions will be lost.

## Choose a transport

- Same trusted LAN: start `codex-sync serve` on each computer, discover with `codex-sync peers`, then run `codex-sync pull HOST:PORT`. Offer the Web UI at `http://localhost:8787`.
- USB or removable disk: run `codex-sync usb export MOUNT`, then `codex-sync usb list MOUNT` and `codex-sync usb import MOUNT` on the destination.
- Mounted SMB share: run `codex-sync smb push SHARE`, then `codex-sync smb list SHARE` and `codex-sync smb pull SHARE` on the destination. Let the operating system manage SMB credentials.
- NFS, WebDAV, Syncthing, Dropbox, OneDrive, iCloud Drive, Google Drive, NAS, or any shared directory: run `codex-sync relay push DIR`, wait for the storage service to finish syncing, then run `codex-sync relay list DIR` and `codex-sync relay pull DIR` on the destination.
- SSH destination: install the same current `codex-sync` version on both computers, then run `codex-sync ssh push HOST`. This command must remain one-way and incremental by default: package only local records, upload only missing content, and invoke the remote receiver to merge them into the remote current provider. Do not enumerate or retrieve the remote host's existing `~/.codex` records unless the user explicitly requests a pull or restore.
- CC-Switch accounts on one computer: run `codex-sync cc-switch status`, then `codex-sync cc-switch merge` for a dry-run. Ask the user to close Codex and CC-Switch before running `codex-sync cc-switch merge --apply`. Never apply without explicit confirmation; the command backs up affected records and does not migrate credentials.
- Local history corruption or archive/delete failures: run `codex-sync history doctor` first. It is read-only and checks SQLite integrity, rollout JSONL/session metadata, missing files, and sidebar index consistency. `history doctor --apply` may be used only after explicit confirmation and preferably with Codex and CC-Switch closed; without extra recovery arguments it atomically repairs only the sidebar index, creates a rollback copy, verifies the replacement, and restores the original on failure. User sidebar reconstruction must exclude JSON `source.subagent` records, `thread_source = "subagent"` records, and untitled `codex-auto-review` technical records; never expose guardian/worker/reviewer implementation tasks as user chats. If one missing rollout has been located in a trusted backup, validate that exact candidate first and pass it explicitly with `--restore-missing BACKUP_JSONL --apply`; never guess among differing backup candidates. The command must reject a candidate unless its path, filename, JSONL, single session metadata record, and thread ID all match the missing database row. For a stable rollout with multiple metadata records, use `--dedupe-session-meta ROLLOUT --apply` one file at a time; keep only the metadata whose ID matches the database row, preserve every non-metadata event, and never touch the currently active rollout. It deliberately does not rewrite databases, providers, or models.
- Local histories hidden after switching provider, model, API, or login method: after `history doctor`, run `codex-sync history status`, then `codex-sync history merge` for a dry-run. Use `codex-sync history merge --apply` only after the user confirms the preview. This path does not require CC-Switch; it updates the SQLite thread assignment, rollout `session_meta`, and sidebar index together, and creates an online backup first. Use `history backup` for a manual snapshot. `history restore BACKUP` is dry-run by default and requires both explicit user authorization and `--apply` because it replaces current local history state.

If `list` reports multiple snapshots, select the intended device snapshot path with the user and pass that exact path to `import` or `pull`.

## Verify

1. Read the command report: downloaded/acquired, skipped, and conflict counts.
2. Run `codex-sync status` on the destination.
3. For SSH, confirm the report includes the number of sidebar index entries added, then verify every non-archived database thread ID exists in `session_index.jsonl`.
4. Run `history doctor` and distinguish structural corruption from provider/model reassignment. Use `history doctor --apply` for index-only repairs. For a stable rollout with duplicate metadata, obtain confirmation, close Codex and CC-Switch, then run `history doctor --dedupe-session-meta ROLLOUT --apply` on that one file and report its backup path. Never deduplicate a rollout while its task is active.
5. Report every conflict. Conflicting remote versions use `.conflict-DEVICE` filenames by default.
6. Confirm sensitive excluded files did not enter a removable or shared snapshot when testing a new setup.
7. Never delete the source snapshot or source Codex data unless the user explicitly asks.

For demonstrations, use temporary directories and `--config` files under `/tmp`; do not modify the user's real `~/.codex` data.
