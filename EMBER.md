# Ember

Ember 是 [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) 的长期跟随分叉（soft fork）。
Pumpkin 是南瓜，Ember 是把它点亮的那团火。

本文件是这个仓库最主要的"分叉自有文档"，维护规则都在这里。
**除 `README.md` 外，上游的任何文件都不承载 Ember 自己的内容。**

`README.md` 是唯一的例外：它是 Ember 面向 GitHub 的门面（已品牌化、中文说明、社区入口），
由 `.gitattributes` 的 `merge=ours` 保护——上游对 README 的改动在合并时自动保留我方版本，
不产生冲突。上游 README 原文镜像在 `PUMPKIN_README.md`，由 `sync-upstream` 每次同步自动刷新。

---

## 分支模型

| 分支 | 作用 | 规则 |
|---|---|---|
| `master` | 上游纯镜像 | 只允许 `git pull upstream master`，**永远不直接提交** |
| `main` | Ember 主线 | 上游镜像 + Ember 自己的提交，服务器部署用这条线 |
| `feat/*` | 大功能开发 | 从 `main` 拉出，完成后合回 `main` |

远程：

- `upstream` → `https://github.com/Pumpkin-MC/Pumpkin`（只拉不推）
- `origin` → `https://github.com/dongzh1/Ember`

**⚠️ 本地 `master` 推到 `origin` 时改名为 `upstream-mirror`**（`git push origin
master:upstream-mirror`），不再用 `master` 这个远程分支名。原因：上游
`.github/workflows/rust.yml` 的 SignPath 签名步骤写死判断
`github.ref == 'refs/heads/master'`（且没有 `HAS_SIGNPATH` 这类守卫，因为
`master` 分支的 workflow 文件必须和上游字节级一致），Ember 没配 SignPath
密钥，只要真把某个分支推成 GitHub 上名叫 `master` 的 ref 就必炸
（`Error: Input required and not supplied: organization-id`）。本地分支名不受
影响，`--ff-only` 校验、`git show master:README.md` 这些本地操作照常用
`master`，只有推到云端这一步换了目标分支名。

## 同步上游（建议每周一次，小步高频）

**首选：双击仓库根目录的 `sync-upstream.bat`。**
它会自动完成下面的全部步骤并推送云端；有冲突时会打印冲突报告
（标注哪些文件含 EMBER 标记块）并保留合并现场，按提示处理即可。

脚本不可用时的手工流程：

```bash
git fetch upstream
git checkout master && git merge --ff-only upstream/master && git push origin master:upstream-mirror
git checkout main   && git merge master
# 解决冲突（冲突只会出现在 EMBER 标记块附近，见下）
git push origin main
```

`master` 必须永远能 `--ff-only`；如果 fast-forward 失败说明有人往 master 直接提交了，要先修复。

## 代码隔离三条铁律

1. **新功能优先放新文件**。新事件、新模块、新命令都建自己的 `.rs` 文件
   （例如 `pumpkin/src/plugin/api/events/ember/` 目录下），新文件几乎不会和上游冲突。
2. **必须改上游文件时，改动用标记包起来**：

   ```rust
   // EMBER start - <功能名>
   ...改动内容...
   // EMBER end
   ```

   插入位置尽量选在函数末尾、match 分支末尾等"上游不常动"的地方。
   merge 冲突时，`grep -rn "EMBER start"` 就能列出全部自有改动清单。
3. **不改名、不移动、不格式化上游的任何东西**（`README.md` 除外，见开头）。目录名、crate 名、文件名、
   import 顺序全部保持上游原样。品牌欲望克制在新文件里。

## API 撞车处理（上游后来实现了我们已有的功能）

原则：**Ember 对外的 API 永不破坏，实现内部切换。**

以"玩家在容器中右键"事件为例，处理流程：

1. **上游没有时**：Ember 定义 `EmberPlayerContainerClickEvent`（放在自己的
   `events/ember/` 目录），在合适的位置用标记块插入触发点。
