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

## 同步上游（建议每周一次，小步高频）

**首选：双击仓库根目录的 `sync-upstream.bat`。**
它会自动完成下面的全部步骤并推送云端；有冲突时会打印冲突报告
（标注哪些文件含 EMBER 标记块）并保留合并现场，按提示处理即可。

脚本不可用时的手工流程：

```bash
git fetch upstream
git checkout master && git merge --ff-only upstream/master && git push origin master
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
| `auto_approve_permissions` | `pumpkin-config/src/plugins.rs`、`pumpkin/src/plugin/mod.rs` | 配置开启后插件权限请求自动批准，适合无人值守服务器 |
| 一键上游同步脚本 | `sync-upstream.bat`、`scripts/sync-upstream.ps1` | 双击同步上游并推送云端，冲突时输出报告；同步时刷新上游 README 镜像 |
| 品牌 README + 上游镜像 | `README.md`、`PUMPKIN_README.md`、`.gitattributes` | README 品牌化 + 中文说明 + QQ 群；`merge=ours` 保护不撞冲突；上游 README 镜像到 `PUMPKIN_README.md` 随同步自动更新 |
| EasyWorld 存储格式 | `pumpkin-world/src/chunk/format/easy.rs`、`pumpkin-world/src/chunk/easy_mysql.rs`、`pumpkin-config/src/chunk.rs`、`pumpkin-world/src/level.rs`、`pumpkin-world/src/chunk/palette.rs`、`pumpkin-world/src/chunk/io/mod.rs` | 区块存储新格式：`easy`（区域级 zstd + 空区块修剪的 .easy 文件）和 `easy_mysql`（存 MySQL，SlimeWorld 式一写多读：`mode = "read_write"` 独占写锁+心跳，`mode = "read_only"` 任意多服务器只读共享，写者宕机 60 秒后可被接管）。配置 `[world.chunk] type = "easy"` 或 `type = "easy_mysql"` + `url` + 可选 `mode` |
| EasyWorld 验证 | `scripts/verify-easyworld.*`、`.github/workflows/easyworld-ci.yml` | 本地/CI 启动服务端验证 .easy 文件与 MySQL 表落盘 |
| 构建打包脚本 | `build.bat`、`check.bat`、`push.bat`、`scripts/check.ps1`、`scripts/build-windows.ps1`、`scripts/build-remote.ps1`、`scripts/push.ps1`、`.github/workflows/build-release.yml` | 本地 Windows 打包、云端 Linux+Windows 打包（`ember-*` 标签自动发 Release）、代码检查、一键推送 |
| 动态世界管理 | `pumpkin/src/command/commands/world.rs`、`pumpkin/src/command/commands/mod.rs`、`pumpkin/src/server/mod.rs` | `/world list/load/unload/tp/clone/prewarm` 命令（权限 `ember:command.world`，OP 3 级）；`Server::unload_world` 运行时卸载世界（撤离玩家→移出 tick→存盘停机）。加载复用上游 `Server::create_world` |
| 每世界配置 sidecar | `pumpkin-config/src/ember_world.rs`、`pumpkin-config/src/lib.rs`、`pumpkin-world/src/dimension.rs`、`pumpkin/src/server/mod.rs` | 世界文件夹放 `ember-world.toml` 即可按世界覆盖存储格式/自动存盘/常驻策略（`archetype = personal/hub/resource/dungeon`、`residency = auto/full/lazy`、`[chunk]` 任意格式）。四类世界一台服共存的总闸；`/world clone` 按源世界的 resolved 配置决定数据库克隆 |
| 世界常驻预热 | `pumpkin-world/src/level.rs`（`prewarm_storage`）、`pumpkin-world/src/chunk/io/mod.rs`（`list_regions`） | 按 residency 策略把小世界的全部区域预热进存储缓存：`auto` 下 ≤1 区(512²)全驻、hub/dungeon ≤4 区(1024²)全驻,上限 64 区防 OOM;`/world prewarm <名>` 手动触发。主城传送风暴零冷加载 |
| EasyShard 格式 (`easy_shard`) | `pumpkin-world/src/chunk/format/easy_shard.rs`、`pumpkin-config/src/chunk.rs`、`pumpkin-world/src/chunk/io/file_manager.rs`、`pumpkin-world/src/chunk/format/pump.rs` | 资源世界专用 `.ezs`：每 `group_chunks`（默认 1）个区块独立 zstd,改一个只重压一组,终结整区重压写放大;常驻内存为压缩态（省 10-30 倍）;脏区**在被玩家占用时也随 autosave 增量落盘**（其他格式行为不变,file_manager 激活了原为死代码的 `should_write` 门）;原子写（tmp+fsync+rename） |
| 副本模板多实例 (`easy_instance`) | `pumpkin-world/src/chunk/easy_instance.rs`、`pumpkin/src/command/commands/dungeon.rs` | SlimeWorld 式模板实例化：模板（easy/easy_mysql 世界）解压一次进全局注册表,N 个实例共享同一份内存（开新实例=Arc 克隆,纳秒级）;每实例 RAM 覆盖层,改动卸载即丢,模板永不被污染;模板外区块回填空气 `Full` 区块,原版地形生成器永不运行;实体用 `DiscardEntityIO` 不落盘。`/dungeon prewarm/start/stop/list/reload`（权限 `ember:command.dungeon`,OP 3 级） |
| easy 格式强化 | `pumpkin-world/src/chunk/format/easy.rs`、`pumpkin-world/src/chunk/easy_mysql.rs` | 原子写（崩溃不再截断区域文件）;干净区域跳过整区重压;O(1) 前缀和索引替代 O(N) 扫描;MySQL 区缓存命中只走读锁（多人同世界冷加载不再串行）;`easy_mysql` 支持模板批量加载与区列表 |
| 格式检测与转换 | `pumpkin-world/src/chunk/convert.rs`、`pumpkin-world/src/level.rs`、`pumpkin/src/command/commands/world.rs` | 开档时检测磁盘已有格式:与配置不符时**尊重磁盘并响亮报错**（切换格式永不再静默重生成地形）;`/world convert <名> <格式>` 显式迁移（区块+实体,旧文件改 `.bak`,成功后写 sidecar 固化格式,覆盖 DIM-1/DIM1） |
| 默认格式 = easy | `pumpkin-config/src/chunk.rs` | `#[default]` 从 `pump` 移到 `easy`——新世界原生 easy;老世界由格式检测兜底继续可读 |
| 副本零留存卫生 | `pumpkin-world/src/level.rs`（`Level::ephemeral`）、`pumpkin/src/world/mod.rs`、`pumpkin/src/command/commands/dungeon.rs` | 实例世界不再创建 region/entities/poi 文件夹;shutdown 时抑制传送门 POI 落盘;`/dungeon stop` 等后台卸载完成后清理残留实例文件夹——"改动不留存"承诺闭环 |
| 启动世界预热 | `pumpkin/src/server/mod.rs` | 启动加载的默认世界（主世界等）也按 sidecar residency 预热（原来只有动态加载的世界吃 sidecar） |

（新增功能时更新此表。）

## 许可证

上游为 GPLv3，Ember 的全部改动同样以 GPLv3 发布。
