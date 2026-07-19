use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

mod model;
mod output;
mod storage;

use model::{
    Assignment, IndexDiagnostics, RolloutDiagnostics, RolloutRecovery, SessionMetaRepair,
    SessionRecord, SidebarThread,
};
pub use model::{DoctorReport, Paths, Report};
pub use output::{print_doctor_report, print_report};
use storage::{atomic_copy, atomic_write, copy_database, open_database};

const MAX_RECOVERY_FILE_SIZE: u64 = 1024 * 1024 * 1024;

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
    let sidebar_threads = sidebar_threads(&conn, &columns)?;
    let missing_rollout_files = sidebar_threads
        .iter()
        .filter(|thread| !rollout_exists(&paths.codex_home, thread.rollout_path.as_deref()))
        .count();
    let database_ids = indexable_ids(&paths.codex_home, &sidebar_threads);
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
        stale_session_index_entries: index_ids.difference(&database_ids).count(),
        missing_rollout_files,
        backup: None,
    })
}

pub fn doctor(
    paths: &Paths,
    apply: bool,
    restore_missing: &[PathBuf],
    dedupe_session_meta: Option<&Path>,
) -> Result<DoctorReport> {
    let report = inspect_doctor(paths)?;
    if !apply {
        if !restore_missing.is_empty() || dedupe_session_meta.is_some() {
            bail!("恢复或去重参数必须与 --apply 一起使用");
        }
        return Ok(report);
    }
    if !report.database_integrity_ok {
        bail!("数据库完整性检查失败；doctor 未修改任何数据");
    }
    if report.invalid_rollout_files != 0
        || report.missing_session_meta_files != 0
        || report.invalid_session_index_lines != 0
    {
        bail!("存在无法安全自动修复的 JSONL；doctor 未修改任何数据");
    }
    let recoveries = prepare_rollout_recoveries(paths, restore_missing)?;
    let session_repair = dedupe_session_meta
        .map(|path| prepare_session_meta_repair(paths, path))
        .transpose()?;
    let needs_index_repair = report.missing_session_index_entries != 0
        || report.stale_session_index_entries != 0
        || report.duplicate_session_index_ids != 0;
    if !needs_index_repair && recoveries.is_empty() && session_repair.is_none() {
        return Ok(report);
    }

    let original = fs::read(&paths.session_index).ok();
    let backup = backup_doctor_state(paths, &report, original.as_deref(), session_repair.as_ref())?;
    let mut installed = Vec::new();
    let mut session_replaced = false;
    let index_touched = needs_index_repair || !recoveries.is_empty();
    let repair = (|| -> Result<DoctorReport> {
        let mut connection = open_database(&paths.database, false)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for recovery in &recoveries {
            if recovery.target.exists() {
                bail!(
                    "待恢复的 rollout 已被其他进程创建：{}",
                    recovery.target.display()
                );
            }
            atomic_write(&recovery.target, &recovery.content)?;
            installed.push(recovery.target.clone());
        }
        if let Some(repair) = &session_repair {
            if fs::read(&repair.target)? != repair.original {
                bail!(
                    "rollout 在诊断后发生变化，拒绝覆盖：{}",
                    repair.target.display()
                );
            }
            atomic_write(&repair.target, &repair.repaired)?;
            session_replaced = true;
            validate_recovery_rollout(&repair.target, &repair.repaired)?;
        }
        if index_touched {
            let content = render_session_index(paths, &transaction)?;
            atomic_write(&paths.session_index, &content)?;
        }
        let verified = inspect_doctor(paths)?;
        if (index_touched
            && (verified.invalid_session_index_lines != 0
                || verified.duplicate_session_index_ids != 0
                || verified.missing_session_index_entries != 0
                || verified.stale_session_index_entries != 0))
            || verified.missing_rollout_files + recoveries.len() != report.missing_rollout_files
        {
            bail!("rollout/侧栏索引原子替换后的复检失败");
        }
        transaction.commit()?;
        Ok(verified)
    })();

    match repair {
        Ok(mut verified) => {
            verified.repaired = true;
            verified.repaired_session_index = index_touched;
            verified.restored_rollout_files = recoveries.len();
            verified.deduplicated_session_meta_files = usize::from(session_repair.is_some());
            verified.backup = Some(backup);
            Ok(verified)
        }
        Err(error) => {
            for target in installed.iter().rev() {
                let _ = fs::remove_file(target);
            }
            if session_replaced && let Some(repair) = &session_repair {
                atomic_write(&repair.target, &repair.original)
                    .context("修复失败，且恢复原 rollout 失败")?;
            }
            if index_touched {
                restore_original_index(paths, original.as_deref())
                    .context("修复失败，且恢复原侧栏索引失败")?;
            }
            Err(error.context("修复失败；已原子恢复所有已替换文件"))
        }
    }
}

