use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct Paths {
    codex_home: PathBuf,
    cc_switch_home: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub current_cc_switch_provider: String,
    pub target_model_provider: String,
    pub rollout_counts: BTreeMap<String, usize>,
    pub database_counts: BTreeMap<String, usize>,
    pub rollout_files_to_change: usize,
    pub database_rows_to_change: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
}

impl Paths {
    pub fn resolve(codex_home: Option<PathBuf>, cc_switch_home: Option<PathBuf>) -> Result<Self> {
        let home = dirs::home_dir().context("无法确定用户主目录")?;
        let paths = Self {
            codex_home: codex_home.unwrap_or_else(|| home.join(".codex")),
            cc_switch_home: cc_switch_home.unwrap_or_else(|| home.join(".cc-switch")),
        };
        if !paths.codex_home.is_dir() {
            bail!("Codex 数据目录不存在：{}", paths.codex_home.display());
        }
        if !paths.cc_switch_home.join("cc-switch.db").is_file() {
            bail!("未检测到 CC-Switch：{}", paths.cc_switch_home.display());
        }
        Ok(paths)
    }
}

pub fn inspect(paths: &Paths) -> Result<Report> {
    let current_cc_switch_provider = current_cc_switch_provider(paths)?;
    let target_model_provider = active_model_provider(paths)?;
    let (rollout_counts, rollout_files_to_change) = rollout_counts(paths, &target_model_provider)?;
    let (database_counts, database_rows_to_change) =
        database_counts(paths, &target_model_provider)?;
    Ok(Report {
        current_cc_switch_provider,
        target_model_provider,
        rollout_counts,
        database_counts,
        rollout_files_to_change,
        database_rows_to_change,
        backup: None,
    })
}

pub fn merge(paths: &Paths, apply: bool) -> Result<Report> {
    let mut report = inspect(paths)?;
    if !apply || (report.rollout_files_to_change == 0 && report.database_rows_to_change == 0) {
        return Ok(report);
    }
    let backup = backup_dir(paths)?;
    fs::create_dir_all(&backup)?;
    rewrite_rollouts(paths, &report.target_model_provider, &backup)?;
    rewrite_database(paths, &report.target_model_provider, &backup)?;
    report.backup = Some(backup);
    let verified = inspect(paths)?;
    if verified.rollout_files_to_change != 0 || verified.database_rows_to_change != 0 {
        bail!("合并后验证失败；原始文件已备份，请勿继续使用并检查备份");
    }
    Ok(report)
}

pub fn print_report(report: &Report) {
    println!("CC-Switch 当前账号：{}", report.current_cc_switch_provider);
    println!("Codex 当前记录桶：{}", report.target_model_provider);
    println!("会话文件分布：");
    for (provider, count) in &report.rollout_counts {
        println!("  {provider}: {count}");
    }
    println!("数据库记录分布：");
    for (provider, count) in &report.database_counts {
        println!("  {provider}: {count}");
    }
    println!(
        "待合并：{} 个会话文件，{} 条数据库记录",
        report.rollout_files_to_change, report.database_rows_to_change
    );
}

fn active_model_provider(paths: &Paths) -> Result<String> {
    let text = fs::read_to_string(paths.codex_home.join("config.toml"))
        .context("无法读取 Codex config.toml")?;
    let value: toml::Value = toml::from_str(&text).context("Codex config.toml 格式错误")?;
    value
        .get("model_provider")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .context("Codex config.toml 缺少 model_provider，无法确定当前记录桶")
}

fn current_cc_switch_provider(paths: &Paths) -> Result<String> {
    let conn = Connection::open_with_flags(
        paths.cc_switch_home.join("cc-switch.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.query_row(
        "SELECT name FROM providers WHERE app_type='codex' AND is_current=1 LIMIT 1",
        [],
        |row| row.get(0),
    )
    .context("CC-Switch 没有当前 Codex 账号")
}

fn rollout_paths(paths: &Paths) -> Vec<PathBuf> {
    ["sessions", "archived_sessions"]
        .into_iter()
        .flat_map(|dir| {
            WalkDir::new(paths.codex_home.join(dir))
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl")
                })
                .map(|e| e.into_path())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn rollout_counts(paths: &Paths, target: &str) -> Result<(BTreeMap<String, usize>, usize)> {
    let mut counts = BTreeMap::new();
    let mut changes = 0;
    for path in rollout_paths(paths) {
        if let Some(provider) = rollout_provider(&path)? {
            *counts.entry(provider.clone()).or_insert(0) += 1;
            if provider != target {
                changes += 1;
            }
        }
    }
    Ok((counts, changes))
}

fn rollout_provider(path: &Path) -> Result<Option<String>> {
    for line in fs::read_to_string(path)?.lines() {
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("JSONL 格式错误：{}", path.display()))?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            return Ok(value
                .pointer("/payload/model_provider")
                .and_then(Value::as_str)
                .map(str::to_owned));
        }
    }
    Ok(None)
}

