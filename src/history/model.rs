use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug)]
pub struct Paths {
    pub(super) codex_home: PathBuf,
    pub(super) config: PathBuf,
    pub(super) database: PathBuf,
    pub(super) session_index: PathBuf,
    pub(super) backup_root: PathBuf,
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
    pub stale_session_index_entries: usize,
    pub missing_rollout_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub database_integrity_ok: bool,
    pub database_integrity_messages: Vec<String>,
    pub total_threads: usize,
    pub sidebar_threads: usize,
    pub rollout_files: usize,
    pub invalid_rollout_files: usize,
    pub missing_session_meta_files: usize,
    pub duplicate_session_meta_lines: usize,
    pub orphan_rollout_files: usize,
    pub missing_rollout_files: usize,
    pub invalid_session_index_lines: usize,
    pub duplicate_session_index_ids: usize,
    pub missing_session_index_entries: usize,
    pub stale_session_index_entries: usize,
    pub clean: bool,
    pub repaired: bool,
    pub repaired_session_index: bool,
    pub restored_rollout_files: usize,
    pub deduplicated_session_meta_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
}

#[derive(Debug)]
pub(super) struct Assignment {
    pub(super) provider: String,
    pub(super) model: Option<String>,
}

#[derive(Debug)]
pub(super) struct SessionRecord {
    pub(super) path: PathBuf,
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) session_meta_lines: usize,
}

#[derive(Debug)]
pub(super) struct SidebarThread {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) updated_at: String,
    pub(super) rollout_path: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub(super) struct IndexDiagnostics {
    pub(super) entries: BTreeMap<String, Value>,
    pub(super) invalid_lines: usize,
    pub(super) duplicate_ids: usize,
}

#[derive(Debug, Default)]
pub(super) struct RolloutDiagnostics {
    pub(super) files: usize,
    pub(super) invalid_files: usize,
    pub(super) missing_session_meta_files: usize,
    pub(super) duplicate_session_meta_lines: usize,
    pub(super) thread_ids: Vec<String>,
}

#[derive(Debug)]
pub(super) struct RolloutRecovery {
    pub(super) target: PathBuf,
    pub(super) content: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct SessionMetaRepair {
    pub(super) target: PathBuf,
    pub(super) original: Vec<u8>,
    pub(super) repaired: Vec<u8>,
}
