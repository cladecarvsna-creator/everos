#!/usr/bin/env bash
# Boot the ISO in QEMU without a display, wait for the kernel's serial
# message, sign in as root on the lock screen, wait for the desktop, then
# type commands on the emulated PS/2 keyboard and check that the shell in
# the terminal window ran them.
# A small web server on the host checks the network card, TCP/IP and
# HTTP: the guest reaches the host at 10.0.2.2 through QEMU's user network.
# A small proxy server (scripts/test-proxy.py) checks that pages load
# through HTTP and SOCKS5 proxies.
# The disc starts the installer. First "Try EverOS" runs it from the
# disc: a blank disk image checks the disk driver and FAT32 (EverOS
# formats it, the shell writes a file, and after starting QEMU again the
# file is still there). Then the installer puts EverOS on a second blank
# disk, and QEMU starts from that disk alone and signs in to the account
# the installer made.
# Exits 0 if everything works, 1 otherwise.
set -u

iso="${1:-build/everos.iso}"
dir="$(mktemp -d)"
log="$dir/serial.log"
monitor="$dir/monitor.sock"
disk="$dir/disk.img"
qemu=""
trap 'kill $qemu "$web" "$proxy" 2> /dev/null; rm -rf "$dir"' EXIT
truncate -s 64M "$disk"

mkdir "$dir/www"
echo '<html><head><title>EverOS test page</title></head><body><h1>It works</h1><a href="/x">x</a></body></html>' \
    > "$dir/www/index.html"
python3 -m http.server 8123 --bind 127.0.0.1 --directory "$dir/www" > /dev/null 2>&1 &
web=$!
python3 "$(dirname "$0")/test-proxy.py" 8124 > "$dir/proxy.log" 2>&1 &
proxy=$!

# start QEMU with the disk and the disc, or from the disk alone when $1
# is "disk"; the serial log starts empty
boot() {
    rm -f "$log" "$monitor"
    local media=(-cdrom "$iso" -boot d)
    if [ "${1:-}" = disk ]; then
        media=(-boot c)
    fi
    timeout 90 qemu-system-x86_64 "${media[@]}" -m 512M -display none \
        -serial "file:$log" -monitor "unix:$monitor,server,nowait" -no-reboot \
        -drive "file=$disk,format=raw,if=ide,index=0,media=disk" \
        -nic user,model=e1000 2> /dev/null &
    qemu=$!
}
boot

