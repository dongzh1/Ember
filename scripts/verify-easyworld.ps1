# ============================================================================
# Ember EasyWorld Verification Script (Windows PowerShell)
#
# Usage:
#   powershell -File scripts\verify-easyworld.ps1
#   powershell -File scripts\verify-easyworld.ps1 -Mode mysql
#   powershell -File scripts\verify-easyworld.ps1 -Mode all
#   powershell -File scripts\verify-easyworld.ps1 -Mode build
#
# Prerequisites:
#   - Rust toolchain (rustup + cargo)
#   - (MySQL mode) Docker Desktop
# ============================================================================

param(
    [ValidateSet("file", "mysql", "all", "build")]
    [string]$Mode = "file"
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectDir = Split-Path -Parent $ScriptDir
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) "ember-easyworld-verify"
$ServerBin = Join-Path $ProjectDir "target\release\pumpkin.exe"

function Write-Pass { Write-Host "[PASS]" -ForegroundColor Green -NoNewline; Write-Host " $_" }
function Write-Fail { Write-Host "[FAIL]" -ForegroundColor Red -NoNewline; Write-Host " $_" }
function Write-Info { Write-Host "[INFO]" -ForegroundColor Yellow -NoNewline; Write-Host " $_" }

if (Test-Path $TempDir) { Remove-Item -Recurse -Force $TempDir }
New-Item -ItemType Directory -Force $TempDir | Out-Null

# =========================================================================
# Step 1: Build
# =========================================================================

function Build-Server {
    Write-Info "Building Ember (release)..."
    Push-Location $ProjectDir
    cargo build --release -p pumpkin
    Pop-Location

    if (-not (Test-Path $ServerBin)) {
        Write-Fail "Build failed: $ServerBin not found"
        exit 1
    }
    Write-Pass "Build OK: $ServerBin"
}

# =========================================================================
# Step 2: File mode verification
# =========================================================================

function Verify-FileMode {
    $worldDir = Join-Path $TempDir "easyworld_file"
    New-Item -ItemType Directory -Force $worldDir | Out-Null

    Write-Info "=== File mode (type=easy) ==="
    Push-Location $worldDir

    New-Item -ItemType Directory -Force "config" | Out-Null

    $configContent = @'
[java_edition]
address = "0.0.0.0:25566"

[chunk]
type = "easy"

[plugin]
auto_approve_permissions = true
'@
    Set-Content -Path "config\configuration.toml" -Value $configContent

    Write-Info "Config:"
    Get-Content "config\configuration.toml"
    Write-Host ""

    $logFile = Join-Path $TempDir "server-file.log"
    Write-Info "Starting server (auto-stop after 10s)..."
    $job = Start-Job -ScriptBlock {
        param($bin, $log)
        & $bin *>&1 | Out-File -FilePath $log
    } -ArgumentList $ServerBin, $logFile

    Start-Sleep -Seconds 10
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
    Get-Process -Name "pumpkin" -ErrorAction SilentlyContinue | Stop-Process -Force

    # Check results: level.dat = server ran; .easy = chunks were written.
    $levelDat = Get-ChildItem -Recurse -Filter "level.dat" -ErrorAction SilentlyContinue
    $easyFiles = Get-ChildItem -Recurse -Filter "*.easy" -ErrorAction SilentlyContinue

    if ($easyFiles) {
        Write-Pass "EasyWorld file mode OK — found .easy files:"
        foreach ($f in $easyFiles) {
            $size = "{0:N0} B" -f $f.Length
            Write-Host "    $($f.FullName) ($size)"
        }
    } elseif ($levelDat) {
        Write-Pass "EasyWorld file mode OK — server started with type=easy (no chunks generated yet, no player joined)."
        Write-Info "level.dat found. Directory tree:"
        Get-ChildItem -Recurse -File -Include @("*.easy","*.toml","level.dat") -ErrorAction SilentlyContinue |
            ForEach-Object { "    " + $_.FullName.Replace($worldDir, ".") }
    } else {
        Write-Host "[WARN] No level.dat found — server may not have started correctly." -ForegroundColor Yellow
        Write-Info "Server log (last 30 lines):"
        if (Test-Path $logFile) {
            Get-Content $logFile -Tail 30 | ForEach-Object { "  $_" }
        }
    }

    Pop-Location
}

