use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, backup::Backup, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Paths {
    codex_home: PathBuf,
    config: PathBuf,
    database: PathBuf,
    session_index: PathBuf,
    backup_root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub current_provider: String,
    pub current_model: Option<String>,
    pub total_threads: usize,
    pub database_counts: BTreeMap<String, usize>,
    pub model_counts: BTreeMap<String, usize>,
    pub database_rows_to_change: usize,
    pub rollout_counts: BTreeMap<String, usize>,
    pub rollout_model_counts: BTreeMap<String, usize>,
    pub rollout_files: usize,
    pub rollout_files_to_change: usize,
    pub duplicate_session_meta_lines: usize,
    pub missing_session_index_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
}

#[derive(Debug)]
struct Assignment {
    provider: String,
    model: Option<String>,
}

#[derive(Debug)]
struct SessionRecord {
    path: PathBuf,
    provider: String,
    model: Option<String>,
    session_meta_lines: usize,
}

impl Paths {
    pub fn resolve(codex_home: Option<PathBuf>) -> Result<Self> {
        let home = dirs::home_dir().context("无法确定用户主目录")?;
        let codex_home = codex_home.unwrap_or_else(|| home.join(".codex"));
        let paths = Self {
            config: codex_home.join("config.toml"),
            database: codex_home.join("state_5.sqlite"),
            session_index: codex_home.join("session_index.jsonl"),
            backup_root: codex_home.join("codex-sync-backups"),
            codex_home,
        };
        paths.validate()?;
        Ok(paths)
    }

    fn validate(&self) -> Result<()> {
        if !self.codex_home.is_dir() {
            bail!("Codex 数据目录不存在：{}", self.codex_home.display());
        }
        if !self.config.is_file() {
            bail!("Codex 配置不存在：{}", self.config.display());
        }
        if !self.database.is_file() {
            bail!("Codex 历史数据库不存在：{}", self.database.display());
        }
        Ok(())
    }
}

pub fn inspect(paths: &Paths) -> Result<Report> {
    let target = active_assignment(paths)?;
    let records = session_records(paths)?;
    let mut rollout_counts = BTreeMap::new();
    let mut rollout_model_counts = BTreeMap::new();
    let mut rollout_files_to_change = 0;
    let mut duplicate_session_meta_lines = 0;
    for record in &records {
        *rollout_counts
            .entry(display_value(&record.provider))
            .or_insert(0) += 1;
        *rollout_model_counts
            .entry(display_value(record.model.as_deref().unwrap_or("")))
            .or_insert(0) += 1;
        if assignment_differs(&record.provider, record.model.as_deref(), &target) {
            rollout_files_to_change += 1;
        }
        duplicate_session_meta_lines += record.session_meta_lines.saturating_sub(1);
    }

    let conn = open_database(&paths.database, true)?;
    let columns = thread_columns(&conn)?;
    if !columns.contains("model_provider") {
        bail!("threads 表缺少 model_provider 字段，无法安全归并历史");
    }
    let database_counts = grouped_counts(&conn, "model_provider")?;
    let model_counts = if columns.contains("model") {
        grouped_counts(&conn, "model")?
    } else {
        BTreeMap::new()
    };
    let total_threads = conn.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
    let database_rows_to_change = count_database_changes(&conn, &columns, &target)?;
    let database_ids = active_database_ids(&conn, &columns)?;
    let index_ids = read_session_index(&paths.session_index)?
        .into_keys()
        .collect::<HashSet<_>>();

    Ok(Report {
        current_provider: target.provider,
        current_model: target.model,
        total_threads,
        database_counts,
        model_counts,
        database_rows_to_change,
        rollout_counts,
        rollout_model_counts,
        rollout_files: records.len(),
        rollout_files_to_change,
        duplicate_session_meta_lines,
        missing_session_index_entries: database_ids.difference(&index_ids).count(),
        backup: None,
    })
}

