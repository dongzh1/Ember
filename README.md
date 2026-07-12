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
| CI artifacts | Nightly source builds | + Auto-versioned releases (`ember`, `ember.exe`), one per ship |
| Built-in economy | — | + Multi-currency, MySQL-backed, atomic balance checks (`/balance`, `/pay`, `/eco`) |
| Packet-only NPCs | — | + Skinned fake-player NPCs, no plugin needed (`/npc`) |
| Offline-mode login wall | — | + Register/login verification for `online_mode = false` servers |

## Quick Start

### Download a pre-built binary

Grab the latest from [GitHub Releases](https://github.com/dongzh1/Ember/releases) — every cloud build publishes
a new `ember-vX.XX` release (auto-incrementing from `0.01`) with the ready-to-run `ember` (Linux) /
`ember.exe` (Windows) binaries attached. `chmod +x ember` after downloading on Linux.

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

`easy` is Ember's **default** chunk format (best compression, empty-chunk pruning, atomic
writes). Worlds stored in another format keep loading unchanged — on startup Ember detects the
on-disk format and honors it (with a loud log) instead of regenerating terrain. Migrate
deliberately with `/world convert <name> <format>` while the world is unloaded; old files are
kept as `*.bak` and the new format is pinned in the world's `ember-world.toml`.

> Note: `easy` prunes all-air chunks, so a chunk mined out to pure air regenerates on reload
> in generator-backed worlds. For void/skyblock-style maps use a `generate = "void"` sidecar
> or a non-pruning format (`anvil`/`pump`).

One format, two backends. The loading strategy is chosen automatically by map size — you only
pick the backend. Full guide: [`docs/easyworld.md`](docs/easyworld.md).

```toml
# On-disk .easy files (default)
[world.chunk]
type = "easy"
backend = "file"
```

```toml
# Shared MySQL storage (one writer, many read-only replicas)
[world.chunk]
type = "easy"
backend = "mysql"
url = "mysql://root:password@localhost:3306/ember"
```

### Per-world configuration (`ember-world.toml`)

Drop an `ember-world.toml` into a world's folder to override the global `[world]` settings for
that world only:

```toml
border   = 512            # max size in blocks; <=512 is loaded whole into RAM (small map)
generate = "seed"         # seed (default) | void | ocean
mode     = "read_write"   # read_write (default) | read_only (never persists)
source   = "arena"        # read-only clone: read another world's data
```

A world with a border ≤ 512×512 is prewarmed entirely into memory and clones instantly; larger
or borderless worlds load region by region. `generate = void`/`ocean` fills ungenerated chunks
without the terrain generator.

### Dynamic worlds

```
/world list                        # list loaded worlds and player counts
/world load <name>                 # load or create a world at runtime
/world unload <name>               # evict players to spawn, save, and unload
/world tp <name>                   # teleport yourself to a world's spawn
/world clone <source> <dest> [save|readonly]  # save copy, or read-only in-memory instance
/world prewarm <name>              # load a world's stored regions into memory
/world convert <name> <format>     # migrate an unloaded world's storage format
/world delete <name>               # delete an unloaded world (folder + DB rows + locks)
```

Permission: `ember:command.world` (OP level 3 by default). Loading/unloading and cloning never
stall the tick loop — saves run in the background. Every world-name/format/border argument
above tab-completes (loaded worlds, on-disk-but-unloaded worlds, or both, depending on what
each subcommand requires).

### Plugin permissions

```toml
[plugins]
auto_approve_permissions = true   # skip interactive permission prompts (unattended servers)
```

### Economy

Multi-currency, MySQL-backed, off by default. Config lives in its own `economy/economy.toml`
(not `pumpkin.toml`):

```toml
enabled = true
url = "mysql://user:pass@localhost:3306/ember"
```

```
/balance [player]                          # check your own (or another player's) balances
/pay <player> <amount> [currency]          # transfer to another player
/eco give|take|set|reset <player> <amount> [currency]  # admin balance management
```

