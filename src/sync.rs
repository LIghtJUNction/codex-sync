use crate::config::Config;
use anyhow::{Context, Result, bail};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

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

pub fn conflict_path(target: &Path, node_name: &str) -> PathBuf {
    target.with_extension(format!(
        "{}.conflict-{}",
        target
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("file"),
        sanitize_name(node_name)
    ))
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn excluded(cfg: &Config, rel: &Path) -> bool {
    rel.components()
        .any(|c| cfg.exclude.iter().any(|x| c.as_os_str() == x.as_str()))
}

pub fn safe_path(root: &Path, rel: &str) -> Result<PathBuf> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("非法路径");
    }
    Ok(root.join(path))
}

pub fn manifest(cfg: &Config) -> Result<Manifest> {
    let mut files = Vec::new();
    if !cfg.sync_dir.exists() {
        fs::create_dir_all(&cfg.sync_dir)?;
    }
    for entry in WalkDir::new(&cfg.sync_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&cfg.sync_dir)?;
        if excluded(cfg, rel) {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        let meta = entry.metadata()?;
        let modified = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        files.push(FileInfo {
            path: rel.to_string_lossy().replace('\\', "/"),
            size: meta.len(),
            modified,
            hash: blake3::hash(&bytes).to_hex().to_string(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Manifest {
        node_id: cfg.node_id(),
        node_name: cfg.node_name.clone(),
        files,
    })
}

pub async fn pull(cfg: &Config, peer: &str, overwrite: bool) -> Result<PullReport> {
    let base = if peer.starts_with("http") {
        peer.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", peer.trim_end_matches('/'))
    };
    let client = reqwest::Client::new();
    let auth = format!("Bearer {}", cfg.shared_key);
    let remote: Manifest = client
        .get(format!("{base}/api/manifest"))
        .header(AUTHORIZATION, &auth)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let local = manifest(cfg)?;
    let local_map: std::collections::HashMap<_, _> = local
        .files
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();
    let mut report = PullReport::default();
    for file in remote.files {
        if local_map
            .get(&file.path)
            .is_some_and(|f| f.hash == file.hash)
        {
            report.skipped += 1;
            continue;
        }
        let target = safe_path(&cfg.sync_dir, &file.path)?;
        if target.exists() && !overwrite {
            let conflict = conflict_path(&target, &remote.node_name);
            download(&client, &auth, &base, &file.path, &conflict).await?;
            report.conflicts += 1;
        } else {
            download(&client, &auth, &base, &file.path, &target).await?;
            report.downloaded += 1;
        }
    }
    Ok(report)
}

async fn download(
    client: &reqwest::Client,
    auth: &str,
    base: &str,
    rel: &str,
    target: &Path,
) -> Result<()> {
    let url = format!(
        "{base}/api/file/{}",
        rel.split('/').map(url_encode).collect::<Vec<_>>().join("/")
    );
    let bytes = client
        .get(url)
        .header(AUTHORIZATION, auth)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension("codex-sync-tmp");
    fs::write(&temp, &bytes).with_context(|| format!("写入 {}", temp.display()))?;
    fs::rename(temp, target)?;
    Ok(())
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_rejects_traversal_and_absolute_paths() {
        let root = Path::new("/tmp/root");
        assert!(safe_path(root, "sessions/chat.jsonl").is_ok());
        assert!(safe_path(root, "../auth.json").is_err());
        assert!(safe_path(root, "/etc/passwd").is_err());
        assert!(safe_path(root, "sessions/./chat.jsonl").is_ok());
    }
}
