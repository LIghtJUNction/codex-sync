//! SSH transport adapter. Codex-aware merging lives in the sibling `codex` module.

use super::codex;
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub use codex::MergeReport as SshReport;

pub fn push(host: &str, codex_home: Option<PathBuf>, mirror: bool) -> Result<SshReport> {
    validate_host(host)?;
    let home = dirs::home_dir().context("无法确定本机主目录")?;
    let codex_home = codex_home.unwrap_or_else(|| home.join(".codex"));
    if !codex_home.is_dir() {
        bail!("本机 Codex 目录不存在：{}", codex_home.display());
    }
    ensure_command("ssh")?;
    ensure_command("scp")?;

    let transfer_id = format!("{}-{}", hostname::get()?.to_string_lossy(), now());
    let temporary = std::env::temp_dir().join(format!("codex-sync-ssh-{transfer_id}"));
    let bundle = temporary.join("bundle");
    fs::create_dir_all(&bundle)?;
    let result = (|| -> Result<SshReport> {
        codex::create_bundle(&codex_home, &bundle)?;
        let remote_relative = format!(".codex-sync/incoming/{transfer_id}");
        run(
            Command::new("ssh")
                .arg(host)
                .arg("mkdir")
                .arg("-p")
                .arg(&remote_relative),
            "创建远端接收目录",
        )?;
        run(
            Command::new("scp")
                .arg("-r")
                .arg(format!("{}/.", bundle.display()))
                .arg(format!("{host}:{remote_relative}/")),
            "上传本机会话包",
        )?;

        let mirror_flag = if mirror { " --mirror" } else { "" };
        let remote_command = format!(
            "bin=$(command -v codex-sync || true); if [ -z \"$bin\" ] && [ -x \"$HOME/.cargo/bin/codex-sync\" ]; then bin=\"$HOME/.cargo/bin/codex-sync\"; fi; if [ -z \"$bin\" ]; then echo '远端未安装 codex-sync 0.4.0+' >&2; exit 127; fi; \"$bin\" ssh receive --bundle \"$HOME/{remote_relative}\"{mirror_flag}"
        );
        let output = Command::new("ssh")
            .arg(host)
            .arg(remote_command)
            .output()
            .context("调用远端 codex-sync")?;
        if !output.status.success() {
            bail!(
                "远端合并失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout)?;
        let serialized = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CODEX_SYNC_REPORT="))
            .context("远端未返回合并报告")?;
        let report = serde_json::from_str(serialized)?;

        let cleanup = format!("rm -rf \"$HOME/{remote_relative}\"");
        run(
            Command::new("ssh").arg(host).arg(cleanup),
            "清理远端临时接收包",
        )?;
        Ok(report)
    })();
    let _ = fs::remove_dir_all(temporary);
    result
}

pub fn receive(
    bundle: &std::path::Path,
    codex_home: Option<PathBuf>,
    mirror: bool,
) -> Result<SshReport> {
    let home = dirs::home_dir().context("无法确定远端主目录")?;
    let codex_home = codex_home.unwrap_or_else(|| home.join(".codex"));
    codex::apply_bundle(bundle, &codex_home, mirror)
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._-".contains(character))
    {
        bail!("非法 SSH 主机名");
    }
    Ok(())
}

fn ensure_command(name: &str) -> Result<()> {
    if Command::new(name).arg("-V").output().is_err() {
        bail!("系统缺少 {name} 命令");
    }
    Ok(())
}

fn run(command: &mut Command, action: &str) -> Result<()> {
    let status = command.status().with_context(|| action.to_owned())?;
    if !status.success() {
        bail!("{action}失败，退出码：{status}");
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_validation_blocks_shell_syntax() {
        assert!(validate_host("lty").is_ok());
        assert!(validate_host("user@example.local").is_ok());
        assert!(validate_host("lty;rm -rf /").is_err());
    }
}
