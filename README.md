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

## 同步策略与安全

- 默认同步 `~/.codex`，排除 `auth.json`、凭据、日志、缓存和临时文件。
- API 使用共享密钥认证；密钥不会经节点发现广播或写入 U 盘快照。
- 默认只拉取。遇到同名但内容不同的文件时，远端版本保存为 `.conflict-节点名`，不会覆盖本地内容。
- `pull --overwrite` 会覆盖不同内容的本地文件，使用前请自行备份。
- 服务默认监听 `0.0.0.0:8787`，请只在可信局域网使用，并通过系统防火墙限制访问。

## 当前范围

这是可用的 MVP：文件清单、哈希比较、节点发现、手动拉取和 Web 管理均已实现。自动后台双向同步、设备审批、TLS、冲突合并与系统服务安装可作为后续版本加入。