pub fn merge(paths: &Paths, apply: bool) -> Result<Report> {
    let mut report = inspect(paths)?;
    let needs_change = report.database_rows_to_change != 0
        || report.rollout_files_to_change != 0
        || report.duplicate_session_meta_lines != 0
        || report.missing_session_index_entries != 0;
    if !apply || !needs_change {
        return Ok(report);
    }

    let target = active_assignment(paths)?;
    let backup_path = backup(paths, "history-merge")?;
    update_database(paths, &target)?;
    rewrite_sessions(paths, &target)?;
    rebuild_session_index(paths)?;

    let verified = inspect(paths)?;
    if verified.database_rows_to_change != 0
        || verified.rollout_files_to_change != 0
        || verified.duplicate_session_meta_lines != 0
        || verified.missing_session_index_entries != 0
    {
        bail!(
            "历史归并后验证失败；修改前备份位于 {}，请先恢复备份",
            backup_path.display()
        );
    }
    report.backup = Some(backup_path);
    Ok(report)
}

pub fn backup(paths: &Paths, label: &str) -> Result<PathBuf> {
    paths.validate()?;
    fs::create_dir_all(&paths.backup_root)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let destination = paths.backup_root.join(format!("{label}-{stamp}"));
    fs::create_dir_all(&destination)?;

    copy_database(&paths.database, &destination.join("state_5.sqlite"))?;
    if paths.session_index.is_file() {
        fs::copy(
            &paths.session_index,
            destination.join("session_index.jsonl"),
        )?;
    }
    for source in rollout_paths(paths) {
        let relative = source.strip_prefix(&paths.codex_home)?;
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
    }
    Ok(destination)
}

pub fn validate_backup(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("历史备份目录不存在：{}", path.display());
    }
    if !path.join("state_5.sqlite").is_file() {
        bail!("备份缺少 state_5.sqlite：{}", path.display());
    }
    Ok(())
}

pub fn restore(paths: &Paths, source: &Path) -> Result<PathBuf> {
    validate_backup(source)?;
    let safety = backup(paths, "pre-history-restore")?;
    copy_database(&source.join("state_5.sqlite"), &paths.database)?;

    let index = source.join("session_index.jsonl");
    if index.is_file() {
        atomic_copy(&index, &paths.session_index)?;
    }
    for directory in ["sessions", "archived_sessions"] {
        let root = source.join(directory);
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(source)?;
            atomic_copy(entry.path(), &paths.codex_home.join(relative))?;
        }
    }
    Ok(safety)
}

pub fn reconcile_session_index(codex_home: &Path) -> Result<usize> {
    let paths = Paths {
        config: codex_home.join("config.toml"),
        database: codex_home.join("state_5.sqlite"),
        session_index: codex_home.join("session_index.jsonl"),
        backup_root: codex_home.join("codex-sync-backups"),
        codex_home: codex_home.to_path_buf(),
    };
    if !paths.database.is_file() {
        return Ok(0);
    }
    let conn = open_database(&paths.database, true)?;
    let columns = thread_columns(&conn)?;
    let database_ids = active_database_ids(&conn, &columns)?;
    let index_ids = read_session_index(&paths.session_index)?
        .into_keys()
        .collect::<HashSet<_>>();
    let missing = database_ids.difference(&index_ids).count();
    drop(conn);
    rebuild_session_index(&paths)?;
    Ok(missing)
}

pub fn print_report(report: &Report) {
    println!("当前 provider：{}", report.current_provider);
    println!(
        "当前 model：{}",
        report.current_model.as_deref().unwrap_or("(未配置)")
    );
    println!("数据库线程：{}", report.total_threads);
    print_counts("数据库 provider 分布", &report.database_counts);
    if !report.model_counts.is_empty() {
        print_counts("数据库 model 分布", &report.model_counts);
    }
    print_counts("会话文件 provider 分布", &report.rollout_counts);
    if !report.rollout_model_counts.is_empty() {
        print_counts("会话文件 model 分布", &report.rollout_model_counts);
    }
    println!(
        "待归并：{} 条数据库记录，{} / {} 个会话文件，{} 条重复 session_meta，缺少 {} 条侧栏索引",
        report.database_rows_to_change,
        report.rollout_files_to_change,
        report.rollout_files,
        report.duplicate_session_meta_lines,
        report.missing_session_index_entries
    );
}

