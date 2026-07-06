<div align="center">

# Ember

[![CI](https://github.com/dongzh1/Ember/actions/workflows/rust.yml/badge.svg)](https://github.com/dongzh1/Ember/actions)
[![License: GPLv3](https://img.shields.io/badge/License-GPLv3-yellow.svg)](https://opensource.org/licenses/gpl-3-0)

</div>

**Ember** is a long-term soft fork of [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin), a Minecraft
server built entirely in Rust. Pumpkin is the pumpkin — Ember is the fire that lights it.

Ember follows Pumpkin upstream weekly while adding features for real-world server operation:
custom world formats, MySQL-backed storage, dynamic world management, and one-click build &
deploy tooling. All Pumpkin features (dual Java/Bedrock protocol, entity AI, world generation,
plugin system) are inherited and kept up to date.

<div align="center">

![chunk loading](assets/pumpkin_chunk_loading.webp)

</div>

## Why Ember over Pumpkin

| Feature | Pumpkin | Ember |
|---|---|---|
| World formats | Anvil, Linear, Pump | + **Easy** (region-level zstd + chunk pruning) |
| Database storage | — | + **MySQL** (multi-server read/write, heartbeat locking) |
| World file size (500×500 map) | ~18 MB (Pump) | **~2-5 MB** (Easy) |
| Empty-chunk pruning | — | + ChunkPruner (skips all-air chunks) |
| Runtime world management | — | + `/world load/unload/tp` with permission system |
| Auto-approve plugin permissions | — | + `auto_approve_permissions` config |
| One-click build & push | — | + `build.bat`, `push.bat`, `sync-upstream.bat` |
| CI artifacts | Nightly source builds | + Cross-platform releases (`ember-windows`, `ember-linux`) |

## Quick Start

### Download a pre-built binary

Grab the latest from [GitHub Releases](https://github.com/dongzh1/Ember/releases) (tag `ember-*`).

### or Build from source

```bash
git clone https://github.com/dongzh1/Ember.git --recurse-submodules
cd Ember

# Windows: double-click build.bat
# Linux:
cargo build --release
```

### Run

```bash
./pumpkin   # generates config/ on first launch, then starts
```

Edit `config/configuration.toml` — see [Configuration](#configuration) below.

## Configuration

### EasyWorld formats

```toml
# Region-level zstd compression (.easy files) — 60-80% smaller than Pump
[chunk]
type = "easy"

# MySQL storage with multi-server read/write
[chunk]
type = "easy_mysql"
url = "mysql://root:password@localhost:3306/ember"
mode = "read_write"   # or "read_only" for shared access
key_prefix = "my_cluster"
max_cached_regions = 32
```

### Dynamic worlds

```
/world list                      # list loaded worlds
/world load <name> [<seed>]      # load or create a world
/world unload <name>             # unload (evicts players, saves, removes from tick)
/world tp <name>                 # teleport self to world spawn
```

Permission: `ember:command.world` (OP level 3 by default).

### Plugin permissions

```toml
[plugins]
auto_approve_permissions = true   # skip interactive permission prompts
```

## Inherited Pumpkin Features

Everything from upstream Pumpkin — Ember syncs weekly and keeps full compatibility:

- **Dual protocol** — Java Edition (TCP) and Bedrock Edition (UDP) on one server
- **World generation** — vanilla terrain, biomes, structures, lighting
- **Entities & AI** — mobs, animals, pathfinding, combat
- **Plugin system** — Native (`.dll`/`.so`) and WASM plugins with rich API
- **Commands** — 50+ vanilla commands with Brigadier-style dispatch
- **Proxy support** — BungeeCord and Velocity

## Fork Maintenance

See [EMBER.md](EMBER.md) for the full fork policy:

- `master` branch = upstream mirror (never committed to directly)
- `main` branch = upstream + Ember changes
- `sync-upstream.bat` = one-click merge from Pumpkin
- All changes in upstream files are wrapped in `EMBER start` / `EMBER end` markers
- grep `EMBER start` to list every deviation from upstream

## License

Upstream is GPLv3. All Ember changes are likewise released under [GPLv3](LICENSE).
