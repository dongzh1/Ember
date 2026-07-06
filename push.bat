@echo off
chcp 65001 >nul
title Ember Push
cd /d "%~dp0"
if "%~1"=="" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\push.ps1"
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\push.ps1" -Message "%~1"
)
echo.
pause
