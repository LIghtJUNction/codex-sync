# Changelog

## 0.3.0

- Require authentication for every LAN API endpoint and use constant-time shared-key comparison.
- Add Web UI shared-key entry stored only for the current browser session.
- Validate remote manifests, file counts, file sizes, paths, and BLAKE3 hashes before writing data.
- Reject serving or overwriting symbolic links; use synced atomic writes for downloaded files.
- Add configurable file-count and file-size limits, with secure Unix permissions for newly created configuration files.
- Add cross-platform GitHub Actions checks for formatting, Clippy, and tests.
