//! HTTP client adapter over the transport-independent snapshot core.

use super::core::{self, FileInfo, Manifest, PullReport, SnapshotSource};
use crate::config::Config;
use anyhow::{Result, bail};
use reqwest::header::AUTHORIZATION;

struct HttpSource {
    client: reqwest::Client,
    base: String,
    authorization: String,
}

impl HttpSource {
    fn new(peer: &str, shared_key: &str) -> Self {
        let base = if peer.starts_with("http") {
            peer.trim_end_matches('/').to_owned()
        } else {
            format!("http://{}", peer.trim_end_matches('/'))
        };
        Self {
            client: reqwest::Client::new(),
            base,
            authorization: format!("Bearer {shared_key}"),
        }
    }
}

impl SnapshotSource for HttpSource {
    async fn manifest(&self) -> Result<Manifest> {
        Ok(self
            .client
            .get(format!("{}/api/manifest", self.base))
            .header(AUTHORIZATION, &self.authorization)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn read(&self, file: &FileInfo) -> Result<Vec<u8>> {
        let url = format!(
            "{}/api/file/{}",
            self.base,
            file.path
                .split('/')
                .map(url_encode)
                .collect::<Vec<_>>()
                .join("/")
        );
        let mut response = self
            .client
            .get(url)
            .header(AUTHORIZATION, &self.authorization)
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|size| size != file.size)
        {
            bail!("远端文件大小不符合清单：{}", file.path);
        }
        let mut bytes = Vec::with_capacity(file.size as usize);
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > file.size as usize {
                bail!("远端文件超过清单大小：{}", file.path);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

pub async fn pull(cfg: &Config, peer: &str, overwrite: bool) -> Result<PullReport> {
    core::import_from(cfg, &HttpSource::new(peer, &cfg.shared_key), overwrite).await
}

fn url_encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
