# Ember 本地构建脚本 (Windows x64)
# cargo release 构建 -> 打包到 dist\ember-<commit>-windows-x86_64.zip
#
# 用法:
#   .\scripts\build-windows.ps1              # 构建并打包
#   .\scripts\build-windows.ps1 -SkipBuild   # 只打包(复用上次构建产物)
#
# Linux 包请用 build-remote.ps1 (云端构建,本机没有交叉编译环境)。

param(
    [switch]$SkipBuild
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

Write-Host "=== Ember 本地构建 (Windows x64) ===" -ForegroundColor Cyan

if (-not $SkipBuild) {
    Write-Host ""
    Write-Host "[1/2] cargo build --release -p pumpkin (首次构建可能要十几分钟)..." -ForegroundColor Cyan
    cargo build --release -p pumpkin
    if ($LASTEXITCODE -ne 0) { Fail "构建失败,请先修复编译错误 (可先跑 scripts\check.ps1)。" }
}

$exe = Join-Path $repo "target\release\pumpkin.exe"
if (-not (Test-Path $exe)) { Fail "找不到 $exe,请先构建。" }

Write-Host ""
Write-Host "[2/2] 打包..." -ForegroundColor Cyan

$commit = (git rev-parse --short HEAD).Trim()
$dist = Join-Path $repo "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

$stage = Join-Path $dist "_stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item $exe (Join-Path $stage "pumpkin.exe")

@(
    "Ember build"
    "commit:  $commit"
    "branch:  $((git rev-parse --abbrev-ref HEAD).Trim())"
    "date:    $(Get-Date -Format 'yyyy-MM-dd HH:mm') (local)"
    "target:  ember-windows-x86_64"
) | Out-File (Join-Path $stage "BUILD-INFO.txt") -Encoding utf8

$zip = Join-Path $dist "ember-$commit-windows-x86_64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
Remove-Item -Recurse -Force $stage

$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "=== 打包完成: $zip ($sizeMb MB) ===" -ForegroundColor Green
Write-Host "解压后运行 pumpkin.exe 启动服务端。" -ForegroundColor Yellow
exit 0
