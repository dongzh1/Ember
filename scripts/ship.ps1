# Ember 一条龙发布脚本 (ship)
#
# 顺序: 检查代码 -> 提交并推送 origin -> 同步上游 Pumpkin -> 云端构建 Linux+Windows
#
# 它只是"编排器": 依次调用现有子脚本 (check / push / sync-upstream / build-remote),
# 每个子脚本在独立的 PowerShell 子进程里跑, 所以它们内部的 exit 不会误杀本流水线。
# 任一步失败即中止并保留现场; 各子脚本仍可单独使用, 逻辑不重复。
#
# 用法:
#   .\scripts\ship.ps1                          # 交互: 询问提交信息, 走全流程
#   .\scripts\ship.ps1 -Message "[EMBER] feat: x"
#   .\scripts\ship.ps1 -Full                    # 检查用完整模式 (全 workspace clippy + 测试)
#   .\scripts\ship.ps1 -NoSync                  # 跳过上游同步
#   .\scripts\ship.ps1 -NoBuild                 # 不触发云端构建 (只 检查+推送+同步)
#   .\scripts\ship.ps1 -SkipCheck               # 跳过代码检查 (不建议)
#   .\scripts\ship.ps1 -Ref main                # 云端构建用的分支 (默认 main)

param(
    [string]$Message,
    [string]$Ref = "main",
    [switch]$Full,
    [switch]$NoSync,
    [switch]$NoBuild,
    [switch]$SkipCheck
)

$ErrorActionPreference = 'Continue'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$here = $PSScriptRoot
$repo = Split-Path -Parent $here
Set-Location $repo

# 用 powershell.exe 跑子脚本, 隔离它们的 exit (否则 exit 会终止本进程)。
$psExe = "powershell"

# 先算出启用了哪些步骤, 好显示 [N/总数]。
$steps = @()
if (-not $SkipCheck) { $steps += "检查代码" }
$steps += "推送 origin"
if (-not $NoSync)  { $steps += "同步上游" }
if (-not $NoBuild) { $steps += "云端构建" }
$script:total = $steps.Count
$script:idx = 0

function Banner($title) {
    $script:idx++
    Write-Host ""
    Write-Host ("=" * 64) -ForegroundColor DarkCyan
    Write-Host ("  [$($script:idx)/$($script:total)]  $title") -ForegroundColor Cyan
    Write-Host ("=" * 64) -ForegroundColor DarkCyan
}

function Abort($title, $code) {
    Write-Host ""
    Write-Host "[中止] 「$title」失败 (退出码 $code)。流水线停止; 已完成的步骤不回滚。" -ForegroundColor Red
    Write-Host "       修好后重跑 ship.bat, 或单独跑对应子脚本继续。" -ForegroundColor Yellow
    exit $code
}

# 跑一个子脚本: 输出直接流到控制台 (不捕获), 只读 $LASTEXITCODE 判成败。
function Step($title, $scriptName, [string[]]$subArgs) {
    Banner $title
    $path = Join-Path $here $scriptName
    & $psExe -NoProfile -ExecutionPolicy Bypass -File $path @subArgs
    $code = $LASTEXITCODE
    if ($code -ne 0) { Abort $title $code }
}

Write-Host "=== Ember 一条龙发布 (ship) ===" -ForegroundColor Green
Write-Host "流程: $($steps -join '  ->  ')"

# master 是上游纯镜像, 不允许在此发布。
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -eq "master") {
    Write-Host ""
    Write-Host "[中止] 当前在 master (上游镜像分支), 不能在此发布。先: git checkout main" -ForegroundColor Red
    exit 1
}
Write-Host "分支: $branch"

# 1. 检查代码 (fmt + clippy; -Full 再加全 workspace + 测试)
if (-not $SkipCheck) {
    $a = @()
    if ($Full) { $a += "-Full" }
    Step "检查代码 (cargo fmt + clippy$(if ($Full) {' + 测试'}))" "check.ps1" $a
}

# 2. 提交并推送 (已经检查过, 让 push 跳过重复的 fmt 检查)
$a = @("-NoCheck")
if ($Message) { $a += @("-Message", $Message) }
Step "提交并推送到 origin/$branch" "push.ps1" $a

# 3. 同步上游 Pumpkin -> master 镜像 -> 合并进 main -> 推送
if (-not $NoSync) {
    Step "同步上游 Pumpkin (有冲突会打印冲突报告并停下)" "sync-upstream.ps1" @()
}

# 4. 云端构建 Linux + Windows (GitHub Actions), 完成后下载到 dist\remote-<runId>\
if (-not $NoBuild) {
    Step "云端构建 Linux + Windows (GitHub Actions)" "build-remote.ps1" @("-Ref", $Ref)
}

Write-Host ""
Write-Host "=== 一条龙完成: $($steps -join ' / ') 全部通过 ===" -ForegroundColor Green
exit 0