fn print_counts(label: &str, counts: &BTreeMap<String, usize>) {
    println!("{label}：");
    for (name, count) in counts {
        println!("  {name}: {count}");
    }
}

fn active_assignment(paths: &Paths) -> Result<Assignment> {
    let text = fs::read_to_string(&paths.config).context("无法读取 Codex config.toml")?;
    let value: toml::Value = toml::from_str(&text).context("Codex config.toml 格式错误")?;
    let provider = value
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "openai".to_owned());
    let model = value
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    Ok(Assignment { provider, model })
}

fn assignment_differs(provider: &str, model: Option<&str>, target: &Assignment) -> bool {
    provider != target.provider
        || target
            .model
            .as_deref()
            .is_some_and(|expected| model != Some(expected))
}

fn display_value(value: &str) -> String {
    if value.is_empty() {
        "(empty)".into()
    } else {
        value.into()
    }
}

fn open_database(path: &Path, readonly: bool) -> Result<Connection> {
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(DB_BUSY_TIMEOUT)?;
    Ok(conn)
}

fn thread_columns(conn: &Connection) -> Result<HashSet<String>> {
    let mut statement = conn.prepare("PRAGMA table_info(threads)")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(names)
}

fn grouped_counts(conn: &Connection, column: &str) -> Result<BTreeMap<String, usize>> {
    let sql = format!(
        "SELECT COALESCE({column}, ''), COUNT(*) FROM threads GROUP BY {column} ORDER BY {column}"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
    })?;
    let mut counts = BTreeMap::new();
    for row in rows {
        let (value, count) = row?;
        counts.insert(display_value(&value), count);
    }
    Ok(counts)
}

fn count_database_changes(
    conn: &Connection,
    columns: &HashSet<String>,
    target: &Assignment,
) -> Result<usize> {
    if columns.contains("model")
        && let Some(model) = &target.model
    {
        return Ok(conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE model_provider IS NULL OR model_provider<>?1 OR model IS NULL OR model<>?2",
            params![target.provider, model],
            |row| row.get(0),
        )?);
    }
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE model_provider IS NULL OR model_provider<>?1",
        [&target.provider],
        |row| row.get(0),
    )?)
}

fn active_database_ids(conn: &Connection, columns: &HashSet<String>) -> Result<HashSet<String>> {
    let sql = if columns.contains("archived") {
        "SELECT id FROM threads WHERE archived=0"
    } else {
        "SELECT id FROM threads"
    };
    let mut statement = conn.prepare(sql)?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn rollout_paths(paths: &Paths) -> Vec<PathBuf> {
    ["sessions", "archived_sessions"]
        .into_iter()
        .flat_map(|directory| {
            WalkDir::new(paths.codex_home.join(directory))
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry
                            .path()
                            .extension()
                            .is_some_and(|value| value == "jsonl")
                })
                .map(|entry| entry.into_path())
        })
        .collect()
}

fn session_records(paths: &Paths) -> Result<Vec<SessionRecord>> {
    let mut records = Vec::new();
    for path in rollout_paths(paths) {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("无法读取会话文件：{}", path.display()))?;
        let mut record = None;
        let mut session_meta_lines = 0;
        for line in text.lines() {
            let value: Value = serde_json::from_str(line)
                .with_context(|| format!("会话 JSONL 格式错误：{}", path.display()))?;
            if value.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            session_meta_lines += 1;
            if record.is_some() {
                continue;
            }
            let payload = value
                .get("payload")
                .and_then(Value::as_object)
                .with_context(|| format!("session_meta 缺少 payload：{}", path.display()))?;
            let provider = payload
                .get("model_provider")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let model = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            record = Some(SessionRecord {
                path: path.clone(),
                provider,
                model,
                session_meta_lines: 0,
            });
        }
        if let Some(mut record) = record {
            record.session_meta_lines = session_meta_lines;
            records.push(record);
        }
    }
    Ok(records)
}

fn read_session_index(path: &Path) -> Result<BTreeMap<String, Value>> {
    let mut entries = BTreeMap::new();
    if !path.is_file() {
        return Ok(entries);
    }
    for line in fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let value: Value = serde_json::from_str(line).context("session_index.jsonl 格式错误")?;
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            entries.insert(id.to_owned(), value);
        }
    }
    Ok(entries)
}

