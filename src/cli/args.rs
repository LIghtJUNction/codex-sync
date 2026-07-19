use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "codex-sync",
    version,
    about = "在局域网内同步 Codex 聊天记录和配置"
)]
pub(crate) struct Cli {
    #[arg(long, global = true)]
    pub(crate) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
    /// 修复本机因 provider、模型或登录方式切换而隐藏的历史记录
    History {
        #[command(subcommand)]
        command: HistoryCommand,
        /// 覆盖 Codex 数据目录（用于测试或非默认安装）
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// 通过 SSH 将本机聊天记录单向合并到远端 Codex
    Ssh {
        #[command(subcommand)]
        command: SshCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum UsbCommand {
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
pub(crate) enum SmbCommand {
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
pub(crate) enum RelayCommand {
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
pub(crate) enum CcSwitchCommand {
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
pub(crate) enum HistoryCommand {
    /// 显示当前 provider/model 与本地历史记录的归属情况
    Status,
    /// 检查数据库、会话文件和侧栏索引；默认只读
    Doctor {
        /// 应用经过完整校验的原子修复
        #[arg(long)]
        apply: bool,
        /// 从经过校验的备份 JSONL 原子恢复缺失 rollout；可重复指定
        #[arg(long = "restore-missing", value_name = "BACKUP_JSONL")]
        restore_missing: Vec<PathBuf>,
        /// 原子删除指定 rollout 中首条之外的重复 session_meta
        #[arg(long = "dedupe-session-meta", value_name = "ROLLOUT_JSONL")]
        dedupe_session_meta: Option<PathBuf>,
    },
    /// 将本机所有历史记录归入当前 provider/model；默认只预览
    #[command(alias = "sync")]
    Merge {
        /// 实际执行；未提供时不修改任何文件
        #[arg(long)]
        apply: bool,
    },
    /// 手动备份历史数据库、侧栏索引和会话元数据
    Backup,
    /// 从指定备份恢复；默认只显示将要恢复的备份
    Restore {
        /// history 备份目录
        backup: PathBuf,
        /// 实际执行恢复
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SshCommand {
    /// 将本机记录单向推送并合并到远端；绝不拉取远端记录
    Push {
        /// SSH 主机或 ~/.ssh/config 中的别名
        host: String,
        /// 本机 Codex 数据目录
        #[arg(long)]
        codex_home: Option<PathBuf>,
        /// 将远端活动历史镜像为本机集合；默认仅增量合并
        #[arg(long)]
        mirror: bool,
    },
    /// 接收并合并已上传的安全包（供远端调用）
    #[command(hide = true)]
    Receive {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        codex_home: Option<PathBuf>,
        #[arg(long)]
        mirror: bool,
    },
}
