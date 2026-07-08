# Ember 云端构建脚本 (Linux x64 + Windows x64)
# 通过 GitHub Actions 的 "Ember Build Release" 工作流在云端构建两个平台的服务端,
# 构建完成后自动下载到 dist\remote-<runId>\,并且工作流自身会自动创建一个新的
# GitHub Release(版本号从 0.01 起自动递增,附件是可直接下载运行的 ember/ember.exe)。
#
# 用法:
#   .\scripts\build-remote.ps1               # 用 origin/main 当前代码构建
#   .\scripts\build-remote.ps1 -Ref mybranch # 用指定分支构建
#
# 前提: 已安装 gh CLI 并 gh auth login;本地改动已推送到 GitHub
# (云端构建的是 GitHub 上的代码,不是本地工作区!先跑 push.bat)。

param(
    [string]$Ref = "main"
)

$ErrorActionPreference = 'Continue'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Fail($msg) {
    Write-Host ""
    Write-Host "[失败] $msg" -ForegroundColor Red
    exit 1
}

$workflow = "build-release.yml"
# 显式指定仓库,避免仓库有多个远程(origin/upstream)时 gh 报
# "No default remote repository has been set" 要求先 gh repo set-default
$repoSlug = "dongzh1/Ember"

Write-Host "=== Ember 云端构建 (Linux + Windows) ===" -ForegroundColor Cyan

# 0. 检查 gh
gh auth status 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "gh CLI 未登录,先运行: gh auth login" }

# 提醒: 本地是否有未推送的提交
$unpushed = git rev-list --count "origin/$Ref..$Ref" 2>$null
if ($LASTEXITCODE -eq 0 -and [int]$unpushed -gt 0) {
    Write-Host ""
    Write-Host "[警告] 本地 $Ref 有 $unpushed 个提交还没推送到 GitHub!" -ForegroundColor Yellow
    Write-Host "       云端构建的是 GitHub 上的代码。建议先跑 push.bat 再构建。" -ForegroundColor Yellow
    $answer = Read-Host "仍然继续? (y/N)"
    if ($answer -ne 'y') { exit 1 }
}

# 1. 记录触发前最新的 run id,用于识别新触发的 run
$prevId = gh run list -R $repoSlug --workflow $workflow --limit 1 --json databaseId --jq '.[0].databaseId' 2>$null
if (-not $prevId) { $prevId = "0" }

# 2. 触发工作流
Write-Host ""
Write-Host "[1/3] 触发工作流 $workflow (ref: $Ref)..." -ForegroundColor Cyan
gh workflow run $workflow -R $repoSlug --ref $Ref
if ($LASTEXITCODE -ne 0) { Fail "触发失败。首次使用需先把 .github/workflows/build-release.yml 推送到 GitHub。" }

# 3. 等待新 run 出现
Write-Host "等待 GitHub 创建构建任务..."
$runId = $null
foreach ($i in 1..12) {
    Start-Sleep -Seconds 5
    $latest = gh run list -R $repoSlug --workflow $workflow --limit 1 --json databaseId --jq '.[0].databaseId' 2>$null
    if ($latest -and $latest -ne $prevId) { $runId = $latest; break }
}
if (-not $runId) { Fail "60 秒内没等到新构建任务,请到 GitHub Actions 页面查看。" }

Write-Host "构建任务: https://github.com/dongzh1/Ember/actions/runs/$runId" -ForegroundColor Yellow

# 4. 等待构建完成 (Linux+Windows 全量 release 构建,无缓存时可能要 30-60 分钟)
Write-Host ""
Write-Host "[2/4] 等待云端构建完成 (Ctrl+C 可退出,之后可手动: gh run download $runId)..." -ForegroundColor Cyan
gh run watch $runId -R $repoSlug --exit-status --interval 30
if ($LASTEXITCODE -ne 0) {
    Fail "云端构建失败,查看日志: gh run view $runId -R $repoSlug --log-failed"
}

# 5. 下载产物 (工作流内部的 workflow artifact 副本,方便本机直接测试)
Write-Host ""
Write-Host "[3/4] 下载构建产物..." -ForegroundColor Cyan
$dest = Join-Path $repo "dist\remote-$runId"
New-Item -ItemType Directory -Force $dest | Out-Null
gh run download $runId -R $repoSlug --dir $dest
if ($LASTEXITCODE -ne 0) { Fail "下载失败,可手动重试: gh run download $runId -R $repoSlug --dir $dest" }

Write-Host ""
Write-Host "=== 构建完成,产物在 $dest ===" -ForegroundColor Green
Get-ChildItem -Recurse $dest -File | ForEach-Object {
    Write-Host ("  {0}  ({1} MB)" -f $_.FullName, [math]::Round($_.Length / 1MB, 1))
}

# 6. 工作流本身会在这次运行里自动发布一个新 GitHub Release (版本号自动递增)。
#    查一下刚发的是哪个版本,把直链打印出来,省得再去网页翻。
Write-Host ""
Write-Host "[4/4] 查询本次自动发布的 Release..." -ForegroundColor Cyan
$release = gh release list -R $repoSlug --limit 1 --json tagName,name,url -q '.[0]' 2>$null | ConvertFrom-Json
if ($release) {
    Write-Host ""
    Write-Host "=== 已发布: $($release.name) ===" -ForegroundColor Green
    Write-Host "  $($release.url)" -ForegroundColor Yellow
} else {
    Write-Host "[警告] 没查到 Release,去 Actions 页面确认一下 release job 是否成功。" -ForegroundColor Yellow
}
exit 0