fn update_database(paths: &Paths, target: &Assignment) -> Result<()> {
    let mut conn = open_database(&paths.database, false)?;
    let columns = thread_columns(&conn)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if columns.contains("model")
        && let Some(model) = &target.model
    {
        transaction.execute(
            "UPDATE threads SET model_provider=?1, model=?2 WHERE model_provider IS NULL OR model_provider<>?1 OR model IS NULL OR model<>?2",
            params![target.provider, model],
        )?;
    } else {
        transaction.execute(
            "UPDATE threads SET model_provider=?1 WHERE model_provider IS NULL OR model_provider<>?1",
            [&target.provider],
        )?;
    }
    transaction.commit()?;
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)")?;
    Ok(())
}

fn rewrite_sessions(paths: &Paths, target: &Assignment) -> Result<usize> {
    let records = session_records(paths)?;
    let mut updated = 0;
    for record in records {
        if !assignment_differs(&record.provider, record.model.as_deref(), target)
            && record.session_meta_lines == 1
        {
            continue;
        }
        rewrite_session_meta(&record.path, target)?;
        updated += 1;
    }
    Ok(updated)
}

fn rewrite_session_meta(path: &Path, target: &Assignment) -> Result<()> {
    let text = fs::read_to_string(path)?;
    let mut output = String::with_capacity(text.len());
    let mut changed = false;
    for segment in text.split_inclusive('\n') {
        let (line, ending) = if let Some(line) = segment.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = segment.strip_suffix('\n') {
            (line, "\n")
        } else {
            (segment, "")
        };
        let mut value: Value = serde_json::from_str(line)?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if changed {
                continue;
            }
            if let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) {
                payload.insert(
                    "model_provider".into(),
                    Value::String(target.provider.clone()),
                );
                if let Some(model) = &target.model {
                    payload.insert("model".into(), Value::String(model.clone()));
                }
                output.push_str(&serde_json::to_string(&value)?);
                output.push_str(ending);
                changed = true;
                continue;
            }
        }
        output.push_str(segment);
    }
    if !changed {
        bail!("会话文件缺少 session_meta：{}", path.display());
    }
    atomic_write(path, output.as_bytes())
}

fn rebuild_session_index(paths: &Paths) -> Result<()> {
    let conn = open_database(&paths.database, true)?;
    let columns = thread_columns(&conn)?;
    let title = if columns.contains("title") {
        "COALESCE(title, id)"
    } else {
        "id"
    };
    let updated = if columns.contains("updated_at") {
        "COALESCE(strftime('%Y-%m-%dT%H:%M:%SZ', CASE WHEN updated_at>100000000000 THEN updated_at/1000 ELSE updated_at END, 'unixepoch'), '')"
    } else {
        "''"
    };
    let archived = if columns.contains("archived") {
        " WHERE archived=0"
    } else {
        ""
    };
    let sql = format!("SELECT id, {title}, {updated} FROM threads{archived}");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut existing = read_session_index(&paths.session_index)?;
    let mut merged = Vec::new();
    for (id, title, updated_at) in rows {
        let entry = existing
            .remove(&id)
            .unwrap_or_else(|| json!({"id": id, "thread_name": title, "updated_at": updated_at}));
        merged.push(entry);
    }
    merged.extend(existing.into_values());
    merged.sort_by_key(index_sort_key);

    let mut output = String::new();
    for entry in merged {
        output.push_str(&serde_json::to_string(&entry)?);
        output.push('\n');
    }
    atomic_write(&paths.session_index, output.as_bytes())
}

fn index_sort_key(value: &Value) -> (String, String) {
    (
        value
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )
}