2. **上游实现了等价事件**（如 `PlayerContainerInteractEvent`）：
   - 删掉 Ember 自己的触发点标记块（消灭重复触发和冲突源）；
   - `EmberPlayerContainerClickEvent` 保留，改为**适配器**：注册一个上游事件的
     监听器，收到后转发/构造 Ember 事件发出，字段做映射；
   - 在事件文档注释上标 `#[deprecated(note = "请迁移到上游 XxxEvent")]`，
     给依赖它的插件一个大版本的迁移窗口，之后再移除。
3. **上游实现了但语义不同**（覆盖面更窄/字段更少）：两条线都保留，
   Ember 事件继续自己触发，文档里写清楚与上游事件的差异，等上游补齐后再走第 2 步。

判断标准一句话：**能委托就委托（薄壳），不能委托就并行（双线），永远不静默删 API。**

## 提交规范

- 一个功能一个提交（或一个 PR），提交信息用上游风格：`feat(plugin): ...` / `fix(world): ...`
- 提交信息里注明 `[EMBER]` 前缀方便 `git log --grep=EMBER` 列出全部自有改动：

  ```
  [EMBER] feat(config): add auto_approve_permissions
  ```

- 攒出的通用修复尽量给上游发 PR，合入后下次同步自动消掉我们的补丁。

## 日常脚本

根目录四个双击入口（详见 `scripts/README.md`）：

| 入口 | 作用 |
|---|---|
| `check.bat` | 代码检查（fmt + clippy） |
| `build.bat` | 构建打包：本地 Windows 包 / 云端 Linux+Windows 包 |
| `push.bat` | 提交并推送到 GitHub（推送前自动格式检查，master 上拒绝提交） |
| `sync-upstream.bat` | 同步上游（见上节） |

## 当前 Ember 自有改动清单

