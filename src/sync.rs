use crate::config::Config;
use anyhow::{Context, Result, bail};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
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
        if files.len() >= cfg.max_files {
            bail!("同步文件数量超过上限 {}", cfg.max_files);
        }
        let meta = entry.metadata()?;
        if meta.len() > cfg.max_file_size_bytes {
            bail!(
                "同步文件超过大小上限 {}：{}",
                cfg.max_file_size_bytes,
                entry.path().display()
            );
        }
        let bytes = fs::read(entry.path())?;
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
    validate_manifest(cfg, &remote)?;
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
            download(
                &client,
                &auth,
                &base,
                &file,
                &conflict,
                cfg.max_file_size_bytes,
            )
            .await?;
            report.conflicts += 1;
        } else {
            download(
                &client,
                &auth,
                &base,
                &file,
                &target,
                cfg.max_file_size_bytes,
            )
            .await?;
            report.downloaded += 1;
        }
    }
    Ok(report)
}

async fn download(
    client: &reqwest::Client,
    auth: &str,
    base: &str,
    file: &FileInfo,
    target: &Path,
    max_file_size: u64,
) -> Result<()> {
    let url = format!(
        "{base}/api/file/{}",
        file.path
            .split('/')
            .map(url_encode)
            .collect::<Vec<_>>()
            .join("/")
    );
    let mut response = client
        .get(url)
        .header(AUTHORIZATION, auth)
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size > max_file_size || size != file.size)
    {
        bail!("远端文件大小不符合清单：{}", file.path);
    }
    let mut bytes = Vec::with_capacity(file.size.min(max_file_size) as usize);
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > max_file_size as usize {
            bail!("远端文件超过大小上限：{}", file.path);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != file.size || blake3::hash(&bytes).to_hex().as_str() != file.hash {
        bail!("远端文件校验失败：{}", file.path);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        bail!("拒绝覆盖符号链接：{}", target.display());
    }
    let temp = temporary_path(target);
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .with_context(|| format!("写入 {}", temp.display()))?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    fs::rename(temp, target)?;
    Ok(())
}

fn validate_manifest(cfg: &Config, manifest: &Manifest) -> Result<()> {
    if manifest.files.len() > cfg.max_files {
        bail!("远端同步文件数量超过上限 {}", cfg.max_files);
    }
    let mut paths = HashSet::new();
    for file in &manifest.files {
        safe_path(&cfg.sync_dir, &file.path)?;
        if file.size > cfg.max_file_size_bytes {
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

fn temporary_path(target: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    target.with_extension(format!("codex-sync-{nonce}-{}-tmp", std::process::id()))
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

    #[test]
    fn manifest_validation_rejects_untrusted_entries() {
        let cfg = Config {
            sync_dir: PathBuf::from("/tmp/codex-sync-test"),
            max_file_size_bytes: 10,
            max_files: 1,
            ..Config::default()
        };
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
        assert!(validate_manifest(&cfg, &valid).is_ok());

        let mut traversal = valid.clone();
        traversal.files[0].path = "../auth.json".into();
        assert!(validate_manifest(&cfg, &traversal).is_err());

        let mut duplicate = valid.clone();
        duplicate.files.push(file);
        assert!(validate_manifest(&cfg, &duplicate).is_err());

        let mut oversized = valid;
        oversized.files[0].size = 11;
        assert!(validate_manifest(&cfg, &oversized).is_err());
    }
}
