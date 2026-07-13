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
| 内置发包 NPC（纯客户端渲染，非真实实体，支持任意实体类型） | `pumpkin/src/data/npc.rs`（`NpcEntry`/`NpcConfig`，独立文件夹 `npc/npcs.json`，不挂靠 `data/` 也不挂靠 `LoadJSONConfiguration`）、`pumpkin/src/server/npc.rs`（`NpcManager`）、`pumpkin/src/net/java/play.rs`（`handle_interact`/`handle_attack` 均拦截）、`pumpkin/src/command/commands/npc.rs`（`/npc create\|remove\|list\|move\|skin\|setaction\|clearaction`） | 与已有的 `Mannequin`（真实实体,见上表）是两条不同的路线:这个 NPC **不进 `world.entities`**,不存档、不参与世界模拟,纯靠手搓 `CSpawnEntity`(可配置成任意 `EntityType`,不再锁死 `player`)+ 逐类型特判 metadata,逐玩家单播。`NpcEntry.entity_type`(资源名字符串,默认 `"player"` 保证旧 `npcs.json` 零改动)决定渲染成什么:`player` 走 `CPlayerInfoUpdate`(`AddPlayer`+`UpdateListed(false)`)+ 皮肤图层 metadata;`mannequin` 走 `PROFILE`/`IMMOVABLE` metadata(和真实 Mannequin 实体同一套皮肤转换);`falling_block` 的外观来自 `CSpawnEntity` 自身的 `data` 字段(存方块状态 id,不是 metadata);`item` 靠 `ITEM`/`ITEM_STACK` metadata;其余类型(生物、动物等)靠通用的 `CUSTOM_NAME`/`CUSTOM_NAME_VISIBLE` metadata 显示悬浮名字,不需要逐类型写代码——`/npc create <name> as <entity-type> [extra]` 里 `extra` 按解析出的类型决定含义(玩家名/方块名/物品名)。可见性照抄真实实体同一套 `chunker::is_within_view_distance`/`get_view_distance` 判定,每 10 tick 从 `Server::tick_worlds` 里重新对每个世界的在线玩家扫一遍,越过视距边界即发生成/移除包——只有 Java 客户端能看到(`try_enqueue_packet` 对基岩连接天然是空操作,未做基岩包体系)。皮肤只能从**当前在线玩家**复制(`GameProfile.properties` 里的 `textures` 属性原样转发),不联网解析 Mojang 用户名,和 Mannequin 的设计取舍一致。点击这个假 entity_id 时 `world.get_entity_by_id` 查不到,原本会走 `PlayerInteractUnknownEntityEvent`(左键攻击还会被防外挂逻辑当成"攻击无效实体"踢人,`SAttack`/`handle_attack` 和 `SInteract`/`handle_interact` 是两条独立的包处理路径,**两处都要拦**,曾经只拦了右键那条导致左键点击 NPC 被踢——实测发现的 bug)——`handle_interact`/`handle_attack` 里在踢人逻辑之前先查 `npc_manager`,命中则以控制台身份执行配置的命令(`%player%` 占位符)并直接 return,不触发事件也不会被踢。**基础动作**(`/npc lookat\|sneak\|swing`):`look_at_nearest_player` 在 `tick()` 自己单独的 4-tick 间隔(比 10-tick 的可见性判定更平滑)里算最近可见玩家的朝向发 `CHeadRot`(公式抄 `Entity::look_at`,注意包是 `CHeadRot` 不是 `CEntityHeadLook`);`sneaking` 走所有实体通用的 `SHARED_FLAGS_ID` metadata(照抄 `Entity::set_flag`);`swing` 是一次性动画包,不落盘。**移动**(`/npc moveto\|wander`):派 Explore agent 研究过现成的 A* 寻路(`entity/ai/pathfinder/`)——搜索核心可脱离真实 Entity 复用,但消费路径的 `Navigator::tick` 深度绑定真实物理(`movement_input` 原子量+碰撞箱),硬套不划算,所以 v1 走直线插值(公式抄 `MoveControl`),不含避障。`RuntimeNpc` 新增 `position`/`yaw`(与 `NpcEntry.x/y/z` 分离,后者仍是"出生点"配置,前者是运行时实时坐标,重启后从出生点重新开始,不落盘)。`moveto` 一次性走到指令发起者位置;`wander` 在出生点半径内随机选点、到达后随机停留 2-6 秒再选下一个点,持久化(`NpcEntry.wander_radius`),重启自动恢复。移动步进用 `CEntityPositionSync` 全量同步广播(不是增量 delta 编码,更简单,对少量装饰 NPC 的带宽成本可接受)。**精细可见性**(`/npc hide\|show\|distance`):`NpcEntry.hidden_from`(`HashSet<Uuid>`,不管距离都不显示)和 `visible_distance`(覆盖该 NPC 的判定距离,不是和玩家客户端视距取 min/max,是**整体替换**)都是持久化字段,改动后复用 `reset_runtime_and_despawn` 强制刷新(所有当前观察者都会重新走一遍生成判定,不是精确到单个玩家的定点操作,但实现简单,这几个指令也不是热路径)。**护送向导**(`/npc escort <name> <player>[ here]\|stop`):`RuntimeNpc.escort` 是纯运行时状态(不落盘)——护送目标是"当前在线的某个玩家",重启后没有恢复的意义,和 `goal`(moveto 目标)同一类取舍,和持久化的 `wander_radius` 不同;开始护送会清掉正在进行的 `goal`,`wander` 配置本身不动,护送结束后自然从下一个移动 tick 恢复游荡。两种模式:不带 `here` 是跟随模式(无限期,`destination=None`),带 `here` 是带路模式(带到执行指令时的位置,到达后自动结束护送)。带路模式下玩家掉队超过 6 格会暂停等待(`waiting` 标记,不前进);任一模式下玩家距离超过 24 格(不同世界也算,因为压根找不到玩家)直接传送追上而不是硬走过去——纯发包系统本来就没有碰撞检测,传送不会引入新的一类穿模问题。从 `move_npc`/`update_escort` 共用的部分抽出 `step_towards` 辅助函数(走一步+广播 `CEntityPositionSync`/`CHeadRot`),避免逻辑复制。**Agent 扩展钩子**:调研过 native 插件事件系统(`Payload`/`EventHandler`/`PluginManager::fire`,`#[derive(Event)]`+`#[cancellable]`两个宏生成样板)后,新增 `plugin/api/events/npc/npc_click.rs`(`NpcClickEvent`,`player`/`npc_name`/`action`/`command` 字段,可取消也可改写 `command`),`server::npc::NpcManager::click_command` 从只返回命令改成同时返回 NPC 名字;`net/java/play.rs` 的 `handle_attack`/`handle_interact` 两处 click_command 分支都改成先 `send_cancellable!` 发这个事件、`'after` 块里才跑(可能被改写过的)命令,不订阅这个事件的现有服务器行为完全不变。WASM 插件那条路径(WIT 定义+3处 handwritten glue)还没做,是下一步。**配置自愈**:审计发现 Ember 自己的 TOML 配置(`ember.toml`/`economy.toml`/`auth.toml`/`home` 都走 `pumpkin-config` 的 `LoadConfiguration` trait)早就自带"合并默认值+有变化就写回旧文件"的行为,唯一的缺口是 `npc/npcs.json` 用的是独立读写、加载后不会把新字段写回旧文件——`NpcConfig::load()` 补上加载后无条件 `save()`。vanilla 镜像的 `data/`(whitelist/ops/bans,`LoadJSONConfiguration`)不在这次范围内,那是纯镜像 vanilla 文件格式,不是 Ember 扩展字段的地方 |
| 内置商店+银行+市场拍卖行+抽奖（`MySQL`，参考 PixelShop 架构） | `pumpkin-config/src/shop.rs`（`ShopSystemConfig`/`ShopListConfig`/`LotteryListConfig`）、`pumpkin/src/server/shop/{mod,basic_shop,bank,market,lottery,gui,chat_capture,shop_menu}.rs`、`pumpkin/src/command/commands/{shop,bank,market,lottery}.rs` | 派两个 Explore agent 分别通读参考项目 `PixelShop`(Paper/Kotlin 商店插件)全部相关源码摸清四个子系统精确机制、核实 Ember 现有经济系统 API 和 GUI 基础设施能力后设计。四个子系统共享一个 `shop/shop.toml`(`enabled`+`url`+各子系统设置)和一个 `MySQL` 连接池(`ShopPool`,`Arc<OnceCell<Arc<MySqlPool>>>`,懒连接,`Server::new` 里只建一次,四个 manager 各自 `clone()` 一份句柄,各自建自己的表)。货币直接复用已有的多货币 `EconomyManager`(`PixelShop` 的 money+points 双货币模型 = 这里两个不同的 `currency` 字符串),物品只认**纯原版资源名**(不做 `MythicMobs` 那套自定义物品 ID)。**商店**(`/shop <name>`,唯一做了完整 GUI 的子系统):`shops.toml` 配置商品列表,GUI 是 `Generic9x3` 容器,自定义 `ShopScreenHandler` 覆盖 `on_slot_click` 拦截所有点击(左键买1个,底部固定槽位分别是"卖手持物品"和"赎回"),从不走默认的物品搬运逻辑;开菜单直接照抄 `VillagerEntity::open_trading_screen` 的 `player.open_handled_screen(factory, None)` 模式,不经过 WASM 那层。**动态定价**:真实出售会在 `ember_shop_prices` 表里累加 `sold_since_decay`,每跨过配置的阈值卖价 `×(1-price_decay_pct)`(下限是 `base_sell_price` 的 `min_price_pct`),`ShopManager::spawn_daily_recovery`(`Server::new` 里 `Arc::new` 后立刻调,因为要 `self: &Arc<Self>` 才能拿到自己的 Arc 引用)后台每 24 小时把跌价商品按 `daily_recovery_multiplier` 回涨、涨回 `base_sell_price` 就整行删掉;买价 = 卖价 + `max(1, ceil(卖价×buy_markup_pct))`,`buy_markup_pct` 做成可配置项(`PixelShop` 参考实现里这个 10% 溢价是硬编码的)。赎回只保留"最近一次卖出"的一条记录(`ember_shop_redeemable`,新卖出覆盖旧的),按**当前**买价结算,`redeemable_expiry_hours` 到期后懒惰失效(读取时判断,没有定时清理任务)。**批量购买子菜单这次没做**(PixelShop 是右键弹固定数量档位子菜单,时间关系v1 先跳过,只支持逐个购买),已在 `gui.rs` 里留了 `bulk_purchase_amounts` 辅助函数(64 叠→x1/5/10/32/64,16 叠→x1/4/8/12/16,和 PixelShop 分档一致)供以后接上。**银行**(`/bank balance\|deposit\|withdraw\|log`,命令界面非 GUI):`ember_bank_accounts`+`ember_bank_transactions` 两张表,复利结算是懒惰的(每次查余额/存取款前调 `settle_and_get`,不是定时任务),公式 `interest = principal×(1+daily_rate)^days_elapsed - principal`,单次结算按 `max_interest_per_settlement` 封顶(不是终身封顶),`last_settled_at` 精确前移 `24h×days_elapsed` 避免时间漂移。利率按权限分档(`BankTier{permission, max_balance, daily_rate, max_interest_per_settlement}`,玩家满足的所有档位里选 `max_balance` 最高的一档生效,无 `permission` 的档位当兜底)。流水表直接 `ORDER BY occurred_at DESC LIMIT 10` 查,不像 PixelShop 手动维护定长环形数组——SQL 的 `LIMIT` 天然做到这件事。存款超过该档上限或取款超过余额直接拒绝(不像 PixelShop 那样自动截断到上限,选择更明确的"要么全额成功要么报错")。**市场/拍卖行**(`/market sell\|list\|buy\|cancel`,命令界面):`ember_market_listings` 一张表(`id`/`seller_uuid`/`item`/`amount`/`currency`/`price`),**没有** PixelShop 那张"离线卖家代收邮箱"表——PixelShop 需要那张表是因为它的经济插件(Vault)绑定在线会话,而 Ember 自己的 `EconomyManager` 本来就直接写 `MySQL` 不管玩家在不在线,卖出款项 `economy.deposit` 直接打给卖家钱包,天然没有"离线卖家"这个问题。购买用 `DELETE FROM ember_market_listings WHERE id=? LIMIT 1` 判 `rows_affected()==1` 做原子"抢单"(和 `EconomyManager::withdraw` 同一套原子性原语),修正了 PixelShop 参考实现用 Kotlin `synchronized` 锁(只在单 JVM 内有效,Ember 多实例共享同一 `MySQL` 的场景下不安全)的问题;扣款失败会把这行数据插回去做补偿。手续费 `commission_rate`/`min_commission` 是真正生效的配置项(PixelShop 声明了这两个配置键但代码硬编码 0.05/1,是抄参考项目时主动修正的一处死代码)。挂单永不过期(和 PixelShop 一致,只会因为卖出或手动下架消失)。**抽奖**(`/lottery [pool]`,命令界面):`lottery.toml` 配置奖池(`cost`/`daily_limit`/`pity`/`prizes`),`ember_lottery_state` 表行锁事务做每日限额检查+计数更新,加权随机(`rand::random_range` 累减定位),命中保底阈值时只从 `pity.group` 标记的奖品里抽。**修正 PixelShop 的扣款时序**:参考实现是"DB 事务先提交验证限额→主线程再扣钱→失败才近似回滚计数",网络抖动会导致"算你抽了但没扣到钱";Ember 反过来是"先 `economy.withdraw`(本身原子安全)→扣款成功后才做行锁事务验证限额+更新计数→事务发现限额被并发占满就 `economy.deposit` 退款",失败窗口从"抽了没扣钱"变成"扣了没抽成"且有确定的退款路径。 |
| 悬浮展示实体菜单（`/menu`，纯发包，骑乘冻结） | `pumpkin-config/src/menu.rs`（`MenuListConfig`/`MenuConfig`/`MenuButton`）、`pumpkin/src/server/menu.rs`（`MenuManager`）、`pumpkin/src/command/commands/menu.rs`、`pumpkin/src/net/java/play.rs`（`handle_attack`/`handle_interact` 拦截 + 蹲下关闭）、`pumpkin/src/plugin/api/events/menu/menu_click.rs`（`MenuClickEvent`） | 参考数据包 `floatmenu_demo.zip` 的机制(骑乘冻结+锚点相对定位),但用 Rust 代码+发包 NPC 同一套基础设施实现,不是数据包。数据包里两处技巧是数据包本身受限才需要、Ember 不需要照抄:①用 `player_interacted_with_entity` 成就触发器模拟点击事件——Ember 直接有 `SAttack`/`SInteract` 包携带精确 entity_id;②模拟射线检测悬停——同理有精确点击目标。真正照抄的是核心视觉思路:锚点(玩家眼睛位置,水平朝向前方固定距离,开菜单那一刻**只算一次**,之后不管头怎么转都不重新计算)+ 每个元素相对锚点的固定偏移。**冻结机制**(骑乘,照抄数据包思路而非简化版):玩家骑乘一个刷在**自己当前脚下位置**(不是锚点)的隐形载具(`item_display` 不设置物品——`Display` 类实体无内容即不渲染,默认 0 判定箱,挂载点算出来正好是载具自身坐标,没有垂直"起跳感"),`CSetPassengers` 直接发给该玩家客户端,不经过真实 `world.entities`/`Entity::add_passenger`。解除挂载(`MenuManager::close`)照抄真实 `Entity::remove_passenger` 的时序:预分配 `teleport_id`、提前写 `player.awaiting_teleport` 挡掉骑乘期间发来的过期移动包,再发空 `CSetPassengers`,最后发一个位移量、旋转量全标记"相对且为0"的 `CPlayerPosition`,纯粹是让客户端结束"骑乘"内部状态、不产生画面回跳。**按钮实体**:每个按钮是 3 个实体叠在同一位置——`item_display`(图标)+`text_display`(标签,图标正下方)+`interaction`(纯发包点击判定箱,故意不设自定义宽高,沿用原版默认约 1×1——`interaction` 自身宽高在生成的协议数据表里没有独立可靠索引,只有 `Display` 家族的 `WIDTH`/`HEIGHT` 常量,硬套会读错实体)。所有展示实体都设 `BILLBOARD=CENTER`(始终朝向摄像机,因为骑乘只冻结移动不冻结视角)。**协议版本兼容坑**(直接逐字段读 `pumpkin-data/src/generated/{tracked_data,meta_data_type}.rs` 生成源码核实,没有凭经验猜):Mojang 26.x 协议把展示实体大量字段整体重新编号/改名(`TRANSLATION`→`TRANSLATION_ID`、`SCALE`→`SCALE_ID`、`TEXT_COMPONENT`→`COMPONENT`……),`Metadata::write` 对每个字段独立按连接版本查表,查到索引 255 或类型 id<0 就静默跳过不写——所以新旧字段**都发一遍**是安全且必要的,两者恰好只有一个对当前连接版本生效。`item_display` 自己的物品字段在旧协议的生成数据里没有可靠常量(`TrackedData::ITEM_STACK` 这个名字撞车,实际指向另一个实体的字段,索引对不上)——手写字面量 `TrackedId`(索引 23,和 `text_display` 的 `TEXT` 字段索引一致,两者都是 `Display` 基类之后的第一个自有字段)。**顺带发现但本次不修**:`server/npc.rs` 的 `CUSTOM_NAME`(悬浮名字)用的 `OPTIONAL_TEXT_COMPONENT` 类型在 26.x 协议整体失效(-1),NPC 名字在 26.x 客户端上大概率不显示——协议核实过程中顺带发现的既有 bug,记录留待后续处理,不在这次任务范围内。**配置**(`menu/menus.toml`,`LoadConfiguration`,和商店系统一样自愈):每个菜单 `name`/`title`/`distance`/`title_height`+任意数量 `buttons`(`item`/`label`/`command`/`offset_right`/`offset_up`/`offset_forward`/`scale`)。按钮的 `command` 支持 `%player%` 占位符,但**不是**以控制台身份执行,而是用点击玩家自己的 `get_command_source` 执行(和聊天里手动打指令同一来源)——`/spawn`/`/home` 这类依赖"指令发起者就是目标玩家"的既有指令不用改代码就能直接复用,权限也天然按点击玩家自己的等级走。默认菜单 3 个按钮:回到主城(`spawn`)、回到自己的世界(`home`)、全球市场(`market list`)。**交互**:`/menu [name]` 打开(已开着同名菜单再次触发=关闭,开着别的菜单直接切换);蹲下(唯一冻结时还能做的输入)也会关闭;点击任意按钮先无条件关闭菜单再判断事件是否被取消——`MenuClickEvent`(`player`/`menu_name`/`command`,可取消/可改写,和 `NpcClickEvent` 同一套模式)。点击判定按**点击玩家自己当前打开的菜单**查(不是全局 entity_id 表),伪造/猜测别人菜单按钮的 entity_id 不会命中——比现有 NPC 点击(全局按 entity_id 查,任何人猜中都能以控制台身份触发)更严格,顺便做得更安全但没有回头改 NPC 那边。**已知限制**:仅支持 Java 客户端(基岩版直接报错,和发包 NPC 现状一致);菜单打开期间玩家被外部传送(如管理员 `/tp`)不会特殊处理,按钮位置可能与新位置脱节,需要玩家蹲下或再次 `/menu` 关闭 |
| 离线模式登录验证（`MySQL`，限界虚空 + 聊天输密码） | `pumpkin-config/src/auth.rs`（`LoginConfig`，独立文件夹 `auth/auth.toml`）、`pumpkin/src/server/auth.rs`（`LoginManager`）、`pumpkin/src/server/mod.rs`（`add_player` 里的 world/gamemode 重定向）、`pumpkin/src/net/java/mod.rs`（`handle_play_packet` 白名单网关 + `handle_auth_chat`）、`pumpkin/src/command/commands/auth.rs`（`/auth reset`） | 只对 Java 版、`online_mode=false` 且 `[auth] enabled=true` 生效,Bedrock/在线模式完全不受影响。**关键限制(实测代码验证过)**:上游 `Dialog`/`DialogInput` 的文字输入框(`DialogInput::Text`)只做了"展示"没做"收集"——`SCustomClickAction` 回传只有 `action_id`+不透明 `payload`,`DialogInput` 也没有真实协议该有的 `key` 字段做关联,没法可靠拿到玩家在输入框里填的值。因此密码不走 dialog 输入框,只用 dialog 弹一个单按钮提示(`minecraft:notice` 类型),密码本身走聊天消息,在 `handle_play_packet` 里于 `PacketReceivedEvent` 之后加一道白名单网关拦截——这也是"冻结"未验证玩家的机制本身(移动/破坏方块/交互/命令包一律直接丢弃,不是逐个事件挂 cancel)。服务端自动判断注册(数据库无记录,连续输入两次密码)还是登录(有记录,输入一次校验),不需要玩家自己选。虚空世界是独立的 `__ember_limbo__`(仿 `/world clone ... readonly` 用的 `generate=void` 临时世界机制),进入后强制 `GameMode::Spectator`(复用现成机制,不是新写的冻结方案)。密码用 `argon2` 哈希(新增依赖,不用明文也不用 sha2 这类不适合密码的快速哈希)。24 小时内同 IP 重新加入可跳过验证。**故意排除**:不支持 Velocity/BungeeCord 代理场景(已知 BungeeCord 模式下真实 IP 转发有先例缺口未修,故意不做),不支持基岩版玩家 |
| 玩家导航指令（`/spawn`、`/home`、`/tpa` 系列）+ `/world` 指令 tab 补全 | `pumpkin/src/command/commands/spawn.rs`、`home.rs`、`tpa.rs`、`world.rs`、`pumpkin/src/server/home.rs`（`HomeManager`）、`pumpkin/src/server/tpa.rs`（`TpaManager`）、`pumpkin-config/src/home.rs`（`HomeConfig`，独立文件夹 `home/home.toml`） | `/spawn` 传送到主城(`server.worlds` 第一个世界的自身出生点);`/home` 传送到个人家园世界 `home_<uuid>`——已存在直接从磁盘加载,首次访问从操作员配置的模板世界(`home/home.toml` 的 `template_world`,默认 `home_template`)用 `clone_world` 克隆生成;`/tpa <玩家>`/`/tpahere <玩家>` 发起传送请求(`TpaManager` 按接收者 uuid 记录,120 秒过期,新请求覆盖旧请求),对方收到可点击的 `[接受]`/`[拒绝]` 聊天消息(`ClickEvent::RunCommand`,复用 `help.rs` 现成机制)也可以直接打 `/tpaaccept`/`/tpadeny`。均用 `EntityBase::teleport()`(自动判同世界/跨世界)而非直接调 `teleport_world`,避免同世界传送时误触发整套跨世界重生包。这几个玩家指令的游戏内提示文字为中文(征询过用户确认,区别于其余指令文件沿用的英文反馈)。**顺带**给 `/world load\|unload\|tp\|prewarm\|delete\|convert\|clone` 的世界名参数补上 tab 补全(`LoadedWorldSuggestionProvider`/`UnloadedWorldSuggestionProvider`/`AnyWorldSuggestionProvider`,按各子命令要求"必须已加载"还是"必须未加载"选取候选源),`convert` 的格式参数补固定列表建议,border 参数建议默认 512(复用既有的 `SMALL_MAP_MAX_BORDER` 常量)。**暂不包含**(用户明确说先做指令,UI 与背景任务推迟):G 键快捷菜单 dialog、社交玩家列表 dialog、家园世界闲置自动卸载定时器、`gen_budget` 忙碌拒绝提示——现有 `GenPoolBudget` 是每区块生成任务粒度的准入控制,语义上不适合直接挪用为"整个 `/home` 克隆/加载请求"级别的粗粒度限流,需要另开一套专用机制 |