fn inspect_doctor(paths: &Paths) -> Result<DoctorReport> {
    paths.validate()?;
    let connection = open_database(&paths.database, true)?;
    let database_integrity_messages = database_integrity(&connection)?;
    let database_integrity_ok = database_integrity_messages.as_slice() == ["ok"];
    let columns = thread_columns(&connection)?;
    let total_threads =
        connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
    let sidebar = sidebar_threads(&connection, &columns)?;
    let database_ids = all_database_ids(&connection)?;
    let indexable = indexable_ids(&paths.codex_home, &sidebar);
    let missing_rollout_files = sidebar
        .iter()
        .filter(|thread| !rollout_exists(&paths.codex_home, thread.rollout_path.as_deref()))
        .count();
    let index = inspect_session_index(&paths.session_index)?;
    let index_ids = index.entries.keys().cloned().collect::<HashSet<_>>();
    let rollouts = inspect_rollouts(paths)?;
    let orphan_rollout_files = rollouts
        .thread_ids
        .iter()
        .filter(|id| !database_ids.contains(*id))
        .count();

    let mut report = DoctorReport {
        database_integrity_ok,
        database_integrity_messages,
        total_threads,
        sidebar_threads: sidebar.len(),
        rollout_files: rollouts.files,
        invalid_rollout_files: rollouts.invalid_files,
        missing_session_meta_files: rollouts.missing_session_meta_files,
        duplicate_session_meta_lines: rollouts.duplicate_session_meta_lines,
        orphan_rollout_files,
        missing_rollout_files,
        invalid_session_index_lines: index.invalid_lines,
        duplicate_session_index_ids: index.duplicate_ids,
        missing_session_index_entries: indexable.difference(&index_ids).count(),
        stale_session_index_entries: index_ids.difference(&indexable).count(),
        clean: false,
        repaired: false,
        repaired_session_index: false,
        restored_rollout_files: 0,
        deduplicated_session_meta_files: 0,
        backup: None,
    };
    report.clean = doctor_is_clean(&report);
    Ok(report)
}

fn doctor_is_clean(report: &DoctorReport) -> bool {
    report.database_integrity_ok
        && report.invalid_rollout_files == 0
        && report.missing_session_meta_files == 0
        && report.duplicate_session_meta_lines == 0
        && report.orphan_rollout_files == 0
        && report.missing_rollout_files == 0
        && report.invalid_session_index_lines == 0
        && report.duplicate_session_index_ids == 0
        && report.missing_session_index_entries == 0
        && report.stale_session_index_entries == 0
}

