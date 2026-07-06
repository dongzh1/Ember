# Ember 代码检查脚本
# 快速模式(默认): cargo fmt --check + clippy (Ember 常改的三个 crate)
# 完整模式(-Full): clippy 全 workspace + 全部测试
#
# 用法:
#   .\scripts\check.ps1          # 快速检查
#   .\scripts\check.ps1 -Full    # 完整检查(慢,提交大改动前跑一次)

param(
    [switch]$Full
)

$ErrorActionPreference = 'Continue'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$failed = @()

function Step($name, $cmd) {
    Write-Host ""
    Write-Host "=== $name ===" -ForegroundColor Cyan
    Write-Host "  $cmd" -ForegroundColor DarkGray
    Invoke-Expression $cmd
    if ($LASTEXITCODE -ne 0) {
        $script:failed += $name
        Write-Host "[未通过] $name" -ForegroundColor Red
    } else {
        Write-Host "[通过] $name" -ForegroundColor Green
    }
}

Write-Host "=== Ember 代码检查 ($(if ($Full) {'完整模式'} else {'快速模式'})) ===" -ForegroundColor Cyan

Step "格式检查 (cargo fmt)" "cargo fmt --check"

if ($Full) {
    Step "Clippy (全 workspace)" "cargo clippy --workspace --all-targets -- -D warnings"
    Step "测试 (全部)" "cargo test --workspace"
} else {
    # Ember 自有改动集中在这三个 crate,与 easyworld-ci.yml 保持一致
    Step "Clippy (pumpkin-config / pumpkin-world / pumpkin)" `
        "cargo clippy -p pumpkin-config -p pumpkin-world -p pumpkin --all-targets -- -D warnings"
}

Write-Host ""
if ($failed.Count -gt 0) {
    Write-Host "=== 检查未通过: $($failed -join ', ') ===" -ForegroundColor Red
    exit 1
}
Write-Host "=== 全部检查通过 ===" -ForegroundColor Green
exit 0
