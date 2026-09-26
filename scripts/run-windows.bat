@echo off
rem Run EverOS in QEMU on Windows. Put this file next to everos.iso.
setlocal
set QEMU=qemu-system-x86_64.exe
where %QEMU% >nul 2>nul || set QEMU="C:\Program Files\qemu\qemu-system-x86_64.exe"
cd /d "%~dp0"
%QEMU% -cdrom everos.iso -m 256M -serial stdio
