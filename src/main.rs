mod config;
mod discovery;
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(Config::default_path);
    if let Command::Init { force } = cli.command {
        Config::init(&path, force)?;
        println!("配置已创建：{}", path.display());
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
        Command::Init { .. } => unreachable!(),
    }
    Ok(())
}
