//! Reading the multiboot2 information structure GRUB passes to the kernel.

use crate::framebuffer::{ColorField, Framebuffer};

const TAG_END: u32 = 0;
const TAG_CMDLINE: u32 = 1;
const TAG_BOOTLOADER_NAME: u32 = 2;
const TAG_MODULE: u32 = 3;
const TAG_BASIC_MEMINFO: u32 = 4;
const TAG_FRAMEBUFFER: u32 = 8;

const FRAMEBUFFER_TYPE_RGB: u8 = 1;
const MAX_MODULES: usize = 4;

/// A file GRUB loaded into memory next to the kernel (`module2` in
/// grub.cfg). The installer gets the files it copies to the disk this way.
#[derive(Clone, Copy)]
pub struct Module {
    pub name: &'static str,
    pub data: &'static [u8],
}

pub struct BootInfo {
    pub framebuffer: Option<Framebuffer>,
    pub bootloader: &'static str,
    /// Memory above 1 MiB, in KiB, as reported by the BIOS.
    pub upper_memory_kib: u32,
    /// What grub.cfg put after the kernel's path: "setup" on the
    /// installation disc.
    pub cmdline: &'static str,
    modules: [Option<Module>; MAX_MODULES],
}

impl BootInfo {
    /// Whether EverOS was started from the installation disc.
    pub fn setup(&self) -> bool {
        self.cmdline.split(' ').any(|w| w == "setup")
    }

    /// The module GRUB loaded under this name.
    pub fn module(&self, name: &str) -> Option<&'static [u8]> {
        self.modules
            .iter()
            .flatten()
            .find(|m| m.name == name)
            .map(|m| m.data)
    }
}

/// A zero-terminated string inside a tag.
unsafe fn c_str(addr: usize, max: usize) -> &'static str {
    let bytes = core::slice::from_raw_parts(addr as *const u8, max);
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..len]).unwrap_or("")
}

unsafe fn read<T: Copy>(addr: usize) -> T {
    (addr as *const T).read_unaligned()
}

/// # Safety
/// `info` must be the multiboot2 information address GRUB passed in ebx.
pub unsafe fn parse(info: usize) -> BootInfo {
    let mut boot = BootInfo {
        framebuffer: None,
        bootloader: "unknown",
        upper_memory_kib: 0,
        cmdline: "",
        modules: [None; MAX_MODULES],
    };
    let mut modules = 0;
    let total_size = read::<u32>(info) as usize;
    let mut tag = info + 8;
    while tag + 8 <= info + total_size {
        let kind = read::<u32>(tag);
        let size = read::<u32>(tag + 4) as usize;
        match kind {
            TAG_END => break,
            TAG_BOOTLOADER_NAME => boot.bootloader = c_str(tag + 8, size - 8),
            TAG_CMDLINE => boot.cmdline = c_str(tag + 8, size - 8),
            TAG_MODULE if modules < MAX_MODULES && size > 16 => {
                let start = read::<u32>(tag + 8) as usize;
                let end = read::<u32>(tag + 12) as usize;
                if end > start {
                    boot.modules[modules] = Some(Module {
                        name: c_str(tag + 16, size - 16),
                        data: core::slice::from_raw_parts(start as *const u8, end - start),
                    });
                    modules += 1;
                }
            }
            TAG_BASIC_MEMINFO => boot.upper_memory_kib = read::<u32>(tag + 12),
            TAG_FRAMEBUFFER if read::<u8>(tag + 29) == FRAMEBUFFER_TYPE_RGB => {
                let bpp = read::<u8>(tag + 28);
                if matches!(bpp, 16 | 24 | 32) {
                    let field = |offset: usize| ColorField {
                        position: read::<u8>(tag + offset),
                        size: read::<u8>(tag + offset + 1),
                    };
                    boot.framebuffer = Some(Framebuffer {
                        base: read::<u64>(tag + 8) as usize as *mut u8,
                        pitch: read::<u32>(tag + 16) as usize,
                        width: read::<u32>(tag + 20) as usize,
                        height: read::<u32>(tag + 24) as usize,
                        bytes_per_pixel: bpp as usize / 8,
                        red: field(32),
                        green: field(34),
                        blue: field(36),
                    });
                }
            }
            _ => {}
        }
        tag += (size + 7) & !7;
    }
    boot
}
