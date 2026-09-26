//! EverOS kernel. `kernel_main` is called from `boot/long_mode.asm`
//! once the CPU is in 64-bit long mode.

#![no_std]

mod serial;
mod vga;

use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn kernel_main(_multiboot_info: usize) -> ! {
    vga::clear();
    vga::write_line(0, "EverOS", vga::Color::LightGreen);
    vga::write_line(1, "Hello from the Rust kernel!", vga::Color::White);

    serial::init();
    serial::write_str("EverOS: kernel started\n");

    halt()
}

fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial::write_str("EverOS: kernel panic\n");
    vga::write_line(24, "KERNEL PANIC", vga::Color::LightRed);
    halt()
}
