//! Thin adapter for snapshots stored in any mounted or synchronized directory.

use super::core::{self, DirectorySource, PullReport};
use crate::config::Config;
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

const ROOT: &str = "codex-sync";

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
    let snapshot = destination.join(ROOT).join(cfg.node_id());
    core::publish_snapshot(cfg, &snapshot)?;
    Ok(snapshot)
}

pub async fn import(cfg: &Config, source: &Path, overwrite: bool) -> Result<PullReport> {
    let snapshot = resolve_snapshot(source)?;
    core::import_from(cfg, &DirectorySource::open(&snapshot)?, overwrite).await
}

pub fn list(source: &Path) -> Result<Vec<SnapshotInfo>> {
    let root = if source.join(ROOT).is_dir() {
        source.join(ROOT)
    } else {
        source.to_path_buf()
    };
    if !root.is_dir() {
        bail!("未找到 codex-sync 快照目录");
    }
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !path.join(core::MANIFEST_FILE).is_file() {
            continue;
        }
        let manifest = core::read_manifest(&path)?;
        snapshots.push(SnapshotInfo {
            node_id: manifest.node_id,
            node_name: manifest.node_name,
            files: manifest.files.len(),
            path,
        });
    }
    snapshots.sort_by(|left, right| left.node_name.cmp(&right.node_name));
    Ok(snapshots)
}

fn resolve_snapshot(source: &Path) -> Result<PathBuf> {
    if source.join(core::MANIFEST_FILE).is_file() {
        return Ok(source.to_path_buf());
    }
    match list(source)?.as_slice() {
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
        assert!(
            export(
                &Config::default(),
                Path::new("/definitely/not/a/real/usb/path")
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn exports_and_imports_a_verified_snapshot() -> Result<()> {
        let root = temp_dir("roundtrip");
        let source = root.join("computer-a");
        let destination = root.join("computer-b");
        let relay = root.join("relay");
        fs::create_dir_all(source.join("sessions"))?;
        fs::create_dir_all(&destination)?;
        fs::create_dir_all(&relay)?;
        fs::write(source.join("sessions/chat.jsonl"), b"hello codex")?;
        fs::write(source.join("auth.json"), b"must not leave computer")?;

        let a = Config {
            node_name: "computer-a".into(),
            shared_key: "same-key".into(),
            sync_dir: source,
            ..Config::default()
        };
        let b = Config {
            node_name: "computer-b".into(),
            sync_dir: destination.clone(),
            ..a.clone()
        };

        export(&a, &relay)?;
        let report = import(&b, &relay, false).await?;
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