fn copy_database(source: &Path, destination: &Path) -> Result<()> {
    let source_conn = open_database(source, true)?;
    let mut destination_conn = Connection::open(destination)?;
    destination_conn.busy_timeout(DB_BUSY_TIMEOUT)?;
    let backup = Backup::new(&source_conn, &mut destination_conn)?;
    backup.run_to_completion(128, Duration::from_millis(25), None)?;
    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    atomic_write(destination, &bytes)
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("history");
    let temporary = path.with_file_name(format!(".{file_name}.codex-sync-{nonce}.tmp"));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Result<(PathBuf, Paths)> {
        let root = std::env::temp_dir().join(format!(
            "codex-sync-history-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let codex = root.join(".codex");
        fs::create_dir_all(codex.join("sessions/2026/07/19"))?;
        fs::write(
            codex.join("config.toml"),
            "model_provider = \"current\"\nmodel = \"gpt-current\"\n",
        )?;
        let conn = Connection::open(codex.join("state_5.sqlite"))?;
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, model_provider TEXT, model TEXT, archived INTEGER, updated_at INTEGER);
             INSERT INTO threads VALUES ('old', 'Old chat', 'old-provider', 'gpt-old', 0, 1700000000);
             INSERT INTO threads VALUES ('current', 'Current chat', 'current', 'gpt-current', 0, 1700000010);",
        )?;
        drop(conn);
        fs::write(
            codex.join("sessions/2026/07/19/rollout-old.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"model_provider\":\"old-provider\",\"model\":\"gpt-old\"}}\n{\"type\":\"message\",\"payload\":{\"text\":\"keep me\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"model_provider\":\"older-provider\",\"model\":\"gpt-older\"}}\n",
        )?;
        fs::write(
            codex.join("session_index.jsonl"),
            "{\"id\":\"current\",\"thread_name\":\"Current chat\",\"updated_at\":\"2023-11-14T22:13:30Z\"}\n",
        )?;
        let paths = Paths::resolve(Some(codex))?;
        Ok((root, paths))
    }

    #[test]
    fn merge_updates_database_session_meta_and_index_with_backup() -> Result<()> {
        let (root, paths) = fixture()?;
        let preview = merge(&paths, false)?;
        assert_eq!(preview.database_rows_to_change, 1);
        assert_eq!(preview.rollout_files_to_change, 1);
        assert_eq!(preview.duplicate_session_meta_lines, 1);
        assert_eq!(preview.missing_session_index_entries, 1);

        let applied = merge(&paths, true)?;
        assert!(applied.backup.as_ref().is_some_and(|path| path.is_dir()));
        let verified = inspect(&paths)?;
        assert_eq!(verified.database_rows_to_change, 0);
        assert_eq!(verified.rollout_files_to_change, 0);
        assert_eq!(verified.duplicate_session_meta_lines, 0);
        assert_eq!(verified.missing_session_index_entries, 0);
        let conn = open_database(&paths.database, true)?;
        let assignment: (String, String) = conn.query_row(
            "SELECT model_provider, model FROM threads WHERE id='old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(assignment, ("current".into(), "gpt-current".into()));
        let rollout = fs::read_to_string(
            paths
                .codex_home
                .join("sessions/2026/07/19/rollout-old.jsonl"),
        )?;
        assert!(rollout.contains("\"model_provider\":\"current\""));
        assert!(rollout.contains("keep me"));
        assert_eq!(rollout.matches("session_meta").count(), 1);
        assert!(fs::read_to_string(&paths.session_index)?.contains("\"id\":\"old\""));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn restore_recovers_previous_assignments() -> Result<()> {
        let (root, paths) = fixture()?;
        let applied = merge(&paths, true)?;
        let backup_path = applied.backup.context("missing backup")?;
        let safety = restore(&paths, &backup_path)?;
        assert!(safety.is_dir());
        let restored = inspect(&paths)?;
        assert_eq!(restored.database_rows_to_change, 1);
        assert_eq!(restored.rollout_files_to_change, 1);
        assert_eq!(restored.missing_session_index_entries, 1);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn missing_environment_is_rejected() {
        assert!(Paths::resolve(Some(PathBuf::from("/missing-codex-history"))).is_err());
    }

    #[test]
    fn missing_provider_uses_codex_builtin_openai_provider() -> Result<()> {
        let (root, paths) = fixture()?;
        fs::write(&paths.config, "model = \"gpt-current\"\n")?;
        let report = inspect(&paths)?;
        assert_eq!(report.current_provider, "openai");
        assert_eq!(report.database_rows_to_change, 2);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
