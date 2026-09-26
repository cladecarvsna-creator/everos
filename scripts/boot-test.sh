#!/usr/bin/env bash
# Boot the ISO in QEMU without a display, wait for the kernel's serial
# message, then type a command on the emulated PS/2 keyboard and check
# that the shell ran it. Exits 0 if both work, 1 otherwise.
set -u

iso="${1:-build/everos.iso}"
dir="$(mktemp -d)"
log="$dir/serial.log"
monitor="$dir/monitor.sock"
trap 'kill "$qemu" 2> /dev/null; rm -rf "$dir"' EXIT

timeout 60 qemu-system-x86_64 -cdrom "$iso" -display none -serial "file:$log" \
    -monitor "unix:$monitor,server,nowait" -no-reboot 2> /dev/null &
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

# type "echo keyboard-ok" and Enter through the QEMU monitor
python3 - "$monitor" << 'PY'
import socket, sys, time
keys = list("echo") + ["spc"] + list("keyboard") + ["minus"] + list("ok") + ["ret"]
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
for key in keys:
    s.sendall(f"sendkey {key}\n".encode())
    time.sleep(0.1)
PY

wait_for "keyboard-ok" || fail "the shell did not answer typed input"
echo "keyboard input works"
echo "boot test passed"
