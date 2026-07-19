mod cc_switch;
mod config;
mod discovery;
mod ssh_sync;
mod sync;
mod usb;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "codex-sync",
    version,
    about = "在局域网内同步 Codex 聊天记录和配置"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 创建默认配置文件
    Init {
        #[arg(long)]
        force: bool,
    },
    /// 启动同步节点及 Web 界面
    Serve,
    /// 发现局域网中的其他节点
    Peers {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// 从指定节点拉取新文件（默认不会覆盖冲突文件）
    Pull {
        peer: String,
        #[arg(long)]
        overwrite: bool,
    },
    /// 查看本地同步状态
    Status,
    /// 通过 U 盘或移动硬盘离线中转
    Usb {
        #[command(subcommand)]
        command: UsbCommand,
    },
    /// 通过已挂载的 SMB 共享目录中转
    Smb {
        #[command(subcommand)]
        command: SmbCommand,
    },
    /// 通过任意共享或云同步目录中转（NFS、WebDAV、Syncthing、云盘等）
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    /// 检查或合并本机 CC-Switch 的 Codex 历史记录
    CcSwitch {
        #[command(subcommand)]
        command: CcSwitchCommand,
        /// 覆盖 Codex 数据目录（用于测试或非默认安装）
        #[arg(long)]
        codex_home: Option<PathBuf>,
        /// 覆盖 CC-Switch 数据目录
        #[arg(long)]
        cc_switch_home: Option<PathBuf>,
    },
    /// 通过 SSH 将本机聊天记录单向合并到远端 Codex
    Ssh {
        #[command(subcommand)]
        command: SshCommand,
    },
}

