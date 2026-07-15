@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\apply-ergouzi-branding.ps1" %*
exit /b %ERRORLEVEL%