Withdrawals are atomic (`UPDATE ... WHERE balance >= ?`, never a read-then-write race), so
concurrent spends on the same account can't overdraw it.

### Packet-only NPCs

NPCs that render entirely via packets — never a real world entity, so zero save-file footprint
and no world-simulation cost. Any entity type works, not just fake players: mobs/animals show a
generic nametag, `mannequin` and `player` support skins, `falling_block`/`item` render correctly
via their type-specific data.

```
/npc create <name> [player]                    # spawn as a fake player at your position
/npc create <name> as <entity-type> [extra]    # spawn as any entity type (extra = player name for
                                                # player/mannequin skins, block/item name for
                                                # falling_block/item)
/npc remove|list|move|skin <name> ...
/npc setaction <name> <command>                # run a console command on click (%player% placeholder)
```

Skins are always copied from a currently-online player (never resolved against Mojang).

### Offline-mode login wall

For `online_mode = false` servers, where anyone can otherwise join under any username. Off by
default; config lives in its own `auth/auth.toml`:

```toml
enabled = true
url = "mysql://user:pass@localhost:3306/ember"
```

New joiners drop into a holding world and are prompted to set a password (existing accounts are
prompted to log in instead — Ember tells the two apart automatically). Password entry happens
over chat, not a form field. A successful login from the same IP is remembered for 24h (configurable),
so it isn't repeated every join. `/auth reset <player>` recovers a forgotten password. Java only.

### Player navigation

```
/spawn                       # teleport to the hub world's spawn point
/home                        # teleport to your personal home world
/tpa <player>                # request to teleport to another player
/tpahere <player>            # request another player to teleport to you
/tpaaccept / /tpadeny        # accept or decline a pending request
```

Every player gets their own `home_<uuid>` world: loaded straight from disk if it already
exists, otherwise cloned from an operator-configured template (`home/home.toml`'s
`template_world`) on first visit. `/tpa`/`/tpahere` requests expire after 2 minutes if
unanswered, and the recipient's chat message includes clickable `[accept]`/`[deny]` buttons
alongside the plain commands. All five commands are allowed for every player by default.

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
| CI 产物 | 每日源码构建 | ＋ 自动发版成品包（`ember`、`ember.exe`），每次 ship 一个版本 |
| 内置经济系统 | 无 | ＋ 多货币、MySQL 存储、原子扣款（`/balance`、`/pay`、`/eco`） |
| 发包 NPC | 无 | ＋ 带皮肤的假人 NPC，不用装插件（`/npc`） |
| 离线模式登录验证 | 无 | ＋ `online_mode=false` 服务器的注册/登录验证墙 |

## 快速开始