| 功能 | 涉及文件 | 说明 |
|---|---|---|
| **配置文件三层约定：`pumpkin.toml` / `ember.toml` / 各功能独立文件夹** | `pumpkin-config/src/lib.rs`（`EmberConfiguration`、`LoadConfiguration::load` 的嵌套目录支持）、`pumpkin-config/src/economy.rs`（`EconomyConfig` 自带 `LoadConfiguration`）、`pumpkin/src/main.rs`、`pumpkin/src/lib.rs`（`PumpkinServer::new`）、`pumpkin/src/server/mod.rs`（`Server::new`/`Server.ember_config`）、`pumpkin/src/server/economy.rs`（`EconomyManager::new()` 自己加载配置） | 三层约定：①`pumpkin.toml`/`AdvancedConfiguration` 保持纯上游,不再塞任何 Ember 字段;②零散的小设置（目前只有 `[performance]`）放 `ember.toml`/`EmberConfiguration`,加载方式和 `PumpkinConfig` 一样（同一套 `LoadConfiguration` trait,首次运行自动建默认值）;③体量大、有自己一整套设置的功能（经济、以后的 NPC 等）**各自开独立文件夹**,不挤进 `ember.toml`,比如经济系统是 `economy/economy.toml`（`EconomyConfig` 自己实现 `LoadConfiguration`,`EconomyManager::new()` 内部自己加载,`Server::new` 不需要知道这个文件在哪）。`LoadConfiguration::load` 改成 `create_dir_all` 建到 `get_path()` 指定的完整目录,不管嵌不嵌套文件夹都能自动建好。**⚠️破坏性变更**:旧版本 `pumpkin.toml` 里配过的 `[performance]`/`[economy]` 字段会被静默忽略(TOML 反序列化对不认识的字段默认丢弃,不报错),升级后 `[performance]` 的值要搬到 `ember.toml`,`[economy]` 的值要搬到 `economy/economy.toml` |
| `auto_approve_permissions` | `pumpkin-config/src/plugins.rs`、`pumpkin/src/plugin/mod.rs` | 配置开启后插件权限请求自动批准，适合无人值守服务器 |
| 一键上游同步脚本 | `sync-upstream.bat`、`scripts/sync-upstream.ps1` | 双击同步上游并推送云端，冲突时输出报告；同步时刷新上游 README 镜像 |
| 品牌 README + 上游镜像 | `README.md`、`PUMPKIN_README.md`、`.gitattributes` | README 品牌化 + 中文说明 + QQ 群；`merge=ours` 保护不撞冲突；上游 README 镜像到 `PUMPKIN_README.md` 随同步自动更新 |
| **EasyWorld 存储格式（单一格式，两后端）** | `pumpkin-world/src/chunk/format/easy.rs`、`pumpkin-world/src/chunk/easy_mysql.rs`、`pumpkin-world/src/chunk/easy_instance.rs`、`pumpkin-world/src/chunk/gen_fill.rs`、`pumpkin-world/src/chunk/convert.rs`、`pumpkin-config/src/chunk.rs`、`pumpkin-config/src/ember_world.rs`、`pumpkin-world/src/level.rs` | Ember 默认格式。用户只见**一个 `easy`**：`[world.chunk] type="easy" backend="file"\|"mysql"`(mysql 加 `url`)。**一切按地图大小自动**:小地图(border≤512²)整世界内存驻留+秒克隆,大地图区域惰性加载。详见 `docs/easyworld.md`。内部机制:整区 zstd+空块修剪+原子写(file);SlimeWorld 式一写多读心跳锁+Arc LRU 区缓存+命中免写锁(mysql);共享内存模板(只读克隆用,内部自动选,不再是用户可见格式)。**已删** `easy_shard`、`easy_instance`/`easy_mysql` 用户可见变体、archetype/residency 概念 |
| 每世界 sidecar（极简）| `pumpkin-config/src/ember_world.rs`、`pumpkin-config/src/world.rs`(`EmberRuntime`)、`pumpkin-world/src/dimension.rs` | 世界文件夹放 `ember-world.toml`:`border`(最大边界,≤512=小地图)、`generate`(seed\|void\|ocean)、`mode`(read_write\|read_only)、`source`(只读克隆源)、可选 `[chunk]` 换后端。解析进 `LevelConfig.ember` 贯穿世界构造。写错忽略并响亮报错 |
| 生成标签 void/ocean | `pumpkin-world/src/chunk/gen_fill.rs`（`GenFillIO`+`synthesize_chunk`） | `generate=void` 未生成区块回填全空气、`ocean` 回填基岩+石头+海平面水——在**存储层合成**(Missing→Loaded),不碰种子生成器。`seed`(默认)照常按种子生成 |
| 克隆两模式 + 尺寸驻留 | `pumpkin/src/server/mod.rs`（`clone_world`/`clone_world_readonly`）、`pumpkin/src/command/commands/world.rs`、`pumpkin-world/src/level.rs`（`prewarm_storage`） | **保存克隆** `/world clone <源> <目标> [save]`(复制持久化)、**只读克隆** `/world clone <源> <目标> readonly`(读源数据、改动丢弃、同源可多开共享内存)。小地图(border≤512)加载时自动整世界预热+worldborder 边界强制(防建过界);`/world prewarm` 手动(上限 64 区防 OOM) |
| 格式检测与迁移 | `pumpkin-world/src/chunk/convert.rs`、`pumpkin-world/src/level.rs`、`pumpkin/src/command/commands/world.rs` | 默认已是 easy。开档检测磁盘格式,与配置不符时**尊重磁盘并响亮报错**(切格式永不静默重生成地形);`/world convert <名> <格式>` 显式迁移(区块+实体+所有维度,旧文件转 `.bak`,写 sidecar 固化) |
| 副本零留存卫生 | `pumpkin-world/src/level.rs`（`Level::ephemeral`）、`pumpkin/src/world/mod.rs` | 只读克隆世界（`/world clone <源> <目标> readonly`）不建 region/entities/poi 文件夹、不落 POI，卸载即丢弃改动 |
| EasyWorld 验证 | `scripts/verify-easyworld.*`、`.github/workflows/easyworld-ci.yml` | 本地/CI 启动服务端验证 .easy 文件与 MySQL 表落盘 |
| 构建打包脚本 | `build.bat`、`check.bat`、`push.bat`、`scripts/check.ps1`、`scripts/build-windows.ps1`、`scripts/build-remote.ps1`、`scripts/push.ps1`、`.github/workflows/build-release.yml` | 本地 Windows 打包、云端 Linux+Windows 打包(每次触发自动发一个新 Release,版本号从 `0.01` 起自动递增,附件是裸的 `ember`/`ember.exe`)、代码检查、一键推送 |
| 动态世界管理 | `pumpkin/src/command/commands/world.rs`、`pumpkin/src/command/commands/mod.rs`、`pumpkin/src/server/mod.rs` | `/world list/load/unload/tp/clone/prewarm/convert/delete`（权限 `ember:command.world`，OP 3 级）；`Server::unload_world` 运行时卸载(撤离玩家→移出 tick→后台存盘停机)；`create_world_with(Option<LevelConfig>)` 按世界配置建世界 |
| 世界生命周期事件 | `pumpkin/src/plugin/api/events/world/world_load.rs`、`world_unload.rs`、`world/mod.rs`、`pumpkin/src/server/mod.rs` | `WorldLoad`（世界上线后通知）和 `WorldUnload`（卸载前触发,**可取消**,在撤离玩家前）——插件可观察/否决世界创建与卸载。接入 `create_world_with`/`unload_world` |
| clone 原语下沉 | `pumpkin/src/server/mod.rs`、`pumpkin/src/command/commands/world.rs` | `Server::clone_world(src,dst)` 成为可复用原语（文件递归拷 + easy_mysql 库内 `INSERT..SELECT` + 加载新世界,按源世界 resolved 配置判后端）;`/world clone` 命令收缩成薄壳。**服务端持有原语,业务留给命令/插件** |
| 原生对话框 + 磁盘世界枚举 | `pumpkin/src/entity/player.rs`（`show_dialog`/`clear_dialog`）、`pumpkin/src/server/mod.rs`（`list_world_folders`、`delete_world`）、`pumpkin-world/src/chunk/easy_mysql.rs`（`delete_world_data`） | `Player::show_dialog`/`clear_dialog` 原生发服务端对话框(原来只有 WASM 有);`Server::list_world_folders` 枚举磁盘未加载世界;`delete_world` 删世界(文件夹+库行+锁)。前两者可 PR 上游 |
| Mannequin NPC 实体 + WASM 控制 API | `pumpkin/src/entity/decoration/mannequin.rs`、`pumpkin-protocol/src/lib.rs`（`ResolvableProfile`）、`pumpkin/src/entity/decoration/mod.rs`、`pumpkin/src/entity/type.rs`、`ember-wit/ember.wit`、`pumpkin-plugin-api/src/lib.rs`、`pumpkin/src/plugin/loader/wasm/wasm_host/wit/v0_1/{entity,mod}.rs` | 上游已有 `MANNEQUIN` 实体类型 id 但无 Rust 实现,Ember 补上装饰实体 + `PROFILE` 元数据(`ResolvableProfile`)。WASM 控制面(`set-skin`/`set-description`/`set-immovable`)首次用**覆盖包技术**扩展 WIT 契约:`ember-wit/ember.wit` 声明独立的 `package ember:plugin`,其 `world` 用 `include` 整体引入上游 `pumpkin:plugin/plugin` 再 `import` 自己的 `mannequin` interface;bindgen 的 `path` 传数组同时读上游目录和 `ember-wit/`。**`pumpkin-plugin-wit` 已不是 git 子模块**(commit `bf24eac4` 起改为普通 vendored 文件,专为支撑这套叠加技术;内容仍要求与上游保持字节级不变,现在靠人工同步纪律而非 git 子模块机制保证)。**易错点**:一旦 `with` 里出现任意一条映射,wit-bindgen 就要求`include`进来的上游 world 触达的**全部** interface(不只是 mannequin 直接用到的)都显式给出 `with` 答案,否则 proc-macro panic;完整清单見两处 bindgen 调用旁的注释 |
| 多世界传送门配对 | `pumpkin/src/server/mod.rs`（`Server::get_paired_world`）、`pumpkin/src/block/blocks/nether_portal.rs`、`pumpkin/src/block/blocks/end_portal.rs`、`pumpkin/src/command/commands/world.rs`（`dimension_for_world_name`） | 上游 `get_world_from_dimension` 只取"第一个已加载的同维度世界",多世界下所有世界的下界/末地传送门都会通向默认世界(唯一有下界/末地的世界)。现按命名约定配对:`/world load <x>_nether`/`<x>_end` 建对应维度而非主世界;传送门优先找"同前缀+对应维度"的已加载世界,找不到再退回默认世界的下界/末地(未配对时行为不变) |
| 内置经济系统（多货币，`MySQL`） | `pumpkin-config/src/economy.rs`、`pumpkin/src/server/economy.rs`（`EconomyManager`）、`pumpkin/src/command/commands/economy.rs`（`/balance`、`/pay`、`/eco`） | 配置在独立的 `economy/economy.toml`(不在 `pumpkin.toml`/`ember.toml`,见上表),`enabled=false` 默认关闭,开启需配 `MySQL` `url`。纯整数金额,**懒创建账户**(查询未命中直接回落 `starting_balance`,不写库;第一次真正写入才 upsert 出行)。扣款原子性:`UPDATE ... WHERE balance >= ?` + 判 `rows_affected()`(仿 EasyWorld `easy_mysql.rs` 的 `REFRESH_LOCK` 模式),转账(跨两账户)用 `sqlx` 事务——是仓库里第一次用事务,单条语句表达不了跨行原子性。货币在配置里定义,不是数据库表。**故意不做插件层**(native/WASM 都没有专属 API 封装):`economy_manager` 是 `Server` 的公开字段,native 插件天生能直接调,没有另外接线 |
| 内置发包 NPC（纯客户端渲染，非真实实体） | `pumpkin/src/data/npc.rs`（`NpcEntry`/`NpcConfig`，独立文件夹 `npc/npcs.json`，不挂靠 `data/` 也不挂靠 `LoadJSONConfiguration`）、`pumpkin/src/server/npc.rs`（`NpcManager`）、`pumpkin/src/net/java/play.rs`（`handle_interact` 拦截）、`pumpkin/src/command/commands/npc.rs`（`/npc create\|remove\|list\|move\|skin\|setaction\|clearaction`） | 与已有的 `Mannequin`（真实实体,见上表）是两条不同的路线:这个 NPC **不进 `world.entities`**,不存档、不参与世界模拟,纯靠手搓 `CPlayerInfoUpdate`(`AddPlayer`+`UpdateListed(false)`,材质走真玩家同款 `Property`)+`CSpawnEntity`(类型=`player`)+皮肤图层 metadata,逐玩家单播。可见性照抄真实实体同一套 `chunker::is_within_view_distance`/`get_view_distance` 判定,每 10 tick 从 `Server::tick_worlds` 里重新对每个世界的在线玩家扫一遍,越过视距边界即发生成/移除包——只有 Java 客户端能看到(`try_enqueue_packet` 对基岩连接天然是空操作,未做基岩包体系)。皮肤只能从**当前在线玩家**复制(`GameProfile.properties` 里的 `textures` 属性原样转发),不联网解析 Mojang 用户名,和 Mannequin 的设计取舍一致。点击这个假 entity_id 时 `world.get_entity_by_id` 查不到,原本会走 `PlayerInteractUnknownEntityEvent`(左键还会被防外挂逻辑当成非法攻击踢人)——`handle_interact` 里在那之前先查 `npc_manager`,命中则以控制台身份执行配置的命令(`%player%` 占位符)并直接 return,不触发事件也不会被踢 |
| 离线模式登录验证（`MySQL`，限界虚空 + 聊天输密码） | `pumpkin-config/src/auth.rs`（`LoginConfig`，独立文件夹 `auth/auth.toml`）、`pumpkin/src/server/auth.rs`（`LoginManager`）、`pumpkin/src/server/mod.rs`（`add_player` 里的 world/gamemode 重定向）、`pumpkin/src/net/java/mod.rs`（`handle_play_packet` 白名单网关 + `handle_auth_chat`）、`pumpkin/src/command/commands/auth.rs`（`/auth reset`） | 只对 Java 版、`online_mode=false` 且 `[auth] enabled=true` 生效,Bedrock/在线模式完全不受影响。**关键限制(实测代码验证过)**:上游 `Dialog`/`DialogInput` 的文字输入框(`DialogInput::Text`)只做了"展示"没做"收集"——`SCustomClickAction` 回传只有 `action_id`+不透明 `payload`,`DialogInput` 也没有真实协议该有的 `key` 字段做关联,没法可靠拿到玩家在输入框里填的值。因此密码不走 dialog 输入框,只用 dialog 弹一个单按钮提示(`minecraft:notice` 类型),密码本身走聊天消息,在 `handle_play_packet` 里于 `PacketReceivedEvent` 之后加一道白名单网关拦截——这也是"冻结"未验证玩家的机制本身(移动/破坏方块/交互/命令包一律直接丢弃,不是逐个事件挂 cancel)。服务端自动判断注册(数据库无记录,连续输入两次密码)还是登录(有记录,输入一次校验),不需要玩家自己选。虚空世界是独立的 `__ember_limbo__`(仿 `/world clone ... readonly` 用的 `generate=void` 临时世界机制),进入后强制 `GameMode::Spectator`(复用现成机制,不是新写的冻结方案)。密码用 `argon2` 哈希(新增依赖,不用明文也不用 sha2 这类不适合密码的快速哈希)。24 小时内同 IP 重新加入可跳过验证。**故意排除**:不支持 Velocity/BungeeCord 代理场景(已知 BungeeCord 模式下真实 IP 转发有先例缺口未修,故意不做),不支持基岩版玩家 |