pub fn merge(paths: &Paths, apply: bool) -> Result<Report> {
    let mut report = inspect(paths)?;
    let needs_change = report.database_rows_to_change != 0
        || report.rollout_files_to_change != 0
        || report.duplicate_session_meta_lines != 0
        || report.missing_session_index_entries != 0
        || report.stale_session_index_entries != 0;
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
        || verified.stale_session_index_entries != 0
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
    let database_ids = indexable_ids(&paths.codex_home, &sidebar_threads(&conn, &columns)?);
    let index_ids = read_session_index(&paths.session_index)?
        .into_keys()
        .collect::<HashSet<_>>();
    let missing = database_ids.difference(&index_ids).count();
    drop(conn);
    rebuild_session_index(&paths)?;
    Ok(missing)
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

fn database_integrity(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("PRAGMA quick_check")?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn all_database_ids(connection: &Connection) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("SELECT id FROM threads")?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
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

fn sidebar_threads(conn: &Connection, columns: &HashSet<String>) -> Result<Vec<SidebarThread>> {
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
    let rollout_path = if columns.contains("rollout_path") {
        "rollout_path"
    } else {
        "NULL"
    };
    let source = if columns.contains("source") {
        "COALESCE(source, '')"
    } else {
        "''"
    };
    let thread_source = if columns.contains("thread_source") {
        "COALESCE(thread_source, '')"
    } else {
        "''"
    };
    let model = if columns.contains("model") {
        "COALESCE(model, '')"
    } else {
        "''"
    };
    let archived = if columns.contains("archived") {
        " WHERE archived=0"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, {title}, {updated}, {rollout_path}, {source}, {thread_source}, {model} FROM threads{archived}"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut threads = Vec::new();
    for row in rows {
        let (id, title, updated_at, rollout_path, source, thread_source, model) = row?;
        if is_technical_thread(&source, &thread_source, &model, &title) {
            continue;
        }
        threads.push(SidebarThread {
            id,
            title,
            updated_at,
            rollout_path: rollout_path.map(PathBuf::from),
        });
    }
    Ok(threads)
}

fn is_subagent_source(source: &str) -> bool {
    serde_json::from_str::<Value>(source)
        .ok()
        .is_some_and(|value| value.get("subagent").is_some())
}

fn is_technical_thread(source: &str, thread_source: &str, model: &str, title: &str) -> bool {
    is_subagent_source(source)
        || thread_source == "subagent"
        || (model == "codex-auto-review" && title.trim().is_empty())
}

fn rollout_exists(codex_home: &Path, rollout_path: Option<&Path>) -> bool {
    let Some(path) = rollout_path else {
        return true;
    };
    if path.is_absolute() {
        path.is_file()
    } else {
        codex_home.join(path).is_file()
    }
}

fn indexable_ids(codex_home: &Path, threads: &[SidebarThread]) -> HashSet<String> {
    threads
        .iter()
        .filter(|thread| rollout_exists(codex_home, thread.rollout_path.as_deref()))
        .map(|thread| thread.id.clone())
        .collect()
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

fn inspect_rollouts(paths: &Paths) -> Result<RolloutDiagnostics> {
    let mut diagnostics = RolloutDiagnostics::default();
    for path in rollout_paths(paths) {
        diagnostics.files += 1;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("无法读取会话文件：{}", path.display()))?;
        let mut invalid = false;
        let mut session_meta_lines = 0usize;
        let mut first_id = None;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(_) => {
                    invalid = true;
                    continue;
                }
            };
            if value.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            session_meta_lines += 1;
            if first_id.is_none() {
                first_id = value
                    .get("payload")
                    .and_then(|payload| payload.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        diagnostics.invalid_files += usize::from(invalid);
        diagnostics.missing_session_meta_files += usize::from(session_meta_lines == 0);
        diagnostics.duplicate_session_meta_lines += session_meta_lines.saturating_sub(1);
        if let Some(id) = first_id {
            diagnostics.thread_ids.push(id);
        }
    }
    Ok(diagnostics)
}

fn prepare_rollout_recoveries(paths: &Paths, sources: &[PathBuf]) -> Result<Vec<RolloutRecovery>> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let connection = open_database(&paths.database, true)?;
    let columns = thread_columns(&connection)?;
    let missing = sidebar_threads(&connection, &columns)?
        .into_iter()
        .filter(|thread| !rollout_exists(&paths.codex_home, thread.rollout_path.as_deref()))
        .collect::<Vec<_>>();
    let mut targets = HashSet::new();
    let mut recoveries = Vec::new();

    for source in sources {
        let metadata = fs::symlink_metadata(source)
            .with_context(|| format!("恢复来源不存在：{}", source.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!("恢复来源不是普通文件：{}", source.display());
        }
        if metadata.len() > MAX_RECOVERY_FILE_SIZE {
            bail!("恢复来源超过大小上限：{}", source.display());
        }
        let content = fs::read(source)?;
        let thread_id = validate_recovery_rollout(source, &content)?;
        let thread = missing
            .iter()
            .find(|thread| thread.id == thread_id)
            .with_context(|| format!("恢复来源不对应任何缺失线程：{}", source.display()))?;
        let relative = thread
            .rollout_path
            .as_deref()
            .context("缺失线程没有 rollout_path，无法自动恢复")?;
        let target = if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            paths.codex_home.join(relative)
        };
        ensure_safe_recovery_target(&paths.codex_home, &target)?;
        if source.file_name() != target.file_name() {
            bail!("恢复来源文件名与数据库 rollout_path 不一致");
        }
        if !targets.insert(target.clone()) {
            bail!("同一缺失 rollout 被重复指定：{}", target.display());
        }
        recoveries.push(RolloutRecovery { target, content });
    }
    Ok(recoveries)
}

fn prepare_session_meta_repair(paths: &Paths, target: &Path) -> Result<SessionMetaRepair> {
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        paths.codex_home.join(target)
    };
    ensure_safe_recovery_target(&paths.codex_home, &target)?;
    let relative = target.strip_prefix(&paths.codex_home)?;
    if !relative.starts_with("sessions") && !relative.starts_with("archived_sessions") {
        bail!("只能去重 sessions 或 archived_sessions 中的 rollout");
    }
    let metadata = fs::symlink_metadata(&target)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("rollout 不是普通文件：{}", target.display());
    }
    if metadata.len() > MAX_RECOVERY_FILE_SIZE {
        bail!("rollout 超过大小上限：{}", target.display());
    }
    let connection = open_database(&paths.database, true)?;
    let database_id = connection
        .query_row(
            "SELECT id FROM threads WHERE rollout_path=?1",
            [target.to_string_lossy().as_ref()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("数据库中找不到 rollout_path：{}", target.display()))?;
    let original = fs::read(&target)?;
    let repaired = deduplicate_session_meta(&target, &original, &database_id)?;
    Ok(SessionMetaRepair {
        target,
        original,
        repaired,
    })
}

fn deduplicate_session_meta(path: &Path, content: &[u8], expected_id: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(content)
        .with_context(|| format!("rollout 不是 UTF-8 JSONL：{}", path.display()))?;
    let mut output = Vec::with_capacity(content.len());
    let mut session_meta_lines = 0usize;
    let mut kept_expected = false;
    for segment in text.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("rollout 包含非法 JSONL：{}", path.display()))?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            session_meta_lines += 1;
            let id = value
                .get("payload")
                .and_then(|payload| payload.get("id"))
                .and_then(Value::as_str)
                .context("session_meta 缺少线程 ID")?;
            if id != expected_id || kept_expected {
                continue;
            }
            kept_expected = true;
        }
        output.extend_from_slice(segment.as_bytes());
    }
    if session_meta_lines < 2 {
        bail!("rollout 没有重复 session_meta，无需修改");
    }
    if !kept_expected {
        bail!("rollout 中没有与数据库线程 ID 一致的 session_meta");
    }
    Ok(output)
}

