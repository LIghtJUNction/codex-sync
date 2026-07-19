use crate::sync::{FileInfo, safe_path};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

#[derive(Debug, Serialize, Deserialize)]
struct SshManifest {
    source_node: String,
    created_at: u64,
    files: Vec<FileInfo>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SshReport {
    pub copied: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub removed: usize,
    pub database_rows: usize,
    pub backup: String,
}

pub fn push(host: &str, codex_home: Option<PathBuf>, keep_remote: bool) -> Result<SshReport> {
    validate_host(host)?;
    let home = dirs::home_dir().context("无法确定本机主目录")?;
    let codex_home = codex_home.unwrap_or_else(|| home.join(".codex"));
    if !codex_home.is_dir() {
        bail!("本机 Codex 目录不存在：{}", codex_home.display());
    }
    ensure_command("ssh")?;
    ensure_command("scp")?;

    let transfer_id = format!("{}-{}", hostname::get()?.to_string_lossy(), now());
    let temp = std::env::temp_dir().join(format!("codex-sync-ssh-{transfer_id}"));
    let bundle = temp.join("bundle");
    fs::create_dir_all(&bundle)?;
    let result = (|| -> Result<SshReport> {
        create_bundle(&codex_home, &bundle)?;
        let remote_rel = format!(".codex-sync/incoming/{transfer_id}");
        run(
            Command::new("ssh")
                .arg(host)
                .arg("mkdir")
                .arg("-p")
                .arg(&remote_rel),
            "创建远端接收目录",
        )?;
        let destination = format!("{host}:{remote_rel}/");
        run(
            Command::new("scp")
                .arg("-r")
                .arg(format!("{}/.", bundle.display()))
                .arg(&destination),
            "上传本机会话包",
        )?;
        let keep_flag = if keep_remote { " --keep-remote" } else { "" };
        let remote_command = format!(
            "bin=$(command -v codex-sync || true); if [ -z \"$bin\" ] && [ -x \"$HOME/.cargo/bin/codex-sync\" ]; then bin=\"$HOME/.cargo/bin/codex-sync\"; fi; if [ -z \"$bin\" ]; then echo '远端未安装 codex-sync 0.2.1+' >&2; exit 127; fi; \"$bin\" ssh receive --bundle \"$HOME/{remote_rel}\"{keep_flag}"
        );
        let output = Command::new("ssh")
            .arg(host)
            .arg(remote_command)
            .output()
            .context("调用远端 codex-sync")?;
        if !output.status.success() {
            bail!(
                "远端合并失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout)?;
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CODEX_SYNC_REPORT="))
            .context("远端未返回合并报告")?;
        let report: SshReport = serde_json::from_str(line)?;
        let cleanup = format!("rm -rf \"$HOME/{remote_rel}\"");
        run(
            Command::new("ssh").arg(host).arg(cleanup),
            "清理远端临时接收包",
        )?;
        Ok(report)
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

pub fn receive(bundle: &Path, codex_home: Option<PathBuf>, keep_remote: bool) -> Result<SshReport> {
    let home = dirs::home_dir().context("无法确定远端主目录")?;
    let codex_home = codex_home.unwrap_or_else(|| home.join(".codex"));
    fs::create_dir_all(&codex_home)?;
    let manifest: SshManifest = serde_json::from_slice(
        &fs::read(bundle.join("manifest.json")).context("接收包缺少 manifest.json")?,
    )?;
    verify_bundle(bundle, &manifest)?;
    let backup = codex_home
        .join("codex-sync-backups")
        .join(format!("ssh-import-{}", now()));
    fs::create_dir_all(&backup)?;
    let target_provider = active_model_provider(&codex_home);
    let mut report = SshReport {
        backup: backup.to_string_lossy().into_owned(),
        ..Default::default()
    };
    if !keep_remote {
        report.removed = remove_remote_only_files(&codex_home, &manifest, &backup)?;
    }

    for info in &manifest.files {
        if matches!(
            info.path.as_str(),
            "state_5.sqlite" | "session_index.jsonl" | "history.jsonl"
        ) {
            continue;
        }
        let source = safe_path(&bundle.join("files"), &info.path)?;
        let target = safe_path(&codex_home, &info.path)?;
        if target.exists() {
            if blake3::hash(&fs::read(&target)?).to_hex().as_str() == info.hash {
                report.skipped += 1;
                continue;
            } else if keep_remote {
                report.conflicts += 1;
                continue;
            } else {
                atomic_copy(&target, &backup.join("replaced").join(&info.path))?;
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if info.path.starts_with("sessions/") || info.path.starts_with("archived_sessions/") {
            copy_rollout(&source, &target, target_provider.as_deref())?;
        } else {
            atomic_copy(&source, &target)?;
        }
        report.copied += 1;
    }
    for name in ["session_index.jsonl", "history.jsonl"] {
        let source = bundle.join("files").join(name);
        if source.is_file() {
            let target = codex_home.join(name);
            if keep_remote {
                merge_jsonl(&source, &target, &backup.join(name))?;
            } else {
                if target.is_file() {
                    atomic_copy(&target, &backup.join(name))?;
                }
                atomic_copy(&source, &target)?;
            }
        }
    }
    let source_db = bundle.join("files/state_5.sqlite");
    let target_db = codex_home.join("state_5.sqlite");
    if source_db.is_file() {
        if target_db.is_file() {
            report.database_rows = merge_database(
                &source_db,
                &target_db,
                &backup.join("state_5.sqlite"),
                target_provider.as_deref(),
                keep_remote,
            )?;
        } else {
            atomic_copy(&source_db, &target_db)?;
        }
    }
    Ok(report)
}

fn remove_remote_only_files(
    codex_home: &Path,
    manifest: &SshManifest,
    backup: &Path,
) -> Result<usize> {
    let wanted: HashSet<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
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
            let rel = entry
                .path()
                .strip_prefix(codex_home)?
                .to_string_lossy()
                .replace('\\', "/");
            if wanted.contains(rel.as_str()) {
                continue;
            }
            atomic_copy(entry.path(), &backup.join("removed").join(&rel))?;
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn create_bundle(codex_home: &Path, bundle: &Path) -> Result<()> {
    let files = bundle.join("files");
    fs::create_dir_all(&files)?;
    for dir in ["sessions", "archived_sessions", "attachments"] {
        let source = codex_home.join(dir);
        if source.is_dir() {
            copy_tree(&source, &files.join(dir))?;
        }
    }
    for name in ["session_index.jsonl", "history.jsonl"] {
        let source = codex_home.join(name);
        if source.is_file() {
            atomic_copy(&source, &files.join(name))?;
        }
    }
    let state = codex_home.join("state_5.sqlite");
    if state.is_file() {
        let target = files.join("state_5.sqlite");
        let conn = Connection::open_with_flags(state, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute("VACUUM INTO ?1", params![target.to_string_lossy().as_ref()])?;
    }
    let manifest = build_manifest(&files)?;
    fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn build_manifest(files: &Path) -> Result<SshManifest> {
    let mut entries = Vec::new();
    for entry in WalkDir::new(files)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(files)?
            .to_string_lossy()
            .replace('\\', "/");
        if !allowed(&rel) {
            bail!("接收包包含不允许的路径：{rel}");
        }
        let bytes = fs::read(entry.path())?;
        let meta = entry.metadata()?;
        let modified = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        entries.push(FileInfo {
            path: rel,
            size: meta.len(),
            modified,
            hash: blake3::hash(&bytes).to_hex().to_string(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SshManifest {
        source_node: hostname::get()?.to_string_lossy().into_owned(),
        created_at: now(),
        files: entries,
    })
}

fn verify_bundle(bundle: &Path, manifest: &SshManifest) -> Result<()> {
    for info in &manifest.files {
        if !allowed(&info.path) {
            bail!("拒绝不允许的同步文件：{}", info.path);
        }
        let path = safe_path(&bundle.join("files"), &info.path)?;
        let bytes = fs::read(&path).with_context(|| format!("同步文件缺失：{}", info.path))?;
        if bytes.len() as u64 != info.size || blake3::hash(&bytes).to_hex().as_str() != info.hash {
            bail!("同步文件校验失败：{}", info.path);
        }
    }
    Ok(())
}

fn allowed(path: &str) -> bool {
    path == "state_5.sqlite"
        || path == "session_index.jsonl"
        || path == "history.jsonl"
        || path.starts_with("sessions/")
        || path.starts_with("archived_sessions/")
        || path.starts_with("attachments/")
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    for entry in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let rel = entry.path().strip_prefix(source)?;
        let dest = target.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(dest)?;
        } else if entry.file_type().is_file() {
            atomic_copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn atomic_copy(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension("codex-sync-tmp");
    fs::copy(source, &temp)?;
    fs::rename(temp, target)?;
    Ok(())
}

fn copy_rollout(source: &Path, target: &Path, provider: Option<&str>) -> Result<()> {
    let Some(provider) = provider else {
        return atomic_copy(source, target);
    };
    let mut output = String::new();
    for line in fs::read_to_string(source)?.lines() {
        let mut value: Value = serde_json::from_str(line)?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta")
            && let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut)
        {
            payload.insert("model_provider".into(), Value::String(provider.into()));
        }
        output.push_str(&serde_json::to_string(&value)?);
        output.push('\n');
    }
    let temp = target.with_extension("jsonl.codex-sync-tmp");
    fs::write(&temp, output)?;
    fs::rename(temp, target)?;
    Ok(())
}

fn merge_jsonl(source: &Path, target: &Path, backup: &Path) -> Result<()> {
    let existing = fs::read_to_string(target).unwrap_or_default();
    if target.is_file() {
        atomic_copy(target, backup)?;
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
    let temp = target.with_extension("jsonl.codex-sync-tmp");
    fs::write(&temp, output)?;
    fs::rename(temp, target)?;
    Ok(())
}

fn merge_database(
    source: &Path,
    target: &Path,
    backup: &Path,
    provider: Option<&str>,
    keep_remote: bool,
) -> Result<usize> {
    let mut conn = Connection::open(target)?;
    conn.execute("VACUUM INTO ?1", params![backup.to_string_lossy().as_ref()])?;
    conn.execute(
        "ATTACH DATABASE ?1 AS src",
        params![source.to_string_lossy().as_ref()],
    )?;
    let tx = conn.transaction()?;
    let mut inserted = 0;
    if !keep_remote {
        for table in ["thread_dynamic_tools", "thread_spawn_edges"] {
            if table_exists(&tx, "main", table)? {
                inserted += tx.execute(&format!("DELETE FROM main.\"{table}\""), [])?;
            }
        }
        if table_exists(&tx, "main", "threads")? && table_exists(&tx, "src", "threads")? {
            inserted += tx.execute(
                "DELETE FROM main.threads WHERE id NOT IN (SELECT id FROM src.threads)",
                [],
            )?;
        }
    }
    for table in ["threads", "thread_dynamic_tools", "thread_spawn_edges"] {
        if !table_exists(&tx, "main", table)? || !table_exists(&tx, "src", table)? {
            continue;
        }
        let dest = table_columns(&tx, "main", table)?;
        let src: HashSet<_> = table_columns(&tx, "src", table)?.into_iter().collect();
        let columns: Vec<_> = dest.into_iter().filter(|c| src.contains(c)).collect();
        if columns.is_empty() {
            continue;
        }
        let quoted = columns
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>();
        let select = columns
            .iter()
            .map(|c| {
                if table == "threads" && c == "model_provider" && provider.is_some() {
                    "?1".to_string()
                } else {
                    format!("\"{}\"", c.replace('"', "\"\""))
                }
            })
            .collect::<Vec<_>>();
        let verb = if keep_remote {
            "INSERT OR IGNORE"
        } else {
            "INSERT OR REPLACE"
        };
        let sql = format!(
            "{verb} INTO main.\"{table}\" ({}) SELECT {} FROM src.\"{table}\"",
            quoted.join(","),
            select.join(",")
        );
        inserted += if let ("threads", Some(provider)) = (table, provider) {
            tx.execute(&sql, [provider])?
        } else {
            tx.execute(&sql, [])?
        };
    }
    tx.commit()?;
    Ok(inserted)
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1"),
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn table_columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info('{table}')"))?;
    Ok(stmt
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn active_model_provider(codex_home: &Path) -> Option<String> {
    let text = fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value.get("model_provider")?.as_str().map(str::to_owned)
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._-".contains(c))
    {
        bail!("非法 SSH 主机名");
    }
    Ok(())
}

fn ensure_command(name: &str) -> Result<()> {
    if Command::new(name).arg("-V").output().is_err() {
        bail!("系统缺少 {name} 命令");
    }
    Ok(())
}

fn run(command: &mut Command, action: &str) -> Result<()> {
    let status = command.status().with_context(|| action.to_string())?;
    if !status.success() {
        bail!("{action}失败，退出码：{status}");
    }
    Ok(())
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

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-sync-ssh-test-{}-{}",
            std::process::id(),
            now()
        ))
    }

    #[test]
    fn host_validation_blocks_shell_syntax() {
        assert!(validate_host("lty").is_ok());
        assert!(validate_host("user@example.local").is_ok());
        assert!(validate_host("lty;rm -rf /").is_err());
    }

    #[test]
    fn receive_merges_local_records_into_remote_provider() -> Result<()> {
        let root = temp_dir();
        let local = root.join("local");
        let remote = root.join("remote");
        let bundle = root.join("bundle");
        fs::create_dir_all(local.join("sessions/2026/07/19"))?;
        fs::create_dir_all(&remote)?;
        fs::create_dir_all(&bundle)?;
        fs::write(
            local.join("sessions/2026/07/19/rollout-local.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"local\",\"model_provider\":\"local-provider\"}}\n",
        )?;
        fs::write(
            remote.join("config.toml"),
            "model_provider = \"remote-provider\"\n",
        )?;
        let local_db = Connection::open(local.join("state_5.sqlite"))?;
        local_db.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )?;
        local_db.execute("INSERT INTO threads VALUES ('local', 'local-provider')", [])?;
        drop(local_db);
        let remote_db = Connection::open(remote.join("state_5.sqlite"))?;
        remote_db.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )?;
        remote_db.execute(
            "INSERT INTO threads VALUES ('remote', 'remote-provider')",
            [],
        )?;
        drop(remote_db);

        create_bundle(&local, &bundle)?;
        let report = receive(&bundle, Some(remote.clone()), false)?;
        assert_eq!(report.copied, 1);
        assert!(report.database_rows >= 2);
        assert_eq!(report.removed, 0);
        let imported = remote.join("sessions/2026/07/19/rollout-local.jsonl");
        let text = fs::read_to_string(imported)?;
        assert!(text.contains("\"model_provider\":\"remote-provider\""));
        let remote_db = Connection::open(remote.join("state_5.sqlite"))?;
        let providers: Vec<String> = remote_db
            .prepare("SELECT model_provider FROM threads ORDER BY id")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(providers, vec!["remote-provider"]);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
