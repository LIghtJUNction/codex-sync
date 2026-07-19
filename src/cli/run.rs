use super::args::{
    CcSwitchCommand, Cli, Command, HistoryCommand, RelayCommand, SmbCommand, SshCommand, UsbCommand,
};
use crate::{cc_switch, config::Config, discovery, history, sync, web};
use anyhow::Result;
use clap::Parser;

pub async fn run() -> Result<()> {
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

    if let Command::History {
        command,
        codex_home,
    } = &cli.command
    {
        let paths = history::Paths::resolve(codex_home.clone())?;
        match command {
            HistoryCommand::Status => history::print_report(&history::inspect(&paths)?),
            HistoryCommand::Doctor {
                apply,
                restore_missing,
                dedupe_session_meta,
            } => {
                let report = history::doctor(
                    &paths,
                    *apply,
                    restore_missing,
                    dedupe_session_meta.as_deref(),
                )?;
                history::print_doctor_report(&report);
                if !*apply {
                    println!("以上为只读诊断；添加 --apply 可原子修复侧栏索引。");
                }
            }
            HistoryCommand::Merge { apply } => {
                let report = history::merge(&paths, *apply)?;
                history::print_report(&report);
                if *apply {
                    if let Some(backup) = report.backup {
                        println!("本地历史合并完成；备份：{}", backup.display());
                    }
                } else {
                    println!("以上为预览；添加 --apply 后才会修改本地历史。");
                }
            }
            HistoryCommand::Backup => {
                let backup = history::backup(&paths, "manual")?;
                println!("历史备份已创建：{}", backup.display());
            }
            HistoryCommand::Restore { backup, apply } => {
                if *apply {
                    let safety = history::restore(&paths, backup)?;
                    println!("历史已从 {} 恢复", backup.display());
                    println!("恢复前安全备份：{}", safety.display());
                } else {
                    history::validate_backup(backup)?;
                    println!("将从 {} 恢复；添加 --apply 后才会执行。", backup.display());
                }
            }
        }
        return Ok(());
    }

    if let Command::Ssh { command } = &cli.command {
        match command {
            SshCommand::Push {
                host,
                codex_home,
                mirror,
            } => {
                let report = sync::ssh::push(host, codex_home.clone(), *mirror)?;
                println!(
                    "SSH 单向同步完成：新增/更新 {}，跳过 {}，移除远端独有 {}，冲突 {}，数据库变更 {}，侧栏索引补齐 {}",
                    report.copied,
                    report.skipped,
                    report.removed,
                    report.conflicts,
                    report.database_rows,
                    report.index_entries_added
                );
                println!("远端备份：{}", report.backup);
            }
            SshCommand::Receive {
                bundle,
                codex_home,
                mirror,
            } => {
                let report = sync::ssh::receive(bundle, codex_home.clone(), *mirror)?;
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
            let report = sync::http::pull(&config, &peer, overwrite).await?;
            println!(
                "下载 {}，跳过 {}，冲突 {}",
                report.downloaded, report.skipped, report.conflicts
            );
        }
        Command::Status => {
            let manifest = sync::core::manifest(&config)?;
            println!("节点：{}", config.node_name);
            println!("目录：{}", config.sync_dir.display());
            println!("文件：{}", manifest.files.len());
            println!("Web：http://{}", config.listen);
        }
        Command::Usb { command } => run_directory_command(&config, command, "快照已导出").await?,
        Command::Smb { command } => run_smb_command(&config, command).await?,
        Command::Relay { command } => run_relay_command(&config, command).await?,
        Command::CcSwitch { .. }
        | Command::History { .. }
        | Command::Ssh { .. }
        | Command::Init { .. } => unreachable!(),
    }
    Ok(())
}

async fn run_directory_command(cfg: &Config, command: UsbCommand, label: &str) -> Result<()> {
    match command {
        UsbCommand::Export { destination } => {
            let path = sync::directory::export(cfg, &destination)?;
            println!("{label}：{}", path.display());
        }
        UsbCommand::Import { source, overwrite } => {
            print_pull_report(
                "导入",
                sync::directory::import(cfg, &source, overwrite).await?,
            );
        }
        UsbCommand::List { source } => print_snapshots(sync::directory::list(&source)?),
    }
    Ok(())
}

async fn run_smb_command(cfg: &Config, command: SmbCommand) -> Result<()> {
    match command {
        SmbCommand::Push { share } => {
            let path = sync::directory::export(cfg, &share)?;
            println!("快照已推送到 SMB：{}", path.display());
        }
        SmbCommand::Pull { share, overwrite } => {
            print_pull_report(
                "拉取",
                sync::directory::import(cfg, &share, overwrite).await?,
            );
        }
        SmbCommand::List { share } => print_snapshots(sync::directory::list(&share)?),
    }
    Ok(())
}

async fn run_relay_command(cfg: &Config, command: RelayCommand) -> Result<()> {
    match command {
        RelayCommand::Push { directory } => {
            let path = sync::directory::export(cfg, &directory)?;
            println!("快照已发布：{}", path.display());
        }
        RelayCommand::Pull {
            directory,
            overwrite,
        } => print_pull_report(
            "获取",
            sync::directory::import(cfg, &directory, overwrite).await?,
        ),
        RelayCommand::List { directory } => print_snapshots(sync::directory::list(&directory)?),
    }
    Ok(())
}

fn print_pull_report(verb: &str, report: sync::core::PullReport) {
    println!(
        "{verb} {}，跳过 {}，冲突 {}",
        report.downloaded, report.skipped, report.conflicts
    );
}

fn print_snapshots(snapshots: Vec<sync::directory::SnapshotInfo>) {
    for snapshot in snapshots {
        println!(
            "{}\t{}\t{} 个文件\t{}",
            snapshot.node_name,
            snapshot.node_id,
            snapshot.files,
            snapshot.path.display()
        );
    }
}
