; Multiboot2 header: tells GRUB that this file is a kernel it can load.
section .multiboot_header
header_start:
    dd 0xe85250d6                ; multiboot2 magic number
    dd 0                         ; architecture: i386 (protected mode)
    dd header_end - header_start ; header length
    ; checksum: magic + architecture + length + checksum must equal 0
    dd 0x100000000 - (0xe85250d6 + 0 + (header_end - header_start))

    ; end tag
    dw 0
    dw 0
    dd 8
header_end:
