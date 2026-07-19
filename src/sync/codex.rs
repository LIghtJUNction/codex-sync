//! Transport-independent merge engine for Codex rollouts, SQLite state, and index data.

use super::core;
use crate::history;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

const MAX_BUNDLE_FILE_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_BUNDLE_FILES: usize = 100_000;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MergeReport {
    pub copied: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub removed: usize,
    pub database_rows: usize,
    #[serde(default)]
    pub index_entries_added: usize,
    pub backup: String,
}

pub fn create_bundle(codex_home: &Path, bundle: &Path) -> Result<()> {
    let files = bundle.join("files");
    fs::create_dir_all(&files)?;
    for directory in ["sessions", "archived_sessions", "attachments"] {
        let source = codex_home.join(directory);
        if source.is_dir() {
            copy_tree(&source, &files.join(directory))?;
        }
    }
    for name in ["session_index.jsonl", "history.jsonl"] {
        let source = codex_home.join(name);
        if source.is_file() {
            core::atomic_copy(&source, &files.join(name))?;
        }
    }
    let state = codex_home.join("state_5.sqlite");
    if state.is_file() {
        let target = files.join("state_5.sqlite");
        let connection =
            Connection::open_with_flags(state, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute("VACUUM INTO ?1", params![target.to_string_lossy().as_ref()])?;
    }
    let node = hostname::get()?.to_string_lossy().into_owned();
    let manifest = core::manifest_tree(
        &files,
        node.clone(),
        node,
        MAX_BUNDLE_FILE_SIZE,
        MAX_BUNDLE_FILES,
        |relative| allowed(&relative.to_string_lossy().replace('\\', "/")),
    )?;
    core::atomic_write(
        &bundle.join(core::MANIFEST_FILE),
        &serde_json::to_vec_pretty(&manifest)?,
    )
}

pub fn apply_bundle(bundle: &Path, codex_home: &Path, mirror: bool) -> Result<MergeReport> {
    fs::create_dir_all(codex_home)?;
    let manifest = core::read_manifest(bundle).context("接收包缺少或无法读取 manifest.json")?;
    core::validate_manifest(
        codex_home,
        &manifest,
        MAX_BUNDLE_FILE_SIZE,
        MAX_BUNDLE_FILES,
    )?;
    if let Some(file) = manifest.files.iter().find(|file| !allowed(&file.path)) {
        bail!("拒绝不允许的同步文件：{}", file.path);
    }
    core::verify_snapshot(bundle, &manifest, MAX_BUNDLE_FILE_SIZE)?;

    let backup = codex_home
        .join("codex-sync-backups")
        .join(format!("sync-import-{}", now()));
    fs::create_dir_all(&backup)?;
    let provider = active_model_provider(codex_home);
    let mut report = MergeReport {
        backup: backup.to_string_lossy().into_owned(),
        ..Default::default()
    };
    if mirror {
        report.removed = remove_target_only_files(codex_home, &manifest, &backup)?;
    }

    for file in &manifest.files {
        if is_structured(file.path.as_str()) {
            continue;
        }
        let source = core::safe_path(&bundle.join("files"), &file.path)?;
        let target = core::safe_path(codex_home, &file.path)?;
        if target.exists() {
            let target_hash = blake3::hash(&fs::read(&target)?).to_hex();
            if target_hash.as_str() == file.hash {
                report.skipped += 1;
                continue;
            }
            if !mirror {
                if is_rollout(&file.path) {
                    core::atomic_copy(&target, &backup.join("merged").join(&file.path))?;
                    merge_rollout(&source, &target, provider.as_deref())?;
                    report.copied += 1;
                    continue;
                }
                report.conflicts += 1;
                continue;
            }
            core::atomic_copy(&target, &backup.join("replaced").join(&file.path))?;
        }
        if is_rollout(&file.path) {
            copy_rollout(&source, &target, provider.as_deref())?;
        } else {
            core::atomic_copy(&source, &target)?;
        }
        report.copied += 1;
    }

    for name in ["session_index.jsonl", "history.jsonl"] {
        let source = bundle.join("files").join(name);
        if !source.is_file() {
            continue;
        }
        let target = codex_home.join(name);
        if mirror {
            if target.is_file() {
                core::atomic_copy(&target, &backup.join(name))?;
            }
            core::atomic_copy(&source, &target)?;
        } else {
            merge_jsonl(&source, &target, &backup.join(name))?;
        }
    }

    let source_database = bundle.join("files/state_5.sqlite");
    let target_database = codex_home.join("state_5.sqlite");
    if source_database.is_file() {
        if target_database.is_file() {
            report.database_rows = merge_database(
                &source_database,
                &target_database,
                &backup.join("state_5.sqlite"),
                provider.as_deref(),
                mirror,
            )?;
        } else {
            core::atomic_copy(&source_database, &target_database)?;
        }
    }
    report.index_entries_added = history::reconcile_session_index(codex_home)?;
    Ok(report)
}

fn is_structured(path: &str) -> bool {
    matches!(
        path,
        "state_5.sqlite" | "session_index.jsonl" | "history.jsonl"
    )
}

fn is_rollout(path: &str) -> bool {
    path.starts_with("sessions/") || path.starts_with("archived_sessions/")
}

fn allowed(path: &str) -> bool {
    is_structured(path) || is_rollout(path) || path.starts_with("attachments/")
}

fn remove_target_only_files(
    codex_home: &Path,
    manifest: &core::Manifest,
    backup: &Path,
) -> Result<usize> {
    let wanted: HashSet<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let mut removed = 0;
    for root in ["sessions", "archived_sessions", "attachments"] {
        let directory = codex_home.join(root);
        if !directory.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&directory)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(codex_home)?
                .to_string_lossy()
                .replace('\\', "/");
            if wanted.contains(relative.as_str()) {
                continue;
            }
            core::atomic_copy(entry.path(), &backup.join("removed").join(&relative))?;
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    for entry in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(source)?;
        core::atomic_copy(entry.path(), &target.join(relative))?;
    }
    Ok(())
}

