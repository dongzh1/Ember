@echo off
chcp 65001 >nul
title Ember Check
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\check.ps1" %*
echo.
pause