**下载成品包**：到 [GitHub Releases](https://github.com/dongzh1/Ember/releases) 拿最新的 `ember-vX.XX` 版本
（每次云端构建自动发布,版本号从 `0.01` 起自动递增,附件是可直接运行的 `ember`(Linux)/`ember.exe`
(Windows),Linux 下载后记得 `chmod +x ember`）。

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

一种格式，两个后端；加载方式按地图大小**自动**决定，你只选后端。完整说明见 [`docs/easyworld.md`](docs/easyworld.md)。

```toml
# 磁盘 .easy 文件（默认）
[world.chunk]
type = "easy"
backend = "file"
```

```toml
# 共享 MySQL（一台读写，多台只读副本）
[world.chunk]
type = "easy"
backend = "mysql"
url = "mysql://root:password@localhost:3306/ember"
```

**按世界覆盖**：世界文件夹放 `ember-world.toml`：

```toml
border   = 512           # 最大边界（格），≤512 = 小地图（整世界内存驻留、秒克隆）
generate = "seed"        # seed 按种子 | void 虚空 | ocean 海洋底
mode     = "read_write"  # read_write（默认）| read_only（不落盘）
source   = "arena"       # 只读克隆：读另一个世界的数据
```

### 动态世界

```
/world list                              # 列出已加载世界及在线人数
/world load <名字>                       # 运行时加载或创建世界
/world unload <名字>                     # 撤离玩家到出生点、存盘、卸载
/world tp <名字>                         # 把自己传送到该世界出生点
/world clone <源> <目标> [save|readonly] # 保存克隆 / 只读内存克隆
/world prewarm <名字>                    # 把世界区域预热进内存
/world convert <名字> <格式>             # 迁移未加载世界的存储格式
/world delete <名字>                     # 删除未加载世界(文件夹+数据库行+锁)
```

权限：`ember:command.world`（默认 OP 3 级）。加载/卸载/克隆都不会卡服 —— 存盘在后台进行。
以上每个世界名/格式/border 参数都支持 tab 补全（已加载世界、磁盘上未加载的世界，或两者都算，
取决于各子命令的要求）。

### 插件权限

```toml
[plugins]
auto_approve_permissions = true   # 跳过插件权限交互确认，适合无人值守服务器
```

### 经济系统

多货币、MySQL 存储，默认关闭。配置在独立的 `economy/economy.toml`（不在 `pumpkin.toml` 里）：

```toml
enabled = true
url = "mysql://user:pass@localhost:3306/ember"
```

```
/balance [玩家]                              # 查自己(或他人)的各货币余额
/pay <玩家> <金额> [货币]                    # 转账给其他玩家
/eco give|take|set|reset <玩家> <金额> [货币] # 管理员操作余额
```

扣款是原子操作（`UPDATE ... WHERE balance >= ?`，不是先读后写），同一账户并发扣款不会透支。

### 发包 NPC

纯靠协议包渲染的 NPC——不是真实世界实体，零存档负担，不参与世界模拟。不止能伪装成玩家，
任意实体类型都支持：普通生物/动物显示通用悬浮名字，`mannequin`、`player` 支持换皮肤，
`falling_block`/`item` 靠各自的专属数据正确渲染外观。

```
/npc create <名字> [玩家]                        # 在你的位置生成一个假玩家,用你(或指定玩家)的皮肤
/npc create <名字> as <实体类型> [附加参数]       # 生成任意实体类型(附加参数:player/mannequin
                                                  # 填玩家名换皮肤,falling_block/item 填方块/物品名)
/npc remove|list|move|skin <名字> ...
/npc setaction <名字> <命令>                     # 点击时以控制台身份执行命令(%player% 占位符)
```

皮肤始终从当前在线玩家复制（不联网解析 Mojang 用户名）。

### 离线模式登录验证

适用于 `online_mode=false` 的服务器——没有正版验证时任何人都能用任意用户名进服。默认关闭，
配置在独立的 `auth/auth.toml`：

```toml
enabled = true
url = "mysql://user:pass@localhost:3306/ember"
```

新玩家进服会先落入一个隔离的虚空世界，提示设置密码（已有账户则提示登录，服务端自动判断
两者，不用玩家自己选）。密码通过聊天框输入，不是表单填写。同一 IP 24 小时内验证过可跳过
（时长可配置）。忘记密码用 `/auth reset <玩家>` 找管理员重置。仅支持 Java 版。

### 玩家导航指令

```
/spawn                        # 传送到主城世界的出生点
/home                         # 传送到你的个人家园世界
/tpa <玩家>                   # 请求传送到另一名玩家那里
/tpahere <玩家>               # 请求另一名玩家传送到你这里
/tpaaccept / /tpadeny         # 接受或拒绝待处理的传送请求
```

每个玩家都有自己的 `home_<uuid>` 世界：已存在就直接从磁盘加载，首次访问则从管理员配置的
模板世界（`home/home.toml` 的 `template_world`）克隆生成。`/tpa`/`/tpahere` 请求 2 分钟内
无人回应会自动失效，接收方收到的聊天消息里带可点击的 `[接受]`/`[拒绝]` 按钮，也可以直接打
对应指令。以上五个指令默认所有玩家都能用。

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
