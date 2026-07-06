# Ember 推送脚本
# 提交本地改动并推送到 origin (你的 GitHub 仓库 dongzh1/Ember)。
# 推送前默认跑一次快速格式检查,避免把明显坏的代码推上去。
#
# 用法:
#   .\scripts\push.ps1                          # 交互式:显示改动,询问提交信息
#   .\scripts\push.ps1 -Message "[EMBER] feat: xxx"
#   .\scripts\push.ps1 -NoCheck                 # 跳过格式检查

param(
    [string]$Message,
    [switch]$NoCheck
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

Write-Host "=== Ember 推送到 GitHub ===" -ForegroundColor Cyan

$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -eq "master") {
    Fail "当前在 master (上游纯镜像分支),禁止直接提交!请切到 main: git checkout main"
}
Write-Host "分支: $branch"

# 1. 显示改动
$dirty = git status --porcelain
if ($dirty) {
    Write-Host ""
    git status --short
    Write-Host ""

    # 2. 快速格式检查 (只在有代码改动时)
    if (-not $NoCheck) {
        Write-Host "[检查] cargo fmt --check ..." -ForegroundColor Cyan
        cargo fmt --check
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[警告] 格式检查未通过 (可运行 cargo fmt 自动修复)。" -ForegroundColor Yellow
            $answer = Read-Host "仍然提交? (y/N)"
            if ($answer -ne 'y') { exit 1 }
        } else {
            Write-Host "[通过] 格式检查" -ForegroundColor Green
        }
    }

    # 3. 提交
    if (-not $Message) {
        $Message = Read-Host "提交信息 (直接回车 = '[EMBER] update')"
        if (-not $Message) { $Message = "[EMBER] update" }
    }
    Write-Host ""
    Write-Host "提交: $Message" -ForegroundColor Cyan
    git add -A
    git commit -m "$Message"
    if ($LASTEXITCODE -ne 0) { Fail "git commit 失败。" }
} else {
    Write-Host "工作区干净,没有新改动,只同步已有提交到云端。" -ForegroundColor Yellow
}

# 4. 推送
Write-Host ""
Write-Host "推送 $branch 到 origin..." -ForegroundColor Cyan
git push -u origin $branch
if ($LASTEXITCODE -ne 0) { Fail "推送失败,检查网络后重试: git push origin $branch" }

Write-Host ""
Write-Host "=== 推送完成: origin/$branch 已更新 ===" -ForegroundColor Green
exit 0