fn validate_recovery_rollout(source: &Path, content: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(content)
        .with_context(|| format!("恢复来源不是 UTF-8 JSONL：{}", source.display()))?;
    let mut thread_id = None;
    let mut session_meta_lines = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("恢复来源包含非法 JSONL：{}", source.display()))?;
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        session_meta_lines += 1;
        let id = value
            .get("payload")
            .and_then(|payload| payload.get("id"))
            .and_then(Value::as_str)
            .context("恢复来源的 session_meta 缺少线程 ID")?;
        if let Some(existing) = &thread_id
            && existing != id
        {
            bail!("恢复来源包含不同线程的 session_meta");
        }
        thread_id = Some(id.to_owned());
    }
    if session_meta_lines != 1 {
        bail!("恢复来源必须恰好包含一条 session_meta");
    }
    thread_id.context("恢复来源缺少 session_meta")
}

fn ensure_safe_recovery_target(codex_home: &Path, target: &Path) -> Result<()> {
    if !target.starts_with(codex_home)
        || target
            .strip_prefix(codex_home)?
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!(
            "数据库 rollout_path 超出 Codex 数据目录：{}",
            target.display()
        );
    }
    let mut current = codex_home.to_path_buf();
    for component in target.strip_prefix(codex_home)?.components() {
        current.push(component);
        if current
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            bail!("拒绝通过符号链接恢复 rollout：{}", current.display());
        }
    }
    Ok(())
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
    let diagnostics = inspect_session_index(path)?;
    if diagnostics.invalid_lines != 0 {
        bail!("session_index.jsonl 包含非法行");
    }
    Ok(diagnostics.entries)
}

