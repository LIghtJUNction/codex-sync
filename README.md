# Codex Sync

在同一局域网的多台电脑之间同步 Codex 聊天记录和配置。提供命令行与内置 Web 界面。

## 架构

同步能力分为“通用核心、Codex 语义合并、传输适配器”三层：

```text
HTTP / Web ───────┐
USB / SMB / Relay ├─ SnapshotSource ─ sync::core
                  │                    ├─ 清单、路径与哈希校验
SSH ──────────────┴─ 安全传输包 ──────┼─ 原子读写与冲突策略
                                       └─ sync::codex
                                          ├─ rollout 增量去重
                                          ├─ SQLite 合并
                                          └─ session_index 修复
```

- `sync::core` 是唯一的文件级同步基础实现。Web/HTTP 只负责网络读取，USB、SMB 和 Relay 只负责目录读写；清单验证、增量判断、冲突保留、哈希验证和原子落盘全部复用核心逻辑。
- `sync::codex` 是唯一的 Codex 数据语义实现。SSH 只负责调用系统 `ssh`/`scp` 传输安全包，rollout、SQLite 和侧栏索引的合并由该模块统一完成。
- `SnapshotSource` 是传输来源的 Rust trait。新增 S3、对象存储或其他只读来源时，只需为新类型实现这个 trait 的 `manifest` 与 `read` 方法；新增共享目录类型可直接复用现有目录适配器。

源码采用 Rust 2018+ 的模块布局：`sync.rs`/`sync/`、`cli.rs`/`cli/` 由同名文件声明同名目录中的子模块，不使用 `mod.rs`。`lib.rs` 暴露可复用库，`main.rs` 只负责启动 CLI。

这样传输协议不再各自维护一套同步规则，安全校验和合并行为也不会随适配器增加而分叉。

## 快速开始

```bash
cargo build --release
./target/release/codex-sync init
```

编辑 `~/.config/codex-sync/config.toml`，把 `shared_key` 改成一个随机长字符串；每台电脑使用相同密钥。随后运行：

```bash
./target/release/codex-sync serve
```

`shared_key` 至少需要 32 个字符。建议使用密码管理器生成的随机值；`init` 会在 Unix 系统将配置文件权限设为仅当前用户可读写。默认单文件上限为 256 MiB、单次清单上限为 100,000 个文件；可在配置中按需调整：

```toml
max_file_size_bytes = 268435456
max_files = 100000
```

打开 `http://localhost:8787`，在页面中输入共享密钥（仅保存于当前浏览器会话）。也可以使用命令行：

```bash
codex-sync peers
codex-sync pull 192.168.1.20:8787
codex-sync status
```

## U 盘离线中转

在电脑 A 上导出（macOS 示例）：

```bash
codex-sync usb export /Volumes/MYUSB
```

把 U 盘插到电脑 B，查看并导入：

```bash
codex-sync usb list /Volumes/MYUSB
codex-sync usb import /Volumes/MYUSB
```

如果 U 盘中有多台电脑的快照，`usb list` 会显示每个快照的具体目录，导入时指定该目录即可。导入会验证 BLAKE3 哈希；同名但内容不同的文件默认保存为冲突副本。需要明确覆盖时使用 `usb import PATH --overwrite`。

## SMB 服务器中转

Codex Sync 使用操作系统已经挂载的 SMB 目录，不保存服务器用户名或密码。先通过系统连接共享：

- macOS：访达 → 前往 → 连接服务器，输入 `smb://服务器/codex-sync`，通常挂载到 `/Volumes/codex-sync`。
- Windows：在资源管理器映射网络驱动器，例如 `Z:`。
- Linux：用桌面文件管理器挂载，或将共享挂载到 `/mnt/codex-sync`。

电脑 A 推送快照：

```bash
codex-sync smb push /Volumes/codex-sync
```

电脑 B 查看并拉取：

```bash
codex-sync smb list /Volumes/codex-sync
codex-sync smb pull /Volumes/codex-sync
```

Windows 示例：

```powershell
codex-sync.exe smb push Z:\
codex-sync.exe smb pull Z:\
```

共享中有多个设备快照时，先通过 `smb list` 找到目标快照的完整路径，再将它传给 `smb pull`。默认保留冲突副本；明确覆盖时添加 `--overwrite`。SMB 快照和 U 盘快照使用相同的开放目录格式，可以相互复制。

