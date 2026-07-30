# EasyWorld MySQL 世界存储

Ember 服务端强制使用 EasyWorld + MySQL。世界由数据库中的逻辑名称管理，不创建世界文件夹，也不读取或写入 `level.dat`、`region/`、`entities/`、`poi/`、玩家 NBT 或进度文件。

## 配置

`pumpkin.toml` 必须提供可用的 MySQL URL：

```toml
[world.chunk]
type = "easy"
backend = "mysql"
url = "mysql://user:password@127.0.0.1:3306/ember"
key_prefix = ""
max_cached_regions = 32
```

服务端启动时会校验 `type = "easy"`、`backend = "mysql"` 和非空 `url`；不满足时直接停止启动，避免意外回退到本地文件。

## 世界与维度

默认的三个维度使用独立的逻辑世界名和数据库键：

| 维度 | 逻辑世界名 |
|---|---|
| 主世界 | `world` |
| 下界 | `world_nether` |
| 末地 | `world_end` |

如果修改了默认世界名，例如 `survival`，对应名称为 `survival`、`survival_nether`、`survival_end`。`_nether` 和 `_end` 后缀不会重复追加。

MySQL 中的 `easyworld_worlds` 保存逻辑世界目录，`easyworld_regions` 保存区域和区块数据，`easyworld_locks` 负责一写多读的心跳锁。世界列表、克隆和删除都直接操作这些表，不扫描磁盘目录。

## 持久化范围

会持久化：

- 区块、方块和方块实体
- 随区块编码的 Ember 自定义方块与家具
- 逻辑世界目录

按当前设计不会持久化，重启后会重置或消失：

- 实体
- POI 和传送门 POI
- 玩家 NBT
- advancements
- `level.dat` 中的出生点、游戏规则和世界元数据

因此这里的“世界全部 MySQL 化”特指需要保留的区块世界数据全部进入 MySQL，并不表示上述被明确禁用的数据也会保存。

## 动态世界

```text
/world list                              # 列出已加载世界
/world load <名称>                      # 从目录加载或注册逻辑世界
/world unload <名称>                    # 保存区块并卸载
/world tp <名称>                        # 传送到世界出生点
/world clone <源> <目标> [save|readonly] # 数据库克隆或临时只读克隆
/world prewarm <名称>                   # 预热数据库中的区域
/world delete <名称>                    # 删除未加载世界的数据库记录
```

`/world convert` 在强制 MySQL 模式下会拒绝执行，因为不存在可转换的本地世界格式。权限统一为 `ember:command.world`，默认要求 OP 3 级。

## 克隆与共享

普通 clone 在数据库内复制区域行，再注册并加载新的逻辑世界；不会复制文件。readonly clone 读取源世界并把改动留在内存，卸载后丢弃。

同一世界同一时间只允许一台服务端持有读写锁，其他实例应使用只读模式。`key_prefix` 必须一致，才能指向同一组逻辑世界。