fn copy_rollout(source: &Path, target: &Path, provider: Option<&str>) -> Result<()> {
    write_rollout(&[source], target, provider)
}

fn merge_rollout(source: &Path, target: &Path, provider: Option<&str>) -> Result<()> {
    write_rollout(&[target, source], target, provider)
}

fn write_rollout(sources: &[&Path], target: &Path, provider: Option<&str>) -> Result<()> {
    let mut seen = HashSet::new();
    let mut saw_session_meta = false;
    let mut output = String::new();
    for source in sources {
        append_rollout(
            &fs::read_to_string(source)?,
            provider,
            &mut seen,
            &mut saw_session_meta,
            &mut output,
        )?;
    }
    core::atomic_write(target, output.as_bytes())
}

fn append_rollout(
    text: &str,
    provider: Option<&str>,
    seen: &mut HashSet<String>,
    saw_session_meta: &mut bool,
    output: &mut String,
) -> Result<()> {
    for line in text.lines() {
        let mut value: Value = serde_json::from_str(line)?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if *saw_session_meta {
                continue;
            }
            *saw_session_meta = true;
            if let Some(provider) = provider
                && let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut)
            {
                payload.insert("model_provider".into(), Value::String(provider.into()));
            }
        }
        let rendered = serde_json::to_string(&value)?;
        if seen.insert(rendered.clone()) {
            output.push_str(&rendered);
            output.push('\n');
        }
    }
    Ok(())
}

fn merge_jsonl(source: &Path, target: &Path, backup: &Path) -> Result<()> {
    let existing = fs::read_to_string(target).unwrap_or_default();
    if target.is_file() {
        core::atomic_copy(target, backup)?;
    }
    let mut seen: HashSet<String> = existing.lines().map(str::to_owned).collect();
    let mut output = existing;
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    for line in fs::read_to_string(source)?.lines() {
        if seen.insert(line.to_owned()) {
            output.push_str(line);
            output.push('\n');
        }
    }
    core::atomic_write(target, output.as_bytes())
}