## 其他中转方式

`relay` 支持任何在操作系统中表现为普通目录的中转服务，包括：

- NFS、AFP 或 NAS 挂载目录
- WebDAV 挂载盘
- Syncthing、Resilio Sync 等点对点同步目录
- Dropbox、OneDrive、iCloud Drive、Google Drive 等桌面同步目录
- 虚拟机共享目录、远程开发共享目录

通用用法：

```bash
# 电脑 A 发布快照
codex-sync relay push /path/to/shared-folder

# 电脑 B 查看及获取
codex-sync relay list /path/to/shared-folder
codex-sync relay pull /path/to/shared-folder
```

例如使用 Syncthing：先让两台电脑同步同一个 `CodexRelay` 文件夹，再分别运行：

```bash
codex-sync relay push ~/Sync/CodexRelay
codex-sync relay pull ~/Sync/CodexRelay
```

例如使用 Windows OneDrive：

```powershell
codex-sync.exe relay push "$env:OneDrive\CodexRelay"
codex-sync.exe relay pull "$env:OneDrive\CodexRelay"
```

使用云同步目录时，请等待云盘客户端完成上传或下载后再执行 `pull`。清单采用最后写入的原子替换方式，未完成的快照不会被识别为可导入快照。

## 合并 CC-Switch 多账号记录

Codex Sync 可以把本机 CC-Switch 不同供应商产生的历史记录统一归入当前登录的 Codex 账号。先预览：

```bash
codex-sync cc-switch status
codex-sync cc-switch merge
```

确认数量后，关闭 Codex 和 CC-Switch，再执行：

```bash
codex-sync cc-switch merge --apply
```

合并会同时更新会话 JSONL 的 `model_provider` 和 `state_5.sqlite` 的记录桶。执行前自动备份到 `~/.codex/codex-sync-backups/cc-switch-merge-时间戳`，该目录默认不会参与设备同步。此操作不读取、复制或修改 API Key、Token、`auth.json` 和供应商认证配置。

## 修复本机多 provider / model 历史

如果切换 API、provider、模型或登录方式后，本机历史文件还在但 Codex 侧栏不再显示，可以使用不依赖 CC-Switch 的 `history` 命令。先预览：

```bash
codex-sync history doctor
codex-sync history status
codex-sync history merge
```

`history doctor` 是只读诊断命令，会检查 SQLite `quick_check`、非法会话 JSONL、缺失或重复的 `session_meta`、孤立或缺失的 rollout、重复/缺失/残留的侧栏索引。它默认永远不修改数据。

只有侧栏索引存在可安全重建的问题时，才可执行：

```bash
codex-sync history doctor --apply
```

普通 `--apply` 不修改 SQLite、rollout 或 provider/model，只重建 `session_index.jsonl`。命令持有 SQLite 写锁，先保存原索引，在同目录写入并 `fsync` 临时文件后原子替换，随即复检；任何错误都会原子恢复原索引。非法 JSONL 和重复 `session_meta` 只会报告，不会被冒险自动修改。

缺失 rollout 已经在备份中定位并人工确认时，可以显式恢复：

```bash
codex-sync history doctor \
  --restore-missing ~/.codex/codex-sync-backups/.../rollout-ID.jsonl \
  --apply
```

恢复前会验证来源是普通文件、大小受限、每行 JSON 合法、恰好包含一条 `session_meta`、线程 ID 和文件名均与数据库中缺失的 `rollout_path` 一致，并拒绝目录穿越或符号链接。目标 rollout 与索引分别使用原子替换，失败时删除新目标并恢复原索引。正式执行前仍建议完全退出 Codex 和 CC-Switch，避免应用在修复完成后用内存中的旧状态再次覆盖索引。

对于已经停止写入、但混入多条 `session_meta` 的单个 rollout，可逐个执行：

```bash
codex-sync history doctor \
  --dedupe-session-meta ~/.codex/sessions/.../rollout-ID.jsonl \
  --apply
```

命令只保留线程数据库中该 `rollout_path` 对应 ID 的元数据行，其他事件行按原字节顺序保留。每次只处理一个文件，修改前保存原文件，提交前重新比较源字节，使用同步落盘后的原子替换；验证或提交失败时恢复原文件。正在写入的当前任务必须等 Codex 完全退出后再处理。

确认待归并数量后执行：

```bash
codex-sync history merge --apply
```

