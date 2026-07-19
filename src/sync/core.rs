//! Transport-independent snapshot creation, validation, and incremental import.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub modified: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub node_id: String,
    pub node_name: String,
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Default, Serialize)]
pub struct PullReport {
    pub downloaded: usize,
    pub skipped: usize,
    pub conflicts: usize,
}

#[allow(async_fn_in_trait)]
/// Trait implemented by HTTP, directory, or future object-storage sources.
pub trait SnapshotSource {
    async fn manifest(&self) -> Result<Manifest>;
    async fn read(&self, file: &FileInfo) -> Result<Vec<u8>>;
}

pub struct DirectorySource {
    files_root: PathBuf,
    manifest: Manifest,
}

impl DirectorySource {
    pub fn open(snapshot: &Path) -> Result<Self> {
        let manifest = read_manifest(snapshot)?;
        Ok(Self {
            files_root: snapshot.join("files"),
            manifest,
        })
    }
}

impl SnapshotSource for DirectorySource {
    async fn manifest(&self) -> Result<Manifest> {
        Ok(self.manifest.clone())
    }

    async fn read(&self, file: &FileInfo) -> Result<Vec<u8>> {
        let path = safe_path(&self.files_root, &file.path)?;
        fs::read(&path).with_context(|| format!("快照文件缺失：{}", path.display()))
    }
}

pub fn manifest(cfg: &Config) -> Result<Manifest> {
    if !cfg.sync_dir.exists() {
        fs::create_dir_all(&cfg.sync_dir)?;
    }
    manifest_tree(
        &cfg.sync_dir,
        cfg.node_id(),
        cfg.node_name.clone(),
        cfg.max_file_size_bytes,
        cfg.max_files,
        |relative| !excluded(cfg, relative),
    )
}

pub fn manifest_tree(
    root: &Path,
    node_id: String,
    node_name: String,
    max_file_size: u64,
    max_files: usize,
    include: impl Fn(&Path) -> bool,
) -> Result<Manifest> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        if !include(relative) {
            continue;
        }
        if files.len() >= max_files {
            bail!("同步文件数量超过上限 {max_files}");
        }
        let metadata = entry.metadata()?;
        if metadata.len() > max_file_size {
            bail!(
                "同步文件超过大小上限 {}：{}",
                max_file_size,
                entry.path().display()
            );
        }
        let bytes = fs::read(entry.path())?;
        files.push(FileInfo {
            path: relative.to_string_lossy().replace('\\', "/"),
            size: metadata.len(),
            modified: metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            hash: blake3::hash(&bytes).to_hex().to_string(),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Manifest {
        node_id,
        node_name,
        files,
    })
}

/// Imports a source using one validation, incremental, and conflict policy.
pub async fn import_from<S: SnapshotSource>(
    cfg: &Config,
    source: &S,
    overwrite: bool,
) -> Result<PullReport> {
    let remote = source.manifest().await?;
    validate_manifest(
        &cfg.sync_dir,
        &remote,
        cfg.max_file_size_bytes,
        cfg.max_files,
    )?;
    if remote.node_id == cfg.node_id() {
        bail!("这是本机导出的快照，无需导入");
    }
    let local_map: HashMap<_, _> = manifest(cfg)?
        .files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect();
    let mut report = PullReport::default();

    for file in &remote.files {
        if local_map
            .get(&file.path)
            .is_some_and(|local| local.hash == file.hash)
        {
            report.skipped += 1;
            continue;
        }
        let bytes = source.read(file).await?;
        verify_bytes(file, &bytes, cfg.max_file_size_bytes)?;
        let target = safe_path(&cfg.sync_dir, &file.path)?;
        let target = if target.exists() && !overwrite {
            report.conflicts += 1;
            conflict_path(&target, &remote.node_name)
        } else {
            report.downloaded += 1;
            target
        };
        atomic_write(&target, &bytes)?;
    }
    Ok(report)
}

pub fn publish_snapshot(cfg: &Config, snapshot: &Path) -> Result<Manifest> {
    let manifest = manifest(cfg)?;
    let files_root = snapshot.join("files");
    fs::create_dir_all(&files_root)?;
    let published_manifest = snapshot.join(MANIFEST_FILE);
    if published_manifest.exists() {
        fs::remove_file(&published_manifest)?;
    }
    for file in &manifest.files {
        let source = safe_path(&cfg.sync_dir, &file.path)?;
        let target = safe_path(&files_root, &file.path)?;
        atomic_copy(&source, &target)?;
    }
    atomic_write(&published_manifest, &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

pub fn read_manifest(snapshot: &Path) -> Result<Manifest> {
    Ok(serde_json::from_slice(&fs::read(
        snapshot.join(MANIFEST_FILE),
    )?)?)
}

pub fn validate_manifest(
    target_root: &Path,
    manifest: &Manifest,
    max_file_size: u64,
    max_files: usize,
) -> Result<()> {
    if manifest.files.len() > max_files {
        bail!("远端同步文件数量超过上限 {max_files}");
    }
    let mut paths = HashSet::new();
    for file in &manifest.files {
        safe_path(target_root, &file.path)?;
        if file.size > max_file_size {
            bail!("远端文件超过大小上限：{}", file.path);
        }
        if file.hash.len() != 64 || !file.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("远端文件哈希格式无效：{}", file.path);
        }
        if !paths.insert(&file.path) {
            bail!("远端清单包含重复路径：{}", file.path);
        }
    }
    Ok(())
}

pub fn verify_snapshot(snapshot: &Path, manifest: &Manifest, max_file_size: u64) -> Result<()> {
    let files_root = snapshot.join("files");
    for file in &manifest.files {
        let bytes = fs::read(safe_path(&files_root, &file.path)?)?;
        verify_bytes(file, &bytes, max_file_size)?;
    }
    Ok(())
}

pub fn verify_bytes(file: &FileInfo, bytes: &[u8], max_file_size: u64) -> Result<()> {
    if bytes.len() as u64 > max_file_size
        || bytes.len() as u64 != file.size
        || blake3::hash(bytes).to_hex().as_str() != file.hash
    {
        bail!("同步文件校验失败：{}", file.path);
    }
    Ok(())
}

pub fn read_local_file(cfg: &Config, relative: &str) -> Result<Vec<u8>> {
    let file = safe_path(&cfg.sync_dir, relative)?;
    let metadata = fs::symlink_metadata(&file)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("拒绝非普通文件");
    }
    if metadata.len() > cfg.max_file_size_bytes {
        bail!("文件超过大小上限");
    }
    Ok(fs::read(file)?)
}