（新增功能时更新此表。）

## 服务端 vs 插件边界（架构原则）

**服务端负责原语**（存储格式、世界加载/卸载/克隆/实例/预热 API、生命周期事件）;**插件负责业务**（副本准入、排队、消息）。

**例外:内置经济系统、内置发包 NPC**(均见上表)。这条原则原本把"经济""NPC"这类业务都算作插件territory,但两者都被判定为要长期维护、放服务端和放插件维护成本相同,所以直接写进了服务端——这是有意识的偏离,不是原则失效,以后遇到类似"反正要一直维护"的功能可以参照这个先例判断,但不代表以后所有业务功能都该往服务端塞。

- **Native 插件**:`Context.server` 是公开的 `Arc<Server>`,无需任何额外改动即可调用全部原语（`create_world_with`/`unload_world`/`clone_world`/`is_world_unloading`/`prewarm_storage`）+ 监听/否决 `WorldLoad`/`WorldUnload` 事件——写业务适配插件无需任何新增 API。
- **WASM 插件**:世界管理 API 仍缺（只有 `create-world`,无 unload/clone/instance;无生命周期事件)。要让沙箱/热重载的 WASM 插件也能驱动,需扩展 WIT 契约——**不必 fork `pumpkin-plugin-wit`**:参照 mannequin 的先例,在 `ember-wit/` 开一个独立的 `ember:plugin` 包,`world` 用 `include` 叠加上游 `plugin` world 再加自己的 interface,bindgen 的 `path` 传数组同时读两个目录,上游目录保持字节级不变(见下表 mannequin 行)。世界管理 API 的扩展本身**仍未做**,待排期。

## 许可证

上游为 GPLv3，Ember 的全部改动同样以 GPLv3 发布。