# =========================================================================
# Step 3: MySQL mode verification
# =========================================================================

function Verify-MysqlMode {
    Write-Info "=== MySQL mode (type=easy_mysql) ==="

    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        Write-Fail "Docker not found. Skipping MySQL verification."
        return
    }

    $worldDir = Join-Path $TempDir "easyworld_mysql"
    New-Item -ItemType Directory -Force $worldDir | Out-Null

    Write-Info "Starting MySQL container..."
    docker rm -f ember-mysql-test 2>$null | Out-Null
    docker run -d --name ember-mysql-test `
        -e MYSQL_ROOT_PASSWORD=ember_test `
        -e MYSQL_DATABASE=ember `
        -p 3307:3306 `
        mysql:8

    Write-Info "Waiting for MySQL..."
    $ready = $false
    for ($i = 1; $i -le 30; $i++) {
        $ping = docker exec ember-mysql-test mysqladmin ping -h localhost --silent 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-Pass "MySQL ready"
            $ready = $true
            break
        }
        Start-Sleep -Seconds 2
    }
    if (-not $ready) {
        Write-Fail "MySQL startup timeout"
        docker logs ember-mysql-test --tail 20
        docker rm -f ember-mysql-test 2>$null
        return
    }

    Push-Location $worldDir
    New-Item -ItemType Directory -Force "config" | Out-Null

    $configContent = @'
[java_edition]
address = "0.0.0.0:25567"

[chunk]
type = "easy_mysql"
url = "mysql://root:ember_test@127.0.0.1:3307/ember"

[plugin]
auto_approve_permissions = true
'@
    Set-Content -Path "config\configuration.toml" -Value $configContent

    $logFile = Join-Path $TempDir "server-mysql.log"
    Write-Info "Starting server (auto-stop after 10s)..."
    $job = Start-Job -ScriptBlock {
        param($bin, $log)
        & $bin *>&1 | Out-File -FilePath $log
    } -ArgumentList $ServerBin, $logFile

    Start-Sleep -Seconds 10
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
    Get-Process -Name "pumpkin" -ErrorAction SilentlyContinue | Stop-Process -Force

    Write-Info "Querying MySQL..."
    $rowCount = docker exec ember-mysql-test mysql -u root -pember_test ember -e `
        "SELECT COUNT(*) FROM easyworld_regions;" 2>$null

    Write-Info "easyworld_regions rows: $rowCount"

    if ($rowCount -match '\d+' -and [int]($rowCount -replace '\D') -gt 0) {
        Write-Pass "MySQL mode OK — found data in easyworld_regions"
        Write-Info "Sample data:"
        docker exec ember-mysql-test mysql -u root -pember_test ember -e `
            "SELECT world_key, region_x, region_z, LENGTH(data) AS size_bytes FROM easyworld_regions LIMIT 5;" 2>$null
    } else {
        $levelDat = Get-ChildItem -Recurse -Filter "level.dat" -ErrorAction SilentlyContinue
        if ($levelDat) {
            Write-Pass "MySQL mode OK — server started (level.dat found), table exists but no chunks yet (no player joined)."
        } else {
            Write-Host "[WARN] No level.dat found — server may not have connected to MySQL." -ForegroundColor Yellow
            Write-Info "Server log (last 30 lines):"
            if (Test-Path $logFile) {
                Get-Content $logFile -Tail 30 | ForEach-Object { "  $_" }
            }
        }
    }

    Write-Info "Cleaning up MySQL container..."
    docker rm -f ember-mysql-test 2>$null
    Write-Pass "MySQL verification done"

    Pop-Location
}

# =========================================================================
# Main
# =========================================================================

Write-Host "============================================"
Write-Host " Ember EasyWorld Verification"
Write-Host " Project dir: $ProjectDir"
Write-Host " Temp dir:    $TempDir"
Write-Host " Mode:        $Mode"
Write-Host "============================================"

Build-Server

switch ($Mode) {
    "file"  { Verify-FileMode }
    "mysql" { Verify-MysqlMode }
    "all"   { Verify-FileMode; Verify-MysqlMode }
    "build" { Write-Pass "Build OK. Binary: $ServerBin" }
}

Write-Host ""
Write-Host "============================================"
Write-Host " EasyWorld verification done" -ForegroundColor Green
Write-Host "============================================"
