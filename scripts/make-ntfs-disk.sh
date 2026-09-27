#!/usr/bin/env bash
# Make a test disk image with one NTFS partition, like a small Windows
# disk, for trying EverOS's NTFS support: folders, Russian names, a folder
# with hundreds of files, a compressed folder, a sparse file, a hidden
# file and the sample .exe programs.
#
#   scripts/make-ntfs-disk.sh build/ntfs.img
#   qemu-system-x86_64 ... -drive file=build/ntfs.img,format=raw,if=ide,index=1,media=disk
#
# Needs mkntfs and ntfs-3g (apt install ntfs-3g attr fdisk) and FUSE, so
# run it as root.
set -eu

out="${1:-build/ntfs.img}"
size_mb=64
dir="$(mktemp -d)"
trap 'umount "$dir/mnt" 2> /dev/null || true; rm -rf "$dir"' EXIT
part="$dir/part.img"
mkdir "$dir/mnt"

truncate -s ${size_mb}M "$part"
mkntfs -F -Q -q -L Windows -p 2048 -H 16 -S 63 "$part" 2> /dev/null

ntfs-3g -o compression "$part" "$dir/mnt"
m="$dir/mnt"
mkdir -p "$m/Windows/System32" "$m/Program Files/Samples" "$m/Users/JACK/Документы" "$m/Many files"
echo "Привет из NTFS! Этот файл лежит на диске D:." > "$m/Users/JACK/Документы/Заметка.txt"
printf 'EverOS reads NTFS disks.\r\nThis file is on an NTFS partition.\r\n' > "$m/readme.txt"
for i in $(seq 1 300); do echo "file number $i" > "$m/Many files/file_$i.txt"; done
python3 -c "import sys; sys.stdout.write(''.join('line %d of a longer text file\n' % i for i in range(20000)))" > "$m/Windows/log.txt"
head -c 3000000 /dev/urandom > "$m/Windows/System32/random.bin"
# a folder whose files NTFS keeps compressed (LZNT1)
mkdir "$m/Compressed"
setfattr -n system.ntfs_attrib_be -v 0x00000800 "$m/Compressed"
cp "$m/Windows/log.txt" "$m/Compressed/log.txt"
# a sparse file: 5 MiB of holes and a line at the end
truncate -s 5M "$m/sparse.bin"
echo "end of the sparse file" >> "$m/sparse.bin"
echo "you should not see this in Explorer" > "$m/hidden.txt"
setfattr -n system.ntfs_attrib_be -v 0x00000002 "$m/hidden.txt"
for exe in samples/exe/*.exe; do
    [ -f "$exe" ] && cp "$exe" "$m/Program Files/Samples/"
done
umount "$m"

# put the partition in a disk with an MBR, starting at 1 MiB
mkdir -p "$(dirname "$out")"
rm -f "$out"
truncate -s $((size_mb + 2))M "$out"
printf 'label: dos\nstart=2048, size=%d, type=7\n' $((size_mb * 2048)) | sfdisk -q "$out"
dd if="$part" of="$out" bs=512 seek=2048 conv=notrunc status=none
echo "made $out"
