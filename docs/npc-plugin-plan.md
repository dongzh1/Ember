# Ember NPC 插件 —— 设计与实施计划

> 目标：为 Ember(Pumpkin 分叉，MC **26.2 / 协议 776**）开发一款显示各类 NPC 的插件。
> 交付路线（已定）：**补齐 Ember 核心的 WASM 插件边界 → 用 WASM 插件写 NPC 逻辑**。
> 首批目标：站桩皮肤 NPC、假人(带皮肤)、多行全息文字、per-viewer 可见性。

## 0. 核心决策：走原版 `mannequin` 实体，不走假人封包 hack

MC 1.21.9(快照 25w36a)引入、26.2 沿用的 **`minecraft:mannequin`** 实体，是原版给的"NPC 身体"，
直接消灭了传统 Citizens/FancyNPCs 假人方案里最难的两块：皮肤签名、手搓封包。

| 能力 | mannequin 原生 | 收益 |
|---|---|---|
| 皮肤 | `minecraft:profile` 组件(元数据 idx17, `ResolvableProfile`)。**填用户名或 UUID，游戏自解析** | 无需签名 textures / `PlayerInfoUpdate` / Tab 列表 / per-viewer 封包 |
| 站桩 | `immovable` 字段(idx18)，且非乱走 mob | 无需 NoAI hack |
| 点击 | 正常 LivingEntity，真实碰撞箱，触发标准 entity-interact | 无需 Interaction 辅助实体 |
| 名字 | `description` 组件(idx19) + CustomName | 头顶文字原生 |
| 持久化 | 真实存盘实体 | 重启不丢，白送视距追踪 |

**边界/限制**：皮肤全局(原版无 per-viewer)；姿势仅粗粒度枚举(standing/crouching/swimming/fall_flying/sleeping，无逐关节)；
皮肤解析依赖 Mojang 会话服务(离线服需自喂 textures)。

来源：minecraft.wiki `/w/Mannequin`、`/w/Java_Edition_26.2`、`/w/Java_Edition_protocol/Entity_metadata`。

## 1. Ember 内核现状：数据就绪，行为缺失

**已就绪(地基)**
- 实体已注册：MANNEQUIN(id 83)、INTERACTION(69)、TEXT_DISPLAY(132)、ITEM_DISPLAY(72)、BLOCK_DISPLAY(15)
  —— `pumpkin-data/src/generated/entity_type.rs`
- **26.2 元数据已生成**：`TrackedData::PROFILE = 17`、`MetaDataType::RESOLVABLE_PROFILE = 41`、display 各字段索引
  —— `pumpkin-data/src/generated/tracked_data.rs`、`meta_data_type.rs`（协议漂移风险低，codegen 已咬住 26.2）
- `World::spawn_entity` 与 WASM `spawn-entity` 已接受 mannequin 变体
- `send_meta_data` + `Metadata<T: Serialize>` 推送机制现成；`usercache.rs` + ureq HTTP 现成
- **最佳照抄模板**：`pumpkin/src/entity/decoration/armor_stand.rs`(非 AI 实体 + 自带元数据)

**缺失(要补的行为)**
1. `from_type()` 无 MANNEQUIN 分支 → spawn 出无皮肤通用 LivingEntity(`pumpkin/src/entity/type.rs:304`)
2. `Metadata::write` 只处理 BLOCK_STATE/ITEM_STACK，无 ResolvableProfile 编码
   (`pumpkin-protocol/src/java/client/play/entity_metadata.rs:73-154`)
3. `minecraft:profile` 组件是空壳 `pub struct ProfileImpl;`(`pumpkin-data/src/data_component_impl.rs:2303`)
4. 无服务端皮肤解析：`lookup_profile_by_name` 只返回 (uuid, name)，不带 textures(`authentication.rs`)
5. 点击这些实体的事件未路由给 WASM 插件(真实实体点击事件目前 native-only，`event.wit` 缺该变体)

## 2. 分阶段实施

- **A｜核心·mannequin 身体**：`MannequinEntity` 结构体(照抄 armor_stand)+ `from_type` 分支；
  spawn 写 PROFILE/immovable/description/skin-parts 元数据；加 `ResolvableProfile` 可序列化类型 +
  `Metadata::write` 编码分支(用现成 type id 41)。
- **B｜核心·皮肤解析**：`authentication.rs` 加 用户名/UUID → 签名 textures 的 Mojang 拉取 + 缓存(复用 usercache)；离线服降级喂 textures。
- **C｜核心·暴露给 WASM**：WIT entity 资源加 `set-profile/set-description/set-immovable`；`event.wit` 加真实实体点击事件 + host 实现。
  **不需要动 `serialize_java_packet`**(那是 per-viewer 幽灵才用)。
- **D｜核心·可选·现代全息**：`TextDisplayEntity`(字段已生成)+ 暴露 `set-text/billboard/scale`，取代盔甲架多行字。
- **E｜WASM 插件**：`/npc create <皮肤名>|remove|list`；spawn mannequin/mob/text_display；右键跑命令/开 GUI(GUI API 已有)；
  scheduler 每 tick 转头朝向最近玩家(head-rot 已可发)。
- **F｜收尾·硬骨头**：per-viewer 可见性 —— 唯一 mannequin 给不了的，回裸封包路线
  (补 `serialize_java_packet` 的 CSpawnEntity/CSetEntityMetadata 分支 + entity-id 防冲突)。

所有核心改动用 `EMBER start` / `EMBER end` 标记包裹(见 EMBER.md 分叉规约)。

## 3. 剩余困难(按严重度)

1. **〖高〗服务端皮肤解析服务** —— Mojang textures-by-uuid 拉取 + 缓存 + 离线降级，内核现在完全没有。
2. **〖中〗per-viewer 可见性** —— 唯一还得走封包 hack，放阶段 F。
3. **〖中〗点击事件暴露到 WASM** —— well-scoped，动 WIT + host。
4. **〖中〗现代全息(text_display)** —— 字段已生成，要写结构体 + 元数据编码。
5. **〖低〗粗粒度姿势** —— mannequin 无法逐关节摆 pose。

## 4. MVP

阶段 **A+C+E** 最小闭环：`/npc create <皮肤名>` → 带皮肤、站桩、可右键的 mannequin，头顶名字，右键跑命令。
落地后叠加 D(全息)与 F(per-viewer)。
