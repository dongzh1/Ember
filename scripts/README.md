# Ember 脚本目录

日常改完代码后的完整流程(推荐顺序):

```
改代码 → check.bat → build.bat(本地测试) → push.bat → build.bat(云端打正式包)
                                              ↓
                                     每周: sync-upstream.bat
```

所有 `.bat` 在仓库根目录,双击即可;`.ps1` 是实际逻辑,放本目录。

## 脚本一览

| 入口(根目录) | 实际脚本 | 作用 |
|---|---|---|
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
- **发正式版**:打 `ember-*` 标签推上去,工作流自动构建并创建 GitHub Release:

  ```
  git tag ember-v0.1.0
  git push origin ember-v0.1.0
  ```

## 命令行直接调用

```powershell
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
