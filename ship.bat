@echo off
chcp 65001 >nul
title Ember Ship
cd /d "%~dp0"
rem One-shot pipeline: check -> push -> sync-upstream -> cloud build.
rem Double-click = interactive (asks for commit message). Or pass args, e.g.:
rem   ship.bat "[EMBER] feat: xxx"   set commit message
rem   ship.bat -NoBuild             stop after check + push + sync
rem   ship.bat -Full                full check (clippy whole workspace + tests)
rem See scripts\ship.ps1 / scripts\README.md for full help (Chinese).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\ship.ps1" %*
echo.
pause