fn inspect_session_index(path: &Path) -> Result<IndexDiagnostics> {
    let mut diagnostics = IndexDiagnostics::default();
    if !path.is_file() {
        return Ok(diagnostics);
    }
    for line in fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            diagnostics.invalid_lines += 1;
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            diagnostics.invalid_lines += 1;
            continue;
        };
        if diagnostics.entries.insert(id.to_owned(), value).is_some() {
            diagnostics.duplicate_ids += 1;
        }
    }
    Ok(diagnostics)
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
    atomic_write(&paths.session_index, &render_session_index(paths, &conn)?)
}

fn render_session_index(paths: &Paths, connection: &Connection) -> Result<Vec<u8>> {
    let columns = thread_columns(connection)?;
    let rows = sidebar_threads(connection, &columns)?;
    let mut existing = read_session_index(&paths.session_index)?;
    let mut merged = Vec::new();
    for thread in rows
        .into_iter()
        .filter(|thread| rollout_exists(&paths.codex_home, thread.rollout_path.as_deref()))
    {
        let entry = existing.remove(&thread.id).unwrap_or_else(|| {
            json!({
                "id": thread.id,
                "thread_name": thread.title,
                "updated_at": thread.updated_at
            })
        });
        merged.push(entry);
    }
    merged.sort_by_key(index_sort_key);

    let mut output = String::new();
    for entry in merged {
        output.push_str(&serde_json::to_string(&entry)?);
        output.push('\n');
    }
    Ok(output.into_bytes())
}

