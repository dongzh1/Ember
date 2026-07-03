@echo off
chcp 65001 >nul
title Ember 上游同步
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\sync-upstream.ps1"
echo.
pause
