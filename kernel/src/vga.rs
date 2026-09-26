//! Minimal VGA text mode output (80x25, buffer at 0xb8000).

const BUFFER: *mut u16 = 0xb8000 as *mut u16;
const WIDTH: usize = 80;
const HEIGHT: usize = 25;

#[allow(dead_code)]
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Color {
    Black = 0x0,
    LightGreen = 0xa,
    LightRed = 0xc,
    White = 0xf,
}

fn put(row: usize, col: usize, byte: u8, color: Color) {
    let cell = (color as u16) << 8 | byte as u16;
    unsafe { BUFFER.add(row * WIDTH + col).write_volatile(cell) };
}

pub fn clear() {
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            put(row, col, b' ', Color::Black);
        }
    }
}

pub fn write_line(row: usize, text: &str, color: Color) {
    for (col, byte) in text.bytes().take(WIDTH).enumerate() {
        put(row, col, byte, color);
    }
}
