@echo off
chcp 65001 >nul
title Ember Build
cd /d "%~dp0"

echo ============================================
echo  Ember 构建打包
echo ============================================
echo.
echo   [1] 本地构建 Windows 包 (快,用于本机测试)
echo   [2] 云端构建 Linux + Windows 包 (GitHub Actions,用于部署)
echo.
choice /c 12 /n /m "请选择 [1/2]: "

if errorlevel 2 (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build-remote.ps1"
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build-windows.ps1"
)

echo.
pause
