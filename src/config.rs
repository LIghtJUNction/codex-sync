use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub node_name: String,
    pub listen: String,
    pub discovery_port: u16,
    pub sync_dir: PathBuf,
    pub shared_key: String,
    pub exclude: Vec<String>,
    pub auto_pull_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            node_name: hostname::get()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            listen: "0.0.0.0:8787".into(),
            discovery_port: 8788,
            sync_dir: home.join(".codex"),
            shared_key: "请修改为所有电脑相同的随机密钥".into(),
            exclude: vec![
                "auth.json".into(),
                "credentials.json".into(),
                "logs".into(),
                "cache".into(),
                "tmp".into(),
                "codex-sync-backups".into(),
                ".DS_Store".into(),
            ],
            auto_pull_seconds: 0,
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("codex-sync/config.toml")
    }

    pub fn init(path: &Path, force: bool) -> Result<()> {
        if path.exists() && !force {
            bail!("配置已存在；如需覆盖请添加 --force");
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, toml::to_string_pretty(&Self::default())?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("无法读取 {}，请先运行 codex-sync init", path.display()))?;
        let cfg: Self = toml::from_str(&text).context("配置格式错误")?;
        if cfg.shared_key.starts_with("请修改") {
            bail!("请先修改配置中的 shared_key");
        }
        Ok(cfg)
    }

    pub fn node_id(&self) -> String {
        blake3::hash(format!("{}:{}", self.node_name, self.shared_key).as_bytes()).to_hex()[..12]
            .to_string()
    }
}
