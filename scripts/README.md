# Ember 脚本目录

**一条龙(推荐):** 双击根目录 `ship.bat` —— 一次跑完 `检查 → 推送 → 同步上游 → 云端构建`,
任一步失败即停下并保留现场。

```
改代码 → ship.bat  (= check + push + sync-upstream + build-remote,顺序执行,失败即止)
```

想分步来 / 只跑其中一步时,用下面的单项脚本:

```
改代码 → check.bat → build.bat(本地测试) → push.bat → build.bat(云端打正式包)
                                              ↓
                                     每周: sync-upstream.bat
```

所有 `.bat` 在仓库根目录,双击即可;`.ps1` 是实际逻辑,放本目录。
`ship.ps1` 只是"编排器":在独立子进程里依次调用下面的单项脚本,不重复它们的逻辑。

## 脚本一览

| 入口(根目录) | 实际脚本 | 作用 |
|---|---|---|
| `ship.bat` | `ship.ps1` | **一条龙**:检查 → 推送 → 同步上游 → 云端构建,顺序执行、失败即止。是编排器,不含新逻辑 |
| `check.bat` | `check.ps1` | 代码检查:fmt + clippy(常改的三个 crate)。`-Full` 查全 workspace 并跑测试 |
| `build.bat` | `build-windows.ps1` / `build-remote.ps1` | 菜单选择:本地构建 Windows 包 / 云端构建 Linux+Windows 包 |
| `push.bat` | `push.ps1` | 提交 + 推送到 GitHub(origin/main),推送前自动做格式检查,禁止在 master 提交 |
| `sync-upstream.bat` | `sync-upstream.ps1` | 拉取上游 Pumpkin → 更新 master 镜像 → 合并进 main → 推送;有冲突时输出冲突报告 |
| — | `verify-easyworld.ps1/.sh/.bat` | EasyWorld 存储功能验证(文件模式 + MySQL 模式) |

## 打包说明

- **Windows 包**:本机 `cargo build --release` 直接出,产物在 `dist\ember-<commit>-windows-x86_64.zip`。
- **Linux 包**:本机没有交叉编译环境(无 Docker/WSL 发行版),走 GitHub Actions
  云端构建(`.github/workflows/build-release.yml`),`build-remote.ps1` 会触发、
  等待并下载产物到 `dist\remote-<runId>\`。
  注意:云端构建的是 **GitHub 上的代码**,先推送再构建。
- **发正式版**:不用手动打 tag —— 云端构建(`build-remote.ps1` / `ship.bat`)每跑一次,
  工作流自己就会创建一个新的 GitHub Release,版本号从 `0.01` 起自动 +0.01(扫描已有
  `ember-vX.XX` Release 取最大值算下一个)。附件是裸的 `ember`(Linux)/`ember.exe`
  (Windows),不再打包 zip/tar,下载下来就能跑(Linux 需要自己 `chmod +x ember`)。

## 命令行直接调用

```powershell
.\scripts\ship.ps1 [-Message "[EMBER] feat: xxx"] [-Full] [-NoSync] [-NoBuild] [-SkipCheck] [-Ref main]
.\scripts\check.ps1 [-Full]
.\scripts\build-windows.ps1 [-SkipBuild]
.\scripts\build-remote.ps1 [-Ref main]
.\scripts\push.ps1 [-Message "[EMBER] feat: xxx"] [-NoCheck]
.\scripts\sync-upstream.ps1
```

## 约定

- 提交信息带 `[EMBER]` 前缀(见 `EMBER.md` 提交规范)。
- `master` 是上游纯镜像,`push.ps1` 会拒绝在 master 上提交。
- 新脚本加进来时更新本 README 和 `EMBER.md` 的自有改动清单。
