<div align="center">

# Ember

[![CI](https://github.com/dongzh1/Ember/actions/workflows/rust.yml/badge.svg)](https://github.com/dongzh1/Ember/actions)
[![License: GPLv3](https://img.shields.io/badge/License-GPLv3-yellow.svg)](https://opensource.org/licenses/gpl-3-0)

**English** ｜ [中文说明](#中文说明)

</div>

**Ember** is a long-term soft fork of [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin), a Minecraft
server built entirely in Rust. Pumpkin is the pumpkin — Ember is the fire that lights it.

Ember tracks Pumpkin upstream weekly and adds features built for running real servers:
custom world formats, MySQL-backed storage, SlimeWorld-style shared worlds, runtime world
management, and one-click build/deploy tooling. Every Pumpkin feature (dual Java/Bedrock
protocol, entity AI, world generation, plugin system) is inherited and kept current.

<div align="center">

![chunk loading](assets/pumpkin_chunk_loading.webp)

### 💬 Join the community

Questions, feature requests, or swapping notes with other server owners —
come say hi in the QQ group:

**【Ember 交流群】** · [点击加入](https://qm.qq.com/q/hmHPe9Diog) · 群号 `1060828130`

</div>

## Why Ember over Pumpkin

| Feature | Pumpkin | Ember |
|---|---|---|
| World formats | Anvil, Linear, Pump | + **Easy** (region-level zstd + empty-chunk pruning) |
| Database storage | — | + **MySQL** (one-writer / many-readers, heartbeat locking) |
| Shared worlds | — | + **read-only** replicas + `/world clone` (SlimeWorld-style) |
| Region file size | baseline | **60–80% smaller** with Easy |
| Runtime world management | — | + `/world load/unload/tp/clone` with permissions |
| Auto-approve plugin permissions | — | + `auto_approve_permissions` config |
| One-click build & push | — | + `build.bat`, `check.bat`, `push.bat`, `sync-upstream.bat` |
| CI artifacts | Nightly source builds | + Cross-platform releases (`ember-windows`, `ember-linux`) |

## Quick Start

### Download a pre-built binary

Grab the latest from [GitHub Releases](https://github.com/dongzh1/Ember/releases) (tag `ember-*`).

### or Build from source

```bash
git clone https://github.com/dongzh1/Ember.git --recurse-submodules
cd Ember

# Windows: double-click build.bat  (local build, or cloud Linux+Windows build)
# Linux:
cargo build --release
```

### Run

```bash
./pumpkin   # writes pumpkin.toml to the working dir on first launch, then starts
```

Edit `pumpkin.toml` and restart — see [Configuration](#configuration) below.

## Configuration

All settings live in `pumpkin.toml` in the server's working directory.

### EasyWorld formats

```toml
# Region-level zstd compression (.easy files) — much smaller than Pump
[world.chunk]
type = "easy"
```

```toml
# MySQL storage with SlimeWorld-style shared access
[world.chunk]
type = "easy_mysql"
url = "mysql://root:password@localhost:3306/ember"
mode = "read_write"        # "read_write" (default, one writer) or "read_only" (unlimited readers)
key_prefix = "my_cluster"  # optional: namespace the DB keys when several clusters share one database
max_cached_regions = 32    # LRU cap on resident decompressed regions (memory bound)
```

- **read_write** — takes a per-world heartbeat lock; only one live writer per world. A second
  writer degrades to read-only automatically. A crashed writer is taken over after ~60s.
- **read_only** — never writes; safe on any number of servers simultaneously. Great for lobby
  displays, spectator copies, or dungeon templates.

### Dynamic worlds

```
/world list                        # list loaded worlds and player counts
/world load <name>                 # load or create a world at runtime
/world unload <name>               # evict players to spawn, save, and unload
/world tp <name>                   # teleport yourself to a world's spawn
/world clone <source> <dest>       # copy a world to a new name and load it (SlimeWorld-style)
```

Permission: `ember:command.world` (OP level 3 by default). Loading/unloading and cloning never
stall the tick loop — saves run in the background.

### Plugin permissions

```toml
[plugins]
auto_approve_permissions = true   # skip interactive permission prompts (unattended servers)
```

## Inherited Pumpkin Features

Everything from upstream Pumpkin — Ember syncs weekly and keeps full compatibility:

- **Dual protocol** — Java Edition (TCP) and Bedrock Edition (UDP) on one server
- **World generation** — vanilla terrain, biomes, structures, lighting
- **Entities & AI** — mobs, animals, pathfinding, combat
- **Plugin system** — Native (`.dll`/`.so`) and WASM plugins with a rich API
- **Commands** — vanilla commands with Brigadier-style dispatch
- **Proxy support** — BungeeCord and Velocity

> 📖 The upstream Pumpkin README is mirrored (and auto-updated on every sync) at
> **[PUMPKIN_README.md](PUMPKIN_README.md)**.

## Fork Maintenance

See [EMBER.md](EMBER.md) for the full fork policy:

- `master` branch = upstream mirror (never committed to directly)
- `main` branch = upstream + Ember changes
- `sync-upstream.bat` = one-click merge from Pumpkin
- Changes in upstream files are wrapped in `EMBER start` / `EMBER end` markers
- grep `EMBER start` to list every deviation from upstream

## License

Upstream is GPLv3. All Ember changes are likewise released under [GPLv3](LICENSE).

---

<div align="center">

# 中文说明

[English](#ember) ｜ **中文**

</div>

**Ember** 是 [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)（一个纯 Rust 编写的 Minecraft 服务端）
的长期跟随分叉。Pumpkin 是南瓜，Ember 是把它点亮的那团火。

Ember 每周跟随上游 Pumpkin，同时叠加面向**实际开服**的功能：自定义世界格式、MySQL 存储、
SlimeWorld 式共享世界、运行时世界管理，以及一键构建/部署工具。Pumpkin 的全部能力
（Java/基岩双协议、实体 AI、世界生成、插件系统）都完整继承并持续更新。

<div align="center">

### 💬 加入交流群

使用中遇到问题、想反馈需求、或和其他服主交流 Ember 心得，欢迎进群：

**【Ember 交流群】** · [点击加入](https://qm.qq.com/q/hmHPe9Diog) · 群号 `1060828130`

</div>

## 相比 Pumpkin 多了什么

| 功能 | Pumpkin | Ember |
|---|---|---|
| 世界格式 | Anvil、Linear、Pump | ＋ **Easy**（区域级 zstd 压缩 + 空区块修剪） |
| 数据库存储 | 无 | ＋ **MySQL**（一写多读，心跳锁保护） |
| 共享世界 | 无 | ＋ **只读**副本 + `/world clone`（SlimeWorld 式） |
| 区域文件体积 | 基准 | Easy **小 60–80%** |
| 运行时世界管理 | 无 | ＋ `/world load/unload/tp/clone`，带权限 |
| 插件权限自动批准 | 无 | ＋ `auto_approve_permissions` 配置 |
| 一键构建推送 | 无 | ＋ `build.bat`、`check.bat`、`push.bat`、`sync-upstream.bat` |
| CI 产物 | 每日源码构建 | ＋ 跨平台成品包（`ember-windows`、`ember-linux`） |

## 快速开始

**下载成品包**：到 [GitHub Releases](https://github.com/dongzh1/Ember/releases) 拿最新的 `ember-*` 版本。

**源码构建**：

```bash
git clone https://github.com/dongzh1/Ember.git --recurse-submodules
cd Ember

# Windows：双击 build.bat（可选本地构建，或云端一键出 Linux+Windows 包）
# Linux：
cargo build --release
```

**运行**：

```bash
./pumpkin   # 首次启动会在当前目录生成 pumpkin.toml，然后启动
```

编辑 `pumpkin.toml` 后重启即可，配置见下。

## 配置

所有配置都在服务端工作目录下的 `pumpkin.toml` 里。

### EasyWorld 世界格式

```toml
# 区域级 zstd 压缩（.easy 文件），比 Pump 小很多
[world.chunk]
type = "easy"
```

```toml
# MySQL 存储，SlimeWorld 式共享访问
[world.chunk]
type = "easy_mysql"
url = "mysql://root:password@localhost:3306/ember"
mode = "read_write"        # "read_write"（默认，独占写）或 "read_only"（任意多服务器只读共享）
key_prefix = "my_cluster"  # 可选：多套集群共用一个数据库时给 key 加命名空间
max_cached_regions = 32    # 常驻的已解压区域数量上限（LRU，控制内存占用）
```

- **read_write**：抢占该世界的心跳锁，同一世界同时只允许一个写入服务器；第二个写入服务器会自动
  降级为只读；写入服务器崩溃后约 60 秒锁可被接管。
- **read_only**：从不写库，可被任意多个服务器同时加载。适合大厅展示、旁观副本、副本模板等。

### 动态世界

```
/world list                        # 列出已加载世界及在线人数
/world load <名字>                 # 运行时加载或创建世界
/world unload <名字>               # 撤离玩家到出生点、存盘、卸载
/world tp <名字>                   # 把自己传送到该世界出生点
/world clone <源> <目标>           # 复制世界为新名字并加载（SlimeWorld 式）
```

权限：`ember:command.world`（默认 OP 3 级）。加载/卸载/克隆都不会卡服 —— 存盘在后台进行。

### 插件权限

```toml
[plugins]
auto_approve_permissions = true   # 跳过插件权限交互确认，适合无人值守服务器
```

## 继承的 Pumpkin 能力

上游 Pumpkin 的一切，Ember 每周同步、保持完整兼容：

- **双协议** —— 一个服务端同时跑 Java 版（TCP）和基岩版（UDP）
- **世界生成** —— 原版地形、生物群系、结构、光照
- **实体与 AI** —— 怪物、动物、寻路、战斗
- **插件系统** —— 原生（`.dll`/`.so`）与 WASM 插件，API 丰富
- **命令** —— 原版命令，Brigadier 式派发
- **代理支持** —— BungeeCord 与 Velocity

> 📖 上游 Pumpkin 的完整 README 镜像在
> **[PUMPKIN_README.md](PUMPKIN_README.md)**，每次同步上游时自动更新。

## 分叉维护

完整分叉规约见 [EMBER.md](EMBER.md)：

- `master` 分支 = 上游纯镜像（永不直接提交）
- `main` 分支 = 上游 + Ember 改动
- `sync-upstream.bat` = 一键从 Pumpkin 合并上游
- 改动上游文件时用 `EMBER start` / `EMBER end` 标记包裹
- `grep "EMBER start"` 可列出对上游的每一处改动

## 许可证

上游为 GPLv3，Ember 的全部改动同样以 [GPLv3](LICENSE) 发布。
