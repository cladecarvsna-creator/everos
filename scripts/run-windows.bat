@echo off
rem Run EverOS in QEMU on Windows. Put this file next to everos.iso.
rem EverOS is installed on everos-disk.vhd next to it, which is made on the
rem first run: the ISO starts the installer, and once EverOS is installed
rem it starts from the disk. "run-windows.bat setup" starts the installer
rem again, for example to update EverOS with a new ISO and keep the files.
rem Windows opens the disk too: double-click the .vhd while EverOS is not
rem running, and eject it before starting EverOS again.
setlocal
cd /d "%~dp0"
set QEMU=qemu-system-x86_64.exe
set QEMU_IMG=qemu-img.exe
where %QEMU% >nul 2>nul || set QEMU="C:\Program Files\qemu\qemu-system-x86_64.exe"
where %QEMU_IMG% >nul 2>nul || set QEMU_IMG="C:\Program Files\qemu\qemu-img.exe"
if not exist everos-disk.vhd %QEMU_IMG% create -f vpc -o subformat=fixed everos-disk.vhd 128M
set DISK=
if exist everos-disk.vhd set DISK=-drive file=everos-disk.vhd,format=vpc,if=ide,index=0,media=disk
rem the hard disk first, then the ISO; with "setup" the ISO first
set BOOT=order=cd,menu=on
if /i "%~1"=="setup" set BOOT=order=dc,menu=on
%QEMU% -cdrom everos.iso -boot %BOOT% -m 512M -serial stdio -rtc base=localtime -nic user,model=e1000 %DISK%
