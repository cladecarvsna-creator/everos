#!/usr/bin/env bash
# Boot the ISO in QEMU without a display and wait for the kernel's
# serial message. Exits 0 if EverOS booted, 1 otherwise.
set -u

iso="${1:-build/everos.iso}"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT

timeout 20 qemu-system-x86_64 -cdrom "$iso" -display none -serial "file:$log" -no-reboot 2> /dev/null &
qemu=$!

for _ in $(seq 1 40); do
    if grep -q "EverOS: kernel started" "$log"; then
        kill "$qemu" 2> /dev/null
        echo "boot test passed: $(cat "$log")"
        exit 0
    fi
    sleep 0.5
done

kill "$qemu" 2> /dev/null
echo "boot test failed, serial output was:"
cat "$log"
exit 1
