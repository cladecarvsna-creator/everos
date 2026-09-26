//! A graphics demo that shows off the framebuffer drawing functions.

use crate::console::Color;
use crate::framebuffer::{Framebuffer, Rgb};

pub fn demo(fb: &Framebuffer) {
    let (w, h) = (fb.width, fb.height);
    fb.vertical_gradient(
        0,
        0,
        w,
        h,
        Rgb::new(0x10, 0x18, 0x40),
        Rgb::new(0x50, 0x10, 0x40),
    );

    // stars, from a small pseudo-random generator
    let mut seed: u32 = 0x2545_f491;
    for _ in 0..400 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let (x, y) = (seed as usize % w, (seed >> 16) as usize % h);
        fb.put_pixel(x, y, Rgb::new(0xff, 0xff, 0xff));
    }

    // starburst of lines from the centre
    let (cx, cy) = (w as isize / 2, h as isize / 2);
    let reach = (w.min(h) / 2 - 20) as isize;
    for i in 0..64isize {
        // walk the edge of a square so we need no trigonometry
        let t = i * 8 * reach / 64;
        let (ex, ey) = match i / 16 {
            0 => (-reach + 2 * (t % (2 * reach)) / 2, -reach),
            1 => (reach, -reach + 2 * (t % (2 * reach)) / 2),
            2 => (reach - 2 * (t % (2 * reach)) / 2, reach),
            _ => (-reach, reach - 2 * (t % (2 * reach)) / 2),
        };
        let color =
            Rgb::new(0x40, (i * 3 + 60) as u8, 0xff).mix(Rgb::new(0xff, 0x60, 0xc0), (i * 4) as u8);
        fb.line(cx, cy, cx + ex, cy + ey, color);
    }

    // palette row
    for i in 0..16usize {
        let size = w / 20;
        let x = w / 2 - 8 * size + i * size;
        fb.fill_rect(x + 2, h - size - 30, size - 4, size - 4, palette(i).rgb());
    }

    // planets
    fb.fill_circle(cx, cy, reach / 4, Rgb::new(0xff, 0xc0, 0x40));
    fb.fill_circle(cx, cy, reach / 4 - 8, Rgb::new(0xff, 0xe8, 0x80));
    fb.fill_circle(
        cx - reach / 2,
        cy - reach / 3,
        reach / 8,
        Rgb::new(0x60, 0xc0, 0xff),
    );
    fb.fill_circle(
        cx + reach / 2,
        cy + reach / 3,
        reach / 10,
        Rgb::new(0xff, 0x70, 0x70),
    );

    let title = "EverOS graphics";
    fb.draw_text(w / 2 - title.len() * 4, 24, title, Color::White.rgb(), None);
    let hint = "press any key to return";
    fb.draw_text(
        w / 2 - hint.len() * 4,
        h - 20,
        hint,
        Color::LightGray.rgb(),
        None,
    );
}

fn palette(i: usize) -> Color {
    const ALL: [Color; 16] = [
        Color::Black,
        Color::Blue,
        Color::Green,
        Color::Cyan,
        Color::Red,
        Color::Magenta,
        Color::Brown,
        Color::LightGray,
        Color::DarkGray,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightCyan,
        Color::LightRed,
        Color::Pink,
        Color::Yellow,
        Color::White,
    ];
    ALL[i]
}