该命令会把 `state_5.sqlite` 的线程归属和会话 JSONL 的 `session_meta` 统一到当前 `config.toml` 中的 `model_provider` / `model`，并补齐 `session_index.jsonl` 缺少的侧栏条目。使用 ChatGPT 登录且没有显式配置 `model_provider` 时，采用 Codex 内置的 `openai` provider。修改前会在线备份数据库、侧栏索引和会话文件，因此 Codex 正在运行时也会等待数据库写锁；若仍繁忙则安全失败，不会跳过备份强行写入。

手动备份和恢复：

```bash
codex-sync history backup
codex-sync history restore ~/.codex/codex-sync-backups/history-merge-时间戳
codex-sync history restore ~/.codex/codex-sync-backups/history-merge-时间戳 --apply
```

恢复默认也只预览，并且正式恢复前会再创建一份 `pre-history-restore` 安全备份。本功能的设计参考了 [GODGOD126/codex-history-sync-tool](https://github.com/GODGOD126/codex-history-sync-tool)，并在 Rust CLI 中扩展为 macOS、Linux 和 Windows 通用实现。

## 默认同步方向

“把本机记录同步到某个目标”始终表示单向的“本机 → 目标”。目标端只作为写入位置，不会自动读取、下载或合并目标端已有的 Codex 记录。只有明确执行 `pull`、`import` 或提出从远端恢复时，才会把远端数据带回本机。

## SSH 单向同步并合并

本机与远端都安装 `codex-sync` 后，只需运行：

```bash
codex-sync ssh push lty
```

`lty` 可以是 `~/.ssh/config` 中的主机别名，也可以是 `user@host`。该命令固定执行“本机 → 远端”：

- 只从本机 `~/.codex` 提取会话、归档会话、附件、索引和 SQLite 线程记录。
- 不读取或下载远端已有聊天记录。
- 使用系统 `ssh`/`scp`，沿用现有 SSH Agent、密钥和主机配置。
- 上传后由远端 `codex-sync` 校验 BLAKE3 清单，再合并进远端 `~/.codex`。
- 默认进行增量合并：只追加本机新增记录，保留远端已有记录，且绝不把远端记录下载回本机。
- 同名 JSONL 会话会按事件行去重合并；同名附件或二进制文件仍保留远端版本并报告冲突。
- 每个 rollout 始终只保留一条 `session_meta`；数据库合并后按线程 ID 自动重建 `session_index.jsonl`，避免记录已入库但侧栏缺失。
- 仅在明确需要让远端完全等同于本机时，使用 `codex-sync ssh push HOST --mirror`；远端独有记录会先备份再移出活动历史。
- 导入的会话自动归入远端当前 `model_provider`。
- 修改前备份远端数据库和索引到 `~/.codex/codex-sync-backups/sync-import-时间戳`。
- 合并完成后自动删除两端临时传输包。

如果远端尚未安装：

```bash
ssh lty 'cargo install codex-sync'
```

## 同步策略与安全

- 默认同步 `~/.codex`，排除 `auth.json`、凭据、日志、缓存和临时文件。
- 所有 API（包括状态、发现和 Web 拉取）均要求共享密钥认证；密钥不会经节点发现广播或写入 U 盘快照。
- LAN 与离线导入均校验清单路径、文件数量、文件大小和 BLAKE3 哈希；下载写入使用同步落盘后的原子替换，并拒绝写入符号链接。
- SMB 账号和密码由操作系统管理，不会写入 Codex Sync 配置或快照。
- 默认只拉取。遇到同名但内容不同的文件时，远端版本保存为 `.conflict-节点名`，不会覆盖本地内容。
- `pull --overwrite` 会覆盖不同内容的本地文件，使用前请自行备份。
- 服务默认监听 `0.0.0.0:8787`，请只在可信局域网使用，并通过系统防火墙限制访问。当前 LAN API 使用共享密钥而非 TLS；跨不可信网络时请使用 SSH、VPN 或受保护的中转目录。

## 当前范围

项目提供生产使用的安全基线：受限数据范围、配置权限、端到端完整性校验、冲突保留、原子发布、SSH 备份/合并，以及 macOS、Linux、Windows CI 门禁。自动后台双向同步、设备审批、TLS 终止与系统服务安装仍需按部署环境单独配置。

## 许可证

本项目采用 [MIT License](LICENSE) 开源。
