# EverOS build: nasm (boot code) + cargo (Rust kernel) -> ld -> GRUB ISO.

BUILD      := build
KERNEL     := $(BUILD)/kernel.bin
ISO        := $(BUILD)/everos.iso
RUST_LIB   := kernel/target/x86_64-unknown-none/release/libeveros_kernel.a
ASM_SRC    := $(wildcard boot/*.asm)
ASM_OBJ    := $(patsubst boot/%.asm,$(BUILD)/boot/%.o,$(ASM_SRC))
QEMU       := qemu-system-x86_64

.PHONY: all iso run test clean kernel-lib

all: $(ISO)

iso: $(ISO)

$(BUILD)/boot/%.o: boot/%.asm
	@mkdir -p $(dir $@)
	nasm -f elf64 $< -o $@

kernel-lib:
	cd kernel && cargo build --release

$(RUST_LIB): kernel-lib

$(KERNEL): $(ASM_OBJ) $(RUST_LIB) linker.ld
	ld -n --gc-sections -z noexecstack --no-warn-rwx-segments -T linker.ld -o $@ $(ASM_OBJ) $(RUST_LIB)
	grub-file --is-x86-multiboot2 $@

$(ISO): $(KERNEL) iso/boot/grub/grub.cfg
	@mkdir -p $(BUILD)/iso/boot/grub
	cp $(KERNEL) $(BUILD)/iso/boot/kernel.bin
	cp iso/boot/grub/grub.cfg $(BUILD)/iso/boot/grub/grub.cfg
	grub-mkrescue -o $@ $(BUILD)/iso 2> /dev/null

# Boot EverOS in a QEMU window. Serial output goes to the terminal, and
# the taskbar clock shows local time.
run: $(ISO)
	$(QEMU) -cdrom $(ISO) -m 256M -serial stdio -rtc base=localtime

# Boot headless and check that the kernel reached Rust code.
test: $(ISO)
	./scripts/boot-test.sh $(ISO)

clean:
	rm -rf $(BUILD)
	cd kernel && cargo clean
