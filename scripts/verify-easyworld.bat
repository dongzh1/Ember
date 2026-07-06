@echo off
chcp 65001 >nul 2>&1
title Ember EasyWorld Verification

REM Prefer PowerShell 7 (pwsh) for native UTF-8 support.
REM Fall back to Windows PowerShell 5.x (the script is pure ASCII so it works on both).
where pwsh >nul 2>&1
if %errorlevel% equ 0 (
    echo Using pwsh (PowerShell 7+)
    pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0verify-easyworld.ps1" %*
) else (
    echo Using powershell (Windows PowerShell 5.x)
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0verify-easyworld.ps1" %*
)

echo.
pause
