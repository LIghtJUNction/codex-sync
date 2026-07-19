use crate::{
    config::Config,
    sync::{self, Manifest, PullReport},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const ROOT: &str = "codex-sync";
const MANIFEST: &str = "manifest.json";

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub node_id: String,
    pub node_name: String,
    pub files: usize,
    pub path: PathBuf,
}

pub fn export(cfg: &Config, destination: &Path) -> Result<PathBuf> {
    if !destination.is_dir() {
        bail!("目标目录不存在：{}", destination.display());
    }
    let manifest = sync::manifest(cfg)?;
    let snapshot = destination.join(ROOT).join(&manifest.node_id);
    let files_root = snapshot.join("files");
    fs::create_dir_all(&files_root)?;

    // Hide an older manifest while the new snapshot is being copied. Readers only
    // consider directories with a complete manifest importable.
    let published_manifest = snapshot.join(MANIFEST);
    if published_manifest.exists() {
        fs::remove_file(&published_manifest)?;
    }

    for file in &manifest.files {
        let source = sync::safe_path(&cfg.sync_dir, &file.path)?;
        let target = sync::safe_path(&files_root, &file.path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &target)
            .with_context(|| format!("复制 {} 到 {}", source.display(), target.display()))?;
    }
    let temp = snapshot.join("manifest.json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(&manifest)?)?;
    fs::rename(temp, published_manifest)?;
    Ok(snapshot)
}

pub fn import(cfg: &Config, source: &Path, overwrite: bool) -> Result<PullReport> {
    let snapshot = resolve_snapshot(source)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(snapshot.join(MANIFEST))?)?;
    if manifest.node_id == cfg.node_id() {
        bail!("这是本机导出的快照，无需导入");
    }
    let files_root = snapshot.join("files");
    let local = sync::manifest(cfg)?;
    let local_map: HashMap<_, _> = local
        .files
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();
    let mut report = PullReport::default();

    for file in manifest.files {
        if local_map
            .get(&file.path)
            .is_some_and(|f| f.hash == file.hash)
        {
            report.skipped += 1;
            continue;
        }
        let source_file = sync::safe_path(&files_root, &file.path)?;
        let bytes = fs::read(&source_file)
            .with_context(|| format!("快照文件缺失：{}", source_file.display()))?;
        if blake3::hash(&bytes).to_hex().as_str() != file.hash {
            bail!("快照文件校验失败：{}", file.path);
        }
        let target = sync::safe_path(&cfg.sync_dir, &file.path)?;
        let target = if target.exists() && !overwrite {
            report.conflicts += 1;
            sync::conflict_path(&target, &manifest.node_name)
        } else {
            report.downloaded += 1;
            target
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = target.with_extension("codex-sync-tmp");
        fs::write(&temp, bytes)?;
        fs::rename(temp, target)?;
    }
    Ok(report)
}

pub fn list(source: &Path) -> Result<Vec<SnapshotInfo>> {
    let root = if source.join(ROOT).is_dir() {
        source.join(ROOT)
    } else {
        source.to_path_buf()
    };
    let mut result = Vec::new();
    if !root.is_dir() {
        bail!("未找到 codex-sync 快照目录");
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let manifest_path = path.join(MANIFEST);
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
        result.push(SnapshotInfo {
            node_id: manifest.node_id,
            node_name: manifest.node_name,
            files: manifest.files.len(),
            path,
        });
    }
    result.sort_by(|a, b| a.node_name.cmp(&b.node_name));
    Ok(result)
}

fn resolve_snapshot(source: &Path) -> Result<PathBuf> {
    if source.join(MANIFEST).is_file() {
        return Ok(source.to_path_buf());
    }
    let snapshots = list(source)?;
    match snapshots.as_slice() {
        [one] => Ok(one.path.clone()),
        [] => bail!("没有找到可导入的快照"),
        _ => bail!("发现多个快照，请指定其中一个快照目录"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-sync-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_usb_directory_is_rejected() {
        let cfg = Config {
            shared_key: "test-key".into(),
            ..Config::default()
        };
        assert!(export(&cfg, Path::new("/definitely/not/a/real/usb/path")).is_err());
    }

    #[test]
    fn exports_and_imports_a_verified_snapshot() -> Result<()> {
        let root = temp_dir("roundtrip");
        let source = root.join("computer-a");
        let destination = root.join("computer-b");
        let usb = root.join("usb");
        fs::create_dir_all(source.join("sessions"))?;
        fs::create_dir_all(&destination)?;
        fs::create_dir_all(&usb)?;
        fs::write(source.join("sessions/chat.jsonl"), b"hello codex")?;
        fs::write(source.join("auth.json"), b"must not leave computer")?;

        let a = Config {
            node_name: "computer-a".into(),
            shared_key: "same-key".into(),
            sync_dir: source,
            ..Config::default()
        };
        let mut b = a.clone();
        b.node_name = "computer-b".into();
        b.sync_dir = destination.clone();

        export(&a, &usb)?;
        let report = import(&b, &usb, false)?;
        assert_eq!(report.downloaded, 1);
        assert_eq!(
            fs::read(destination.join("sessions/chat.jsonl"))?,
            b"hello codex"
        );
        assert!(!destination.join("auth.json").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
