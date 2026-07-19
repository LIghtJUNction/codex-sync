# Codex Sync

在同一局域网的多台电脑之间同步 Codex 聊天记录和配置。提供命令行与内置 Web 界面。

## 快速开始

```bash
cargo build --release
./target/release/codex-sync init
```

编辑 `~/.config/codex-sync/config.toml`，把 `shared_key` 改成一个随机长字符串；每台电脑使用相同密钥。随后运行：

```bash
./target/release/codex-sync serve
```

打开 `http://localhost:8787`。也可以使用命令行：

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

## 默认同步方向

“把本机记录同步到某个目标”始终表示单向的“本机 → 目标”。目标端只作为写入位置，不会自动读取、下载或合并目标端已有的 Codex 记录。只有明确执行 `pull`、`import` 或提出从远端恢复时，才会把远端数据带回本机。

## 同步策略与安全

- 默认同步 `~/.codex`，排除 `auth.json`、凭据、日志、缓存和临时文件。
- API 使用共享密钥认证；密钥不会经节点发现广播或写入 U 盘快照。
- SMB 账号和密码由操作系统管理，不会写入 Codex Sync 配置或快照。
- 默认只拉取。遇到同名但内容不同的文件时，远端版本保存为 `.conflict-节点名`，不会覆盖本地内容。
- `pull --overwrite` 会覆盖不同内容的本地文件，使用前请自行备份。
- 服务默认监听 `0.0.0.0:8787`，请只在可信局域网使用，并通过系统防火墙限制访问。

## 当前范围

这是可用的 MVP：文件清单、哈希比较、节点发现、手动拉取和 Web 管理均已实现。自动后台双向同步、设备审批、TLS、冲突合并与系统服务安装可作为后续版本加入。

## 许可证

本项目采用 [MIT License](LICENSE) 开源。