fn database_counts(paths: &Paths, target: &str) -> Result<(BTreeMap<String, usize>, usize)> {
    let db = paths.codex_home.join("state_5.sqlite");
    if !db.is_file() {
        return Ok((BTreeMap::new(), 0));
    }
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt =
        conn.prepare("SELECT model_provider, COUNT(*) FROM threads GROUP BY model_provider")?;
    let mut counts = BTreeMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
    })?;
    for row in rows {
        let (provider, count) = row?;
        counts.insert(provider, count);
    }
    let changes = conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE model_provider <> ?1",
        [target],
        |row| row.get(0),
    )?;
    Ok((counts, changes))
}

fn backup_dir(paths: &Paths) -> Result<PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(paths
        .codex_home
        .join("codex-sync-backups")
        .join(format!("cc-switch-merge-{timestamp}")))
}

fn rewrite_rollouts(paths: &Paths, target: &str, backup: &Path) -> Result<()> {
    for path in rollout_paths(paths) {
        if rollout_provider(&path)?
            .as_deref()
            .is_none_or(|p| p == target)
        {
            continue;
        }
        let rel = path.strip_prefix(&paths.codex_home)?;
        let backup_path = backup.join(rel);
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&path, &backup_path)?;
        let mut output = String::new();
        for line in fs::read_to_string(&path)?.lines() {
            let mut value: Value = serde_json::from_str(line)?;
            if value.get("type").and_then(Value::as_str) == Some("session_meta")
                && let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut)
            {
                payload.insert("model_provider".into(), Value::String(target.into()));
            }
            output.push_str(&serde_json::to_string(&value)?);
            output.push('\n');
        }
        let temp = path.with_extension("jsonl.codex-sync-tmp");
        fs::write(&temp, output)?;
        fs::rename(temp, path)?;
    }
    Ok(())
}

fn rewrite_database(paths: &Paths, target: &str, backup: &Path) -> Result<()> {
    let db = paths.codex_home.join("state_5.sqlite");
    if !db.is_file() {
        return Ok(());
    }
    let backup_db = backup.join("state_5.sqlite");
    let mut conn = Connection::open(&db)?;
    conn.execute(
        "VACUUM INTO ?1",
        params![backup_db.to_string_lossy().as_ref()],
    )?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE threads SET model_provider=?1 WHERE model_provider<>?1",
        [target],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-sync-cc-switch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_directories_are_rejected() {
        assert!(
            Paths::resolve(
                Some(PathBuf::from("/missing-codex")),
                Some(PathBuf::from("/missing-cc-switch"))
            )
            .is_err()
        );
    }

    #[test]
    fn merges_rollouts_and_database_with_backup() -> Result<()> {
        let root = temp_dir();
        let codex = root.join(".codex");
        let cc = root.join(".cc-switch");
        fs::create_dir_all(codex.join("sessions/2026/07/19"))?;
        fs::create_dir_all(&cc)?;
        fs::write(codex.join("config.toml"), "model_provider = \"current\"\n")?;
        let rollout = codex.join("sessions/2026/07/19/rollout-test.jsonl");
        fs::write(
            &rollout,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"one\",\"model_provider\":\"old\"}}\n",
        )?;

        let state = Connection::open(codex.join("state_5.sqlite"))?;
        state.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )?;
        state.execute("INSERT INTO threads VALUES ('one', 'old')", [])?;
        drop(state);
        let switch = Connection::open(cc.join("cc-switch.db"))?;
        switch.execute(
            "CREATE TABLE providers (name TEXT, app_type TEXT, is_current INTEGER)",
            [],
        )?;
        switch.execute(
            "INSERT INTO providers VALUES ('Current Account', 'codex', 1)",
            [],
        )?;
        drop(switch);

        let paths = Paths {
            codex_home: codex.clone(),
            cc_switch_home: cc,
        };
        let preview = merge(&paths, false)?;
        assert_eq!(preview.rollout_files_to_change, 1);
        assert_eq!(preview.database_rows_to_change, 1);
        let applied = merge(&paths, true)?;
        assert!(applied.backup.as_ref().is_some_and(|p| p.is_dir()));
        assert_eq!(rollout_provider(&rollout)?.as_deref(), Some("current"));
        let provider: String = Connection::open(codex.join("state_5.sqlite"))?.query_row(
            "SELECT model_provider FROM threads",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(provider, "current");
        let verified = inspect(&paths)?;
        assert_eq!(verified.rollout_files_to_change, 0);
        assert_eq!(verified.database_rows_to_change, 0);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
