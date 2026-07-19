---
name: sync-codex-records
description: Safely synchronize, migrate, back up, export, import, or relay Codex chat records and configuration with this repository's codex-sync CLI. Use when a user asks to sync Codex conversations between computers, move or back up ~/.codex data, discover LAN peers, use the Web UI, or transfer records through a USB drive, SMB server, NAS, NFS, WebDAV, Syncthing, OneDrive, Dropbox, iCloud Drive, or another shared folder.
---

# Sync Codex Records

Use the repository's `codex-sync` Rust CLI to transfer Codex data without exposing credentials or silently overwriting conflicts.

## Default direction

Interpret “sync my local Codex records to DESTINATION” as a one-way local-to-destination upload. Use only the local `~/.codex` records as the source. Treat the destination as write-only storage for this operation: do not inspect, download, merge, or import its existing Codex records.

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
- SSH destination: create a local-only snapshot, upload it to the requested remote path, and verify the uploaded copy. Do not enumerate or retrieve the remote host's existing `~/.codex` records unless the user explicitly requests a pull or restore.
- CC-Switch accounts on one computer: run `codex-sync cc-switch status`, then `codex-sync cc-switch merge` for a dry-run. Ask the user to close Codex and CC-Switch before running `codex-sync cc-switch merge --apply`. Never apply without explicit confirmation; the command backs up affected records and does not migrate credentials.

If `list` reports multiple snapshots, select the intended device snapshot path with the user and pass that exact path to `import` or `pull`.

## Verify

1. Read the command report: downloaded/acquired, skipped, and conflict counts.
2. Run `codex-sync status` on the destination.
3. Report every conflict. Conflicting remote versions use `.conflict-DEVICE` filenames by default.
4. Confirm sensitive excluded files did not enter a removable or shared snapshot when testing a new setup.
5. Never delete the source snapshot or source Codex data unless the user explicitly asks.

For demonstrations, use temporary directories and `--config` files under `/tmp`; do not modify the user's real `~/.codex` data.
