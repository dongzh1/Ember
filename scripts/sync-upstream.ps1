# Ember 上游同步脚本
# 流程: fetch upstream -> master 镜像 ff -> 推 origin -> master 合并进 main -> 推 origin
# 冲突时: 打印冲突报告并保留合并中状态, 不自动解决

$ErrorActionPreference = 'Continue'
try {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
} catch {}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Fail($msg) {
    Write-Host ""
    Write-Host "[失败] $msg" -ForegroundColor Red
    exit 1
}

function Show-ConflictReport {
    Write-Host ""
    Write-Host "================ 冲突报告 ================" -ForegroundColor Red
    $conflicts = git diff --name-only --diff-filter=U
    foreach ($f in $conflicts) {
        $hasMarker = $false
        if (Test-Path $f) {
            $hasMarker = Select-String -Path $f -Pattern "EMBER start" -Quiet
        }
        if ($hasMarker) {
            Write-Host "  [含 EMBER 标记块] $f" -ForegroundColor Yellow
        } else {
            Write-Host "  [纯上游冲突]     $f" -ForegroundColor Magenta
        }
    }
    Write-Host ""
    Write-Host "下一步:" -ForegroundColor Cyan
    Write-Host "  1. 逐个打开上述文件解决 <<<<<<< 冲突"
    Write-Host "     - EMBER start/end 块内: 保留我们的逻辑"
    Write-Host "     - 块外: 跟随上游 (master) 的版本"
    Write-Host "  2. git add -A ; git commit"
    Write-Host "     (或到 Claude Code 里说: 帮我解决 Ember 合并冲突)"
    Write-Host "  3. git push origin main"
    Write-Host "=========================================" -ForegroundColor Red
}

function Update-UpstreamMirror {
    # 把上游 README (master 分支) 镜像到 PUMPKIN_README.md, 带自动生成头部
    $header = @'
<!--
  自动生成，请勿手动编辑。
  AUTO-GENERATED — DO NOT EDIT.
  本文件是上游 Pumpkin README 的镜像，由 sync-upstream.bat 每次同步时从 `master` 分支自动刷新。
  This file mirrors upstream Pumpkin's README and is refreshed from the `master`
  branch on every run of sync-upstream.bat.
-->

> 📖 **上游 Pumpkin 的 README 镜像 / Upstream Pumpkin README (mirror)**
> 来源 Source: [Pumpkin-MC/Pumpkin](https://github.com/Pumpkin-MC/Pumpkin/blob/master/README.md)
> 返回 Ember 说明 / Back to Ember: [README.md](README.md)
>
> 下面是上游原文，随每次上游同步自动更新。Ember 自己的说明请看 [README.md](README.md)。

---

'@
    $upstream = (git show master:README.md) -join "`n"
    $content = $header + $upstream + "`n"
    $path = Join-Path $repo "PUMPKIN_README.md"
    [System.IO.File]::WriteAllText($path, $content, (New-Object System.Text.UTF8Encoding $false))
}

Write-Host "=== Ember 上游同步 ===" -ForegroundColor Cyan
Write-Host "仓库: $repo"

# 让上游对 README.md 的改动在合并时自动保留我方版本 (配合 .gitattributes 的 merge=ours)
git config merge.ours.driver true

# 上次合并没收尾的话, 直接再报一次冲突
if (Test-Path (Join-Path $repo ".git\MERGE_HEAD")) {
    Write-Host ""
    Write-Host "检测到未完成的合并 (上次冲突还没解决)。" -ForegroundColor Yellow
    Show-ConflictReport
    exit 1
}

# 0. 工作区必须干净 (父仓库的已跟踪文件)
# --ignore-submodules=dirty: 忽略子模块内部未提交的改动 (如 pumpkin-plugin-wit 的
# mannequin WIT WIP)。上游同步只合并父仓库的 master/main, 子模块内部 WIP 与之无关,
# 由老大自管, 不该挡同步。gitlink 被提交移动过仍算改动 (dirty 级不忽略 M)。
$dirty = git status --porcelain --ignore-submodules=dirty
if ($dirty) {
    git status --short --ignore-submodules=dirty
    Fail "工作区有未提交的改动 (父仓库已跟踪文件), 请先提交或 stash 再同步。"
}

# 1. 拉取上游
Write-Host ""
Write-Host "[1/4] 拉取上游 Pumpkin..." -ForegroundColor Cyan
git fetch upstream
if ($LASTEXITCODE -ne 0) { Fail "git fetch upstream 失败, 检查网络。" }

# 2. 更新 master 镜像并推送
Write-Host ""
Write-Host "[2/4] 更新 master 镜像分支..." -ForegroundColor Cyan
git checkout -q master
if ($LASTEXITCODE -ne 0) { Fail "切换 master 失败。" }
$before = git rev-parse master
git merge --ff-only upstream/master
if ($LASTEXITCODE -ne 0) {
    Fail "master 无法 fast-forward! master 必须是上游纯镜像、不允许直接提交, 需要手工修复。"
}
$after = git rev-parse master
$newCount = [int](git rev-list --count "$before..$after")

git push -u origin master
if ($LASTEXITCODE -ne 0) { Fail "推送 master 到 origin 失败。" }

if ($newCount -eq 0) {
    git checkout -q main
    Write-Host ""
    Write-Host "上游没有新提交。同步 main 到云端..." -ForegroundColor Green
    git push -u origin main
    if ($LASTEXITCODE -ne 0) { Fail "推送 main 到 origin 失败。" }
    Write-Host "=== 同步完成: 上游无更新, main 已同步到云端 ===" -ForegroundColor Green
    exit 0
}

Write-Host ""
Write-Host "上游新增 $newCount 个提交:" -ForegroundColor Yellow
git log --oneline "$before..$after" | Select-Object -First 20
if ($newCount -gt 20) { Write-Host "  ... 其余 $($newCount - 20) 个省略" }

# 3. 合并进 main
Write-Host ""
Write-Host "[3/4] 合并 master -> main..." -ForegroundColor Cyan
git checkout -q main
if ($LASTEXITCODE -ne 0) { Fail "切换 main 失败。" }
git merge master --no-edit
if ($LASTEXITCODE -ne 0) {
    Show-ConflictReport
    exit 1
}

# 刷新上游 README 镜像 (PUMPKIN_README.md), 让它随上游一起更新
Update-UpstreamMirror
git add PUMPKIN_README.md
git diff --cached --quiet
if ($LASTEXITCODE -ne 0) {
    git commit -q -m "[EMBER] docs: refresh upstream Pumpkin README mirror"
    Write-Host "已刷新上游 README 镜像: PUMPKIN_README.md" -ForegroundColor Green
}

# 子模块指针可能随上游移动
git submodule update --init --recursive

# 4. 推送 main
Write-Host ""
Write-Host "[4/4] 推送 main 到 origin..." -ForegroundColor Cyan
git push -u origin main
if ($LASTEXITCODE -ne 0) { Fail "推送 main 到 origin 失败 (合并已完成, 稍后手动 git push origin main 即可)。" }

Write-Host ""
Write-Host "=== 同步完成: 合入 $newCount 个上游提交, master/main 已同步到云端 ===" -ForegroundColor Green
Write-Host "建议有空跑一次: cargo check -p pumpkin  确认编译无恙" -ForegroundColor Yellow
exit 0