fn backup_doctor_state(
    paths: &Paths,
    report: &DoctorReport,
    original: Option<&[u8]>,
    session_repair: Option<&SessionMetaRepair>,
) -> Result<PathBuf> {
    fs::create_dir_all(&paths.backup_root)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let destination = paths.backup_root.join(format!("history-doctor-{stamp}"));
    fs::create_dir(&destination)?;
    let result = (|| -> Result<()> {
        atomic_write(
            &destination.join("doctor-report.json"),
            &serde_json::to_vec_pretty(report)?,
        )?;
        if let Some(content) = original {
            atomic_write(&destination.join("session_index.jsonl"), content)?;
        } else {
            atomic_write(&destination.join("session-index-was-absent"), b"")?;
        }
        if let Some(repair) = session_repair {
            let relative = repair.target.strip_prefix(&paths.codex_home)?;
            atomic_write(
                &destination.join("rollouts").join(relative),
                &repair.original,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    Ok(destination)
}

fn restore_original_index(paths: &Paths, original: Option<&[u8]>) -> Result<()> {
    if let Some(content) = original {
        atomic_write(&paths.session_index, content)
    } else if paths.session_index.exists() {
        fs::remove_file(&paths.session_index)?;
        Ok(())
    } else {
        Ok(())
    }
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
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, model_provider TEXT, model TEXT, archived INTEGER, updated_at INTEGER, source TEXT);
             INSERT INTO threads VALUES ('old', 'Old chat', 'old-provider', 'gpt-old', 0, 1700000000, 'vscode');
             INSERT INTO threads VALUES ('current', 'Current chat', 'current', 'gpt-current', 0, 1700000010, 'vscode');
             INSERT INTO threads VALUES ('archived', 'Archived chat', 'current', 'gpt-current', 1, 1700000020, 'vscode');
             INSERT INTO threads VALUES ('guardian', 'Guardian', 'current', 'gpt-current', 0, 1700000030, '{\"subagent\":{\"other\":\"guardian\"}}');",
        )?;
        drop(conn);
        fs::write(
            codex.join("sessions/2026/07/19/rollout-old.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"model_provider\":\"old-provider\",\"model\":\"gpt-old\"}}\n{\"type\":\"message\",\"payload\":{\"text\":\"keep me\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"model_provider\":\"older-provider\",\"model\":\"gpt-older\"}}\n",
        )?;
        fs::write(
            codex.join("session_index.jsonl"),
            "{\"id\":\"current\",\"thread_name\":\"Current chat\",\"updated_at\":\"2023-11-14T22:13:30Z\"}\n{\"id\":\"archived\",\"thread_name\":\"Archived chat\",\"updated_at\":\"2023-11-14T22:13:40Z\"}\n{\"id\":\"guardian\",\"thread_name\":\"Guardian\",\"updated_at\":\"2023-11-14T22:13:50Z\"}\n",
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
        assert_eq!(preview.stale_session_index_entries, 2);

        let applied = merge(&paths, true)?;
        assert!(applied.backup.as_ref().is_some_and(|path| path.is_dir()));
        let verified = inspect(&paths)?;
        assert_eq!(verified.database_rows_to_change, 0);
        assert_eq!(verified.rollout_files_to_change, 0);
        assert_eq!(verified.duplicate_session_meta_lines, 0);
        assert_eq!(verified.missing_session_index_entries, 0);
        assert_eq!(verified.stale_session_index_entries, 0);
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
        let index = fs::read_to_string(&paths.session_index)?;
        assert!(index.contains("\"id\":\"old\""));
        assert!(!index.contains("\"id\":\"archived\""));
        assert!(!index.contains("\"id\":\"guardian\""));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn doctor_repairs_only_the_index_and_keeps_a_rollback_copy() -> Result<()> {
        let (root, paths) = fixture()?;
        let preview = doctor(&paths, false, &[], None)?;
        assert!(preview.database_integrity_ok);
        assert_eq!(preview.invalid_rollout_files, 0);
        assert_eq!(preview.duplicate_session_meta_lines, 1);
        assert_eq!(preview.missing_session_index_entries, 1);
        assert_eq!(preview.stale_session_index_entries, 2);
        let original = fs::read(&paths.session_index)?;

        let repaired = doctor(&paths, true, &[], None)?;
        assert!(repaired.repaired);
        assert!(repaired.repaired_session_index);
        assert_eq!(repaired.missing_session_index_entries, 0);
        assert_eq!(repaired.stale_session_index_entries, 0);
        assert!(!repaired.clean);
        let backup = repaired.backup.context("doctor backup missing")?;
        assert_eq!(fs::read(backup.join("session_index.jsonl"))?, original);
        let index = fs::read_to_string(&paths.session_index)?;
        assert!(index.contains("\"id\":\"old\""));
        assert!(index.contains("\"id\":\"current\""));
        assert!(!index.contains("\"id\":\"archived\""));
        assert!(!index.contains("\"id\":\"guardian\""));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn doctor_refuses_invalid_input_without_modifying_it() -> Result<()> {
        let (root, paths) = fixture()?;
        let invalid = b"{not-json}\n";
        fs::write(&paths.session_index, invalid)?;
        assert!(doctor(&paths, true, &[], None).is_err());
        assert_eq!(fs::read(&paths.session_index)?, invalid);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn doctor_atomically_restores_a_valid_missing_rollout() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "codex-sync-doctor-recovery-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let codex = root.join(".codex");
        let target = codex.join("sessions/2026/07/19/rollout-missing.jsonl");
        let source = root.join("backup/rollout-missing.jsonl");
        fs::create_dir_all(source.parent().context("backup parent")?)?;
        fs::create_dir_all(&codex)?;
        fs::write(codex.join("config.toml"), "model_provider = \"current\"\n")?;
        let content = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"missing\",\"model_provider\":\"current\"}}\n{\"type\":\"message\",\"payload\":{\"text\":\"preserved\"}}\n";
        fs::write(&source, content)?;
        let connection = Connection::open(codex.join("state_5.sqlite"))?;
        connection.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, archived INTEGER, updated_at INTEGER, source TEXT, rollout_path TEXT)",
            [],
        )?;
        connection.execute(
            "INSERT INTO threads VALUES ('missing', 'Missing', 0, 1700000000, 'vscode', ?1)",
            [target.to_string_lossy().as_ref()],
        )?;
        drop(connection);
        let paths = Paths::resolve(Some(codex))?;

        let preview = doctor(&paths, false, &[], None)?;
        assert_eq!(preview.missing_rollout_files, 1);
        let repaired = doctor(&paths, true, std::slice::from_ref(&source), None)?;
        assert_eq!(repaired.restored_rollout_files, 1);
        assert_eq!(repaired.missing_rollout_files, 0);
        assert_eq!(repaired.missing_session_index_entries, 0);
        assert_eq!(fs::read(&target)?, content);

        let mut duplicated = content.to_vec();
        duplicated.extend_from_slice(
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"other\",\"model_provider\":\"current\"}}\n",
        );
        fs::write(&target, &duplicated)?;
        let deduplicated = doctor(&paths, true, &[], Some(&target))?;
        assert_eq!(deduplicated.deduplicated_session_meta_files, 1);
        assert!(!deduplicated.repaired_session_index);
        assert_eq!(deduplicated.duplicate_session_meta_lines, 0);
        assert_eq!(fs::read(&target)?, content);
        let backup = deduplicated.backup.context("dedupe backup missing")?;
        assert_eq!(
            fs::read(backup.join("rollouts/sessions/2026/07/19/rollout-missing.jsonl"))?,
            duplicated
        );
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
        assert_eq!(restored.stale_session_index_entries, 2);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn missing_environment_is_rejected() {
        assert!(Paths::resolve(Some(PathBuf::from("/missing-codex-history"))).is_err());
    }

    #[test]
    fn technical_threads_are_excluded_from_the_user_sidebar() {
        assert!(is_technical_thread(
            r#"{"subagent":{"other":"guardian"}}"#,
            "",
            "gpt-current",
            "Guardian"
        ));
        assert!(is_technical_thread(
            "vscode",
            "subagent",
            "gpt-current",
            "Worker"
        ));
        assert!(is_technical_thread("unknown", "", "codex-auto-review", ""));
        assert!(!is_technical_thread(
            "vscode",
            "",
            "codex-auto-review",
            "用户主动创建的审核任务"
        ));
    }

    #[test]
    fn missing_provider_uses_codex_builtin_openai_provider() -> Result<()> {
        let (root, paths) = fixture()?;
        fs::write(&paths.config, "model = \"gpt-current\"\n")?;
        let report = inspect(&paths)?;
        assert_eq!(report.current_provider, "openai");
        assert_eq!(report.database_rows_to_change, 4);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