# wait for a line starting with $1 in the serial log
wait_for() {
    for _ in $(seq 1 60); do
        if grep -q "^$1" "$log" 2> /dev/null; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

fail() {
    echo "boot test failed: $1, serial output was:"
    cat "$log"
    exit 1
}

wait_for "EverOS: kernel started" || fail "the kernel did not start"
echo "kernel started"

# press keys on the emulated keyboard through the QEMU monitor
type_keys() {
    python3 - "$monitor" "$@" << 'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
for key in sys.argv[2:]:
    s.sendall(f"sendkey {key}\n".encode())
    time.sleep(0.1)
PY
}

sign_in() {
    wait_for "login: lock screen" || fail "the lock screen did not show"
    type_keys ret
    wait_for "login: password prompt" || fail "the sign-in panel did not open"
    # root has no password at first
    type_keys ret
    wait_for "login: signed in as root" || fail "could not sign in as root"
    echo "signed in as root"
}

# the installer's first page, then "Try EverOS without installing"
try_everos() {
    wait_for "setup: page language" || fail "the installer did not start"
    type_keys ret
    wait_for "setup: page start" || fail "the installer's start page did not show"
    type_keys tab ret
    wait_for "setup: trying EverOS" || fail "Try EverOS did not start EverOS"
}

quit_qemu() {
    python3 - "$monitor" << 'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(b"quit\n")
PY
    wait "$qemu" 2> /dev/null
}

try_everos
echo "the installer starts, Try EverOS works"
wait_for "fs: formatted a blank disk as FAT32" || fail "the blank disk was not formatted"
echo "disk formatted"
sign_in

wait_for "desktop: opened Terminal" || fail "the desktop did not start"
echo "desktop started"
wait_for "icons: loaded 12 pictures" || fail "the app icons did not load"
echo "app icons loaded"

type_keys e c h o spc k e y b o a r d minus o k ret
wait_for "keyboard-ok" || fail "the shell did not answer typed input"
echo "keyboard input works"

# the Telegram client's cryptography against known answers
type_keys t e l e g r a m spc s e l f t e s t ret
wait_for "telegram: self-test ok" || fail "the Telegram self-test failed"
echo "telegram cryptography works"

# WPA2: known answers, and a whole 4-way handshake
type_keys w i f i spc s e l f t e s t ret
wait_for "wifi: self-test ok" || fail "the WPA2 self-test failed"
echo "WPA2 cryptography works"

type_keys f e t c h spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash ret
wait_for 'fetch: "EverOS test page"' || fail "the network test page did not load"
echo "network and HTTP work"

# the same page through the proxy on the host: as a whole URL through
# the HTTP proxy, then a tunnel through SOCKS5. The proxy runs on the
# host, so the page is asked for at the host's own 127.0.0.1.
proxy_seen() {
    for _ in $(seq 1 30); do
        grep -q "$1" "$dir/proxy.log" && return 0
        sleep 0.5
    done
    return 1
}
type_keys p r o x y spc s e l f t e s t ret
wait_for "proxy: self-test ok" || fail "the proxy self-test failed"
type_keys p r o x y spc h t t p spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 4 ret
type_keys p r o x y spc b y p a s s ret
type_keys f e t c h spc 1 2 7 dot 0 dot 0 dot 1 shift-semicolon 8 1 2 3 slash ret
proxy_seen "http: GET http://127.0.0.1:8123/" || fail "the page did not go through the HTTP proxy"
echo "HTTP proxy works"
type_keys p r o x y spc s o c k s 5 spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 4 ret
type_keys f e t c h spc 1 2 7 dot 0 dot 0 dot 1 shift-semicolon 8 1 2 3 slash ret
proxy_seen "socks5: CONNECT 127.0.0.1:8123" || fail "the page did not go through the SOCKS5 proxy"
echo "SOCKS5 proxy works"
type_keys p r o x y spc o f f ret

# the browser loads pages on a fiber, in the background
type_keys b r o w s e r spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash ret
wait_for "browser: showing page" || fail "the browser did not show the test page"
echo "the browser loads pages in the background"
# back to the terminal through the taskbar search
type_keys meta_l-s
wait_for "search: indexed" || fail "the taskbar search did not open"
type_keys t e r m ret
sleep 1

# write a file on the disk, in root's home folder, and open it in
# Notepad (which then has the keyboard)
type_keys e c h o spc s a v e d minus o k spc shift-dot spc s a v e d dot t x t ret
type_keys n o t e p a d spc s a v e d dot t x t ret
wait_for "desktop: opened Notepad" || fail "the shell could not open Notepad"
echo "file written, apps open from the shell"

# start again from the same disk: the file must still be there
quit_qemu
if command -v mtype > /dev/null; then
    mtype -i "$disk@@1M" ::/Users/root/saved.txt | grep -q "saved-ok" \
        || fail "mtools could not read the file EverOS wrote"
    echo "mtools reads the file EverOS wrote"
fi
if command -v fsck.fat > /dev/null; then
    part="$dir/part.img"
    dd if="$disk" of="$part" bs=512 skip=2048 status=none
    fsck.fat -n "$part" > "$dir/fsck.log" 2>&1 || { cat "$dir/fsck.log"; fail "fsck.fat found errors"; }
    echo "fsck.fat finds no errors"
fi

boot
wait_for "fs: mounted FAT32 disk" || fail "the disk was not mounted again"
try_everos
sign_in
wait_for "desktop: opened Terminal" || fail "the desktop did not start again"
type_keys c a t spc s a v e d dot t x t ret
wait_for "saved-ok" || fail "the file was gone after restarting"
echo "files survive a restart"
type_keys s e t t i n g s ret
wait_for "desktop: opened Settings" || fail "the shell could not open Settings"
echo "Settings opens"

# search from the taskbar: Win+S, type, Enter opens the best match
type_keys meta_l-s
wait_for "search: indexed" || fail "the taskbar search did not open"
type_keys c a l c ret
wait_for "desktop: opened Calculator" || fail "search did not open Calculator"
echo "taskbar search works"

# virtual desktops: Win+Ctrl+D makes one, Win+Ctrl+Left goes back
type_keys ctrl-meta_l-d
wait_for "desktops: switched to Desktop 2" || fail "Win+Ctrl+D did not make a new desktop"
type_keys ctrl-meta_l-left
wait_for "desktops: switched to Desktop 1" || fail "Win+Ctrl+Left did not switch back"
type_keys meta_l-tab
wait_for "desktops: task view" || fail "Win+Tab did not open Task View"
echo "virtual desktops and Task View work"
quit_qemu

# install EverOS on a new blank disk with the installer
disk="$dir/install.img"
truncate -s 128M "$disk"
boot
wait_for "setup: page language" || fail "the installer did not start"
# language, Install, accept the license, the disk
type_keys ret
wait_for "setup: page start" || fail "the installer's start page did not show"
type_keys ret
wait_for "setup: page license" || fail "the license page did not show"
type_keys spc ret
wait_for "setup: page disk" || fail "the disk page did not show"
type_keys ret
wait_for "setup: page account" || fail "the account page did not show"
# the account: name, password twice
type_keys t e s t e r tab p w tab p w ret
wait_for "setup: page ready" || fail "the account was not accepted"
type_keys ret
wait_for "setup: done" || fail "the installation did not finish"
echo "the installer put EverOS on a blank disk"
quit_qemu
if command -v fsck.fat > /dev/null; then
    part="$dir/part.img"
    dd if="$disk" of="$part" bs=512 skip=2048 status=none
    fsck.fat -n "$part" > "$dir/fsck.log" 2>&1 || { cat "$dir/fsck.log"; fail "fsck.fat found errors on the installed disk"; }
fi

# start from the installed disk alone, without the disc
boot disk
wait_for "EverOS: kernel started" || fail "EverOS did not start from the installed disk"
wait_for "users: loaded 1 from disk" || fail "the installed accounts were not loaded"
wait_for "login: lock screen" || fail "the lock screen did not show on the installed system"
type_keys ret
wait_for "login: password prompt" || fail "the sign-in panel did not open"
type_keys p w ret
wait_for "login: signed in as tester" || fail "could not sign in to the installed account"
echo "EverOS starts from the installed disk and signs in to the new account"
echo "boot test passed"