（新增功能时更新此表。）

## 服务端 vs 插件边界（架构原则）

**服务端负责原语**（存储格式、世界加载/卸载/克隆/实例/预热 API、生命周期事件）;**插件负责业务**（副本准入、排队、消息）。

**例外:内置经济系统、内置发包 NPC**(均见上表)。这条原则原本把"经济""NPC"这类业务都算作插件territory,但两者都被判定为要长期维护、放服务端和放插件维护成本相同,所以直接写进了服务端——这是有意识的偏离,不是原则失效,以后遇到类似"反正要一直维护"的功能可以参照这个先例判断,但不代表以后所有业务功能都该往服务端塞。

- **Native 插件**:`Context.server` 是公开的 `Arc<Server>`,无需任何额外改动即可调用全部原语（`create_world_with`/`unload_world`/`clone_world`/`is_world_unloading`/`prewarm_storage`）+ 监听/否决 `WorldLoad`/`WorldUnload` 事件——写业务适配插件无需任何新增 API。
- **WASM 插件**:世界管理 API 仍缺（只有 `create-world`,无 unload/clone/instance;无生命周期事件)。要让沙箱/热重载的 WASM 插件也能驱动,需扩展 WIT 契约——**不必 fork `pumpkin-plugin-wit`**:参照 mannequin 的先例,在 `ember-wit/` 开一个独立的 `ember:plugin` 包,`world` 用 `include` 叠加上游 `plugin` world 再加自己的 interface,bindgen 的 `path` 传数组同时读两个目录,上游目录保持字节级不变(见下表 mannequin 行)。世界管理 API 的扩展本身**仍未做**,待排期。

## 许可证

上游为 GPLv3，Ember 的全部改动同样以 GPLv3 发布。
