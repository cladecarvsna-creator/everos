#!/usr/bin/env bash
# Boot the ISO in QEMU without a display, wait for the kernel's serial
# message, sign in as root on the lock screen, wait for the desktop, then
# type commands on the emulated PS/2 keyboard and check that the shell in
# the terminal window ran them.
# A small web server on the host checks the network card, TCP/IP and
# HTTP: the guest reaches the host at 10.0.2.2 through QEMU's user network.
# Exits 0 if everything works, 1 otherwise.
set -u

iso="${1:-build/everos.iso}"
dir="$(mktemp -d)"
log="$dir/serial.log"
monitor="$dir/monitor.sock"
trap 'kill "$qemu" "$web" 2> /dev/null; rm -rf "$dir"' EXIT

mkdir "$dir/www"
echo '<html><head><title>EverOS test page</title></head><body><h1>It works</h1><a href="/x">x</a></body></html>' \
    > "$dir/www/index.html"
python3 -m http.server 8123 --bind 127.0.0.1 --directory "$dir/www" > /dev/null 2>&1 &
web=$!

timeout 90 qemu-system-x86_64 -cdrom "$iso" -m 256M -display none -serial "file:$log" \
    -monitor "unix:$monitor,server,nowait" -no-reboot -nic user,model=e1000 2> /dev/null &
qemu=$!

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

wait_for "login: lock screen" || fail "the lock screen did not show"
type_keys ret
wait_for "login: password prompt" || fail "the sign-in panel did not open"
# root has no password at first
type_keys ret
wait_for "login: signed in as root" || fail "could not sign in as root"
echo "signed in as root"

wait_for "desktop: opened Terminal" || fail "the desktop did not start"
echo "desktop started"

type_keys e c h o spc k e y b o a r d minus o k ret
wait_for "keyboard-ok" || fail "the shell did not answer typed input"
echo "keyboard input works"

type_keys f e t c h spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash ret
wait_for 'fetch: "EverOS test page"' || fail "the network test page did not load"
echo "network and HTTP work"

type_keys p a i n t ret
wait_for "desktop: opened Paint" || fail "the shell could not open Paint"
echo "apps open from the shell"
echo "boot test passed"