fn merge_database(
    source: &Path,
    target: &Path,
    backup: &Path,
    provider: Option<&str>,
    mirror: bool,
) -> Result<usize> {
    let mut connection = Connection::open(target)?;
    connection.execute("VACUUM INTO ?1", params![backup.to_string_lossy().as_ref()])?;
    connection.execute(
        "ATTACH DATABASE ?1 AS src",
        params![source.to_string_lossy().as_ref()],
    )?;
    let transaction = connection.transaction()?;
    let mut changed = 0;
    if mirror {
        for table in ["thread_dynamic_tools", "thread_spawn_edges"] {
            if table_exists(&transaction, "main", table)? {
                changed += transaction.execute(&format!("DELETE FROM main.\"{table}\""), [])?;
            }
        }
        if table_exists(&transaction, "main", "threads")?
            && table_exists(&transaction, "src", "threads")?
        {
            changed += transaction.execute(
                "DELETE FROM main.threads WHERE id NOT IN (SELECT id FROM src.threads)",
                [],
            )?;
        }
    }
    for table in ["threads", "thread_dynamic_tools", "thread_spawn_edges"] {
        if !table_exists(&transaction, "main", table)? || !table_exists(&transaction, "src", table)?
        {
            continue;
        }
        let destination = table_columns(&transaction, "main", table)?;
        let source_columns: HashSet<_> = table_columns(&transaction, "src", table)?
            .into_iter()
            .collect();
        let columns: Vec<_> = destination
            .into_iter()
            .filter(|column| source_columns.contains(column))
            .collect();
        if columns.is_empty() {
            continue;
        }
        let quoted = columns
            .iter()
            .map(|column| quote(column))
            .collect::<Vec<_>>();
        let selected = columns
            .iter()
            .map(|column| {
                if table == "threads" && column == "model_provider" && provider.is_some() {
                    "?1".to_owned()
                } else {
                    quote(column)
                }
            })
            .collect::<Vec<_>>();
        let verb = if mirror {
            "INSERT OR REPLACE"
        } else {
            "INSERT OR IGNORE"
        };
        let sql = format!(
            "{verb} INTO main.\"{table}\" ({}) SELECT {} FROM src.\"{table}\"",
            quoted.join(","),
            selected.join(",")
        );
        changed += if let ("threads", Some(provider)) = (table, provider) {
            transaction.execute(&sql, [provider])?
        } else {
            transaction.execute(&sql, [])?
        };
    }
    transaction.commit()?;
    Ok(changed)
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn table_exists(connection: &Connection, schema: &str, table: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1"),
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn table_columns(connection: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA {schema}.table_info('{table}')"))?;
    Ok(statement
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn active_model_provider(codex_home: &Path) -> Option<String> {
    let text = fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    Some(
        value
            .get("model_provider")
            .and_then(toml::Value::as_str)
            .unwrap_or("openai")
            .to_owned(),
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "codex-sync-merge-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn bundle_merge_uses_target_provider_and_rebuilds_index() -> Result<()> {
        let root = temp_dir();
        let source = root.join("source");
        let target = root.join("target");
        let bundle = root.join("bundle");
        fs::create_dir_all(source.join("sessions/2026/07/19"))?;
        fs::create_dir_all(&target)?;
        fs::write(
            source.join("sessions/2026/07/19/rollout-local.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"local\",\"model_provider\":\"local\"}}\n",
        )?;
        fs::write(target.join("config.toml"), "model_provider = \"remote\"\n")?;
        create_thread_database(&source.join("state_5.sqlite"), "local", "local")?;
        create_thread_database(&target.join("state_5.sqlite"), "remote", "remote")?;

        create_bundle(&source, &bundle)?;
        let report = apply_bundle(&bundle, &target, false)?;
        assert_eq!(report.copied, 1);
        assert_eq!(report.database_rows, 1);
        assert_eq!(report.index_entries_added, 2);
        let rollout = fs::read_to_string(target.join("sessions/2026/07/19/rollout-local.jsonl"))?;
        assert!(rollout.contains("\"model_provider\":\"remote\""));
        assert_eq!(
            fs::read_to_string(target.join("session_index.jsonl"))?
                .lines()
                .count(),
            2
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rollout_merge_keeps_one_meta_and_both_event_sets() -> Result<()> {
        let root = temp_dir();
        fs::create_dir_all(&root)?;
        let source = root.join("source.jsonl");
        let target = root.join("target.jsonl");
        fs::write(
            &source,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"one\",\"model_provider\":\"local\"}}\n{\"type\":\"event\",\"id\":\"local\"}\n",
        )?;
        fs::write(
            &target,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"one\",\"model_provider\":\"old\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"one\"}}\n{\"type\":\"event\",\"id\":\"remote\"}\n",
        )?;
        merge_rollout(&source, &target, Some("openai"))?;
        let merged = fs::read_to_string(&target)?;
        assert_eq!(merged.matches("session_meta").count(), 1);
        assert!(merged.contains("\"model_provider\":\"openai\""));
        assert!(merged.contains("\"id\":\"local\""));
        assert!(merged.contains("\"id\":\"remote\""));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    fn create_thread_database(path: &Path, id: &str, provider: &str) -> Result<()> {
        let connection = Connection::open(path)?;
        connection.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )?;
        connection.execute("INSERT INTO threads VALUES (?1, ?2)", params![id, provider])?;
        Ok(())
    }
}
