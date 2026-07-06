@echo off
chcp 65001 >nul
title Ember Push
cd /d "%~dp0"

echo ============================================
echo  Ember Push
echo ============================================
echo.

git status --short
echo.

REM Use first argument as commit message, or default
set "MSG=%~1"
if "%MSG%"=="" set "MSG=[EMBER] update"

echo Commit: %MSG%
echo.
echo Press any key to push, or Ctrl+C to cancel...
pause >nul

git add -A
git commit -m "%MSG%"
git push origin main

echo.
echo Done!
pause
