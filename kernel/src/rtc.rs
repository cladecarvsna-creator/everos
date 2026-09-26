//! The CMOS real-time clock.

use crate::port::{inb, outb};

fn read(register: u8) -> u8 {
    unsafe {
        outb(0x70, register);
        inb(0x71)
    }
}

fn updating() -> bool {
    read(0x0a) & 0x80 != 0
}

/// Current time of day as (hours, minutes, seconds). QEMU's clock is
/// UTC unless it is started with `-rtc base=localtime`.
pub fn time() -> (u8, u8, u8) {
    // read twice until two reads agree, so an update in between
    // cannot give a torn value
    let sample = || {
        while updating() {
            core::hint::spin_loop();
        }
        (read(0x04), read(0x02), read(0x00))
    };
    let mut now = sample();
    loop {
        let again = sample();
        if again == now {
            break;
        }
        now = again;
    }
    let (mut h, m, s) = now;
    let status = read(0x0b);
    let bcd = |v: u8| (v & 0x0f) + (v >> 4) * 10;
    let pm = h & 0x80 != 0;
    h &= 0x7f;
    let (h, m, s) = if status & 0x04 == 0 {
        (bcd(h), bcd(m), bcd(s))
    } else {
        (h, m, s)
    };
    // 12 hour mode
    let h = if status & 0x02 == 0 {
        (h % 12) + if pm { 12 } else { 0 }
    } else {
        h
    };
    (h, m, s)
}