#[derive(Subcommand)]
enum UsbCommand {
    /// 将本机快照导出到移动存储目录
    Export {
        /// U 盘挂载目录，例如 /Volumes/MYUSB
        destination: PathBuf,
    },
    /// 从移动存储目录导入快照
    Import {
        /// 导出时生成的快照目录，或 U 盘挂载目录
        source: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// 列出移动存储目录里的快照
    List {
        /// U 盘挂载目录
        source: PathBuf,
    },
}

#[derive(Subcommand)]
enum SmbCommand {
    /// 将本机快照推送到 SMB 共享目录
    Push {
        /// 已挂载的 SMB 目录，例如 /Volumes/codex-sync
        share: PathBuf,
    },
    /// 从 SMB 共享目录拉取快照
    Pull {
        /// SMB 共享根目录，或其中一个快照目录
        share: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// 列出 SMB 共享目录中的快照
    List {
        /// 已挂载的 SMB 目录
        share: PathBuf,
    },
}

#[derive(Subcommand)]
enum RelayCommand {
    /// 将本机快照发布到中转目录
    Push {
        /// 已挂载或自动同步的目录
        directory: PathBuf,
    },
    /// 从中转目录获取快照
    Pull {
        /// 中转根目录，或其中一个设备快照目录
        directory: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// 列出中转目录中的设备快照
    List {
        /// 已挂载或自动同步的目录
        directory: PathBuf,
    },
}

#[derive(Subcommand)]
enum CcSwitchCommand {
    /// 显示当前账号及各历史记录桶的数量
    Status,
    /// 将所有历史记录归入当前登录账号；默认只预览
    Merge {
        /// 实际执行；未提供时不修改任何文件
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum SshCommand {
    /// 将本机记录单向推送并合并到远端；绝不拉取远端记录
    Push {
        /// SSH 主机或 ~/.ssh/config 中的别名
        host: String,
        /// 本机 Codex 数据目录
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// 接收并合并已上传的安全包（供远端调用）
    #[command(hide = true)]
    Receive {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(Config::default_path);
    if let Command::Init { force } = cli.command {
        Config::init(&path, force)?;
        println!("配置已创建：{}", path.display());
        return Ok(());
    }

    if let Command::CcSwitch {
        command,
        codex_home,
        cc_switch_home,
    } = &cli.command
    {
        let paths = cc_switch::Paths::resolve(codex_home.clone(), cc_switch_home.clone())?;
        match command {
            CcSwitchCommand::Status => {
                let report = cc_switch::inspect(&paths)?;
                cc_switch::print_report(&report);
            }
            CcSwitchCommand::Merge { apply } => {
                let report = cc_switch::merge(&paths, *apply)?;
                cc_switch::print_report(&report);
                if *apply {
                    if let Some(backup) = report.backup {
                        println!("合并完成；备份：{}", backup.display());
                    }
                } else {
                    println!("以上为预览。关闭 Codex 和 CC-Switch 后，添加 --apply 执行合并。");
                }
            }
        }
        return Ok(());
    }

    if let Command::Ssh { command } = &cli.command {
        match command {
            SshCommand::Push { host, codex_home } => {
                let report = ssh_sync::push(host, codex_home.clone())?;
                println!(
                    "SSH 单向同步完成：新增 {}，跳过 {}，冲突 {}，数据库新增 {}",
                    report.copied, report.skipped, report.conflicts, report.database_rows
                );
                println!("远端备份：{}", report.backup);
            }
            SshCommand::Receive { bundle, codex_home } => {
                let report = ssh_sync::receive(bundle, codex_home.clone())?;
                println!("CODEX_SYNC_REPORT={}", serde_json::to_string(&report)?);
            }
        }
        return Ok(());
    }

    let config = Config::load(&path)?;
    match cli.command {
        Command::Serve => web::serve(config).await?,
        Command::Peers { seconds } => {
            for peer in discovery::discover(&config, seconds).await? {
                println!("{}\t{}\t{}", peer.name, peer.address, peer.node_id);
            }
        }
        Command::Pull { peer, overwrite } => {
            let report = sync::pull(&config, &peer, overwrite).await?;
            println!(
                "下载 {}，跳过 {}，冲突 {}",
                report.downloaded, report.skipped, report.conflicts
            );
        }
        Command::Status => {
            let manifest = sync::manifest(&config)?;
            println!("节点：{}", config.node_name);
            println!("目录：{}", config.sync_dir.display());
            println!("文件：{}", manifest.files.len());
            println!("Web：http://{}", config.listen);
        }
        Command::Usb { command } => match command {
            UsbCommand::Export { destination } => {
                let path = usb::export(&config, &destination)?;
                println!("快照已导出：{}", path.display());
            }
            UsbCommand::Import { source, overwrite } => {
                let report = usb::import(&config, &source, overwrite)?;
                println!(
                    "导入 {}，跳过 {}，冲突 {}",
                    report.downloaded, report.skipped, report.conflicts
                );
            }
            UsbCommand::List { source } => {
                for snapshot in usb::list(&source)? {
                    println!(
                        "{}\t{}\t{} 个文件\t{}",
                        snapshot.node_name,
                        snapshot.node_id,
                        snapshot.files,
                        snapshot.path.display()
                    );
                }
            }
        },
        Command::Smb { command } => match command {
            SmbCommand::Push { share } => {
                let path = usb::export(&config, &share)?;
                println!("快照已推送到 SMB：{}", path.display());
            }
            SmbCommand::Pull { share, overwrite } => {
                let report = usb::import(&config, &share, overwrite)?;
                println!(
                    "拉取 {}，跳过 {}，冲突 {}",
                    report.downloaded, report.skipped, report.conflicts
                );
            }
            SmbCommand::List { share } => {
                for snapshot in usb::list(&share)? {
                    println!(
                        "{}\t{}\t{} 个文件\t{}",
                        snapshot.node_name,
                        snapshot.node_id,
                        snapshot.files,
                        snapshot.path.display()
                    );
                }
            }
        },
        Command::Relay { command } => match command {
            RelayCommand::Push { directory } => {
                let path = usb::export(&config, &directory)?;
                println!("快照已发布：{}", path.display());
            }
            RelayCommand::Pull {
                directory,
                overwrite,
            } => {
                let report = usb::import(&config, &directory, overwrite)?;
                println!(
                    "获取 {}，跳过 {}，冲突 {}",
                    report.downloaded, report.skipped, report.conflicts
                );
            }
            RelayCommand::List { directory } => {
                for snapshot in usb::list(&directory)? {
                    println!(
                        "{}\t{}\t{} 个文件\t{}",
                        snapshot.node_name,
                        snapshot.node_id,
                        snapshot.files,
                        snapshot.path.display()
                    );
                }
            }
        },
        Command::CcSwitch { .. } => unreachable!(),
        Command::Ssh { .. } => unreachable!(),
        Command::Init { .. } => unreachable!(),
    }
    Ok(())
}
