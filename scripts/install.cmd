@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\install-prebuilt.ps1" -PackageRoot "%~dp0."
if errorlevel 1 pause