pub fn conflict_path(target: &Path, node_name: &str) -> PathBuf {
    target.with_extension(format!(
        "{}.conflict-{}",
        target
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file"),
        sanitize_name(node_name)
    ))
}

pub fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("非法路径");
    }
    Ok(root.join(path))
}

pub fn atomic_copy(source: &Path, target: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    atomic_write(target, &bytes)
}

pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!("拒绝覆盖符号链接：{}", target.display());
    }
    let temporary = temporary_path(target);
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("写入 {}", temporary.display()))?;
    output.write_all(bytes)?;
    output.sync_all()?;
    fs::rename(temporary, target)?;
    Ok(())
}

fn excluded(cfg: &Config, relative: &Path) -> bool {
    relative.components().any(|component| {
        cfg.exclude
            .iter()
            .any(|item| component.as_os_str() == item.as_str())
    })
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn temporary_path(target: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    target.with_extension(format!("codex-sync-{nonce}-{}-tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemorySource {
        manifest: Manifest,
        bytes: Vec<u8>,
    }

    impl SnapshotSource for MemorySource {
        async fn manifest(&self) -> Result<Manifest> {
            Ok(self.manifest.clone())
        }

        async fn read(&self, _file: &FileInfo) -> Result<Vec<u8>> {
            Ok(self.bytes.clone())
        }
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-sync-core-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn safe_path_rejects_traversal_and_absolute_paths() {
        let root = Path::new("/tmp/root");
        assert!(safe_path(root, "sessions/chat.jsonl").is_ok());
        assert!(safe_path(root, "../auth.json").is_err());
        assert!(safe_path(root, "/etc/passwd").is_err());
        assert!(safe_path(root, "sessions/./chat.jsonl").is_ok());
    }

    #[test]
    fn manifest_validation_rejects_untrusted_entries() {
        let file = FileInfo {
            path: "sessions/a.jsonl".into(),
            size: 1,
            modified: 0,
            hash: "0".repeat(64),
        };
        let valid = Manifest {
            node_id: "node".into(),
            node_name: "node".into(),
            files: vec![file.clone()],
        };
        assert!(validate_manifest(Path::new("/tmp/root"), &valid, 10, 1).is_ok());

        let mut traversal = valid.clone();
        traversal.files[0].path = "../auth.json".into();
        assert!(validate_manifest(Path::new("/tmp/root"), &traversal, 10, 1).is_err());

        let mut duplicate = valid.clone();
        duplicate.files.push(file);
        assert!(validate_manifest(Path::new("/tmp/root"), &duplicate, 10, 2).is_err());

        let mut oversized = valid;
        oversized.files[0].size = 11;
        assert!(validate_manifest(Path::new("/tmp/root"), &oversized, 10, 1).is_err());
    }

    #[tokio::test]
    async fn any_snapshot_source_gets_the_same_conflict_policy() -> Result<()> {
        let root = temp_dir();
        fs::create_dir_all(root.join("sessions"))?;
        fs::write(root.join("sessions/chat.jsonl"), b"local")?;
        let cfg = Config {
            node_name: "local".into(),
            shared_key: "shared-key".into(),
            sync_dir: root.clone(),
            ..Config::default()
        };
        let bytes = b"remote".to_vec();
        let source = MemorySource {
            manifest: Manifest {
                node_id: "remote-node".into(),
                node_name: "memory-adapter".into(),
                files: vec![FileInfo {
                    path: "sessions/chat.jsonl".into(),
                    size: bytes.len() as u64,
                    modified: 0,
                    hash: blake3::hash(&bytes).to_hex().to_string(),
                }],
            },
            bytes,
        };

        let report = import_from(&cfg, &source, false).await?;
        assert_eq!(report.conflicts, 1);
        assert_eq!(fs::read(root.join("sessions/chat.jsonl"))?, b"local");
        assert_eq!(
            fs::read(root.join("sessions/chat.jsonl.conflict-memory-adapter"))?,
            b"remote"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
