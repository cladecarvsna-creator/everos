//! App icons: drawn at 48 pixels with smooth shapes, and shrunk with
//! alpha to 24 and 16 pixels for the taskbar, menus and title bars.

use super::canvas::{rgb, Canvas, Rect};
use super::{App, APPS};

/// App icons, shrunk from the 48 pixel drawings, with alpha.
pub struct Icons {
    medium: [[u32; 24 * 24]; APPS.len()],
    small: [[u32; 16 * 16]; APPS.len()],
}

impl Icons {
    pub fn new() -> Self {
        let mut icons = Icons {
            medium: [[0; 24 * 24]; APPS.len()],
            small: [[0; 16 * 16]; APPS.len()],
        };
        for app in APPS {
            // pixels left at the marker value are transparent
            let mut big = [TRANSPARENT; 48 * 48];
            draw_icon(&mut Canvas::new(&mut big, 48, 48), app, 0, 0);
            shrink(&big, 2, &mut icons.medium[app.index()]);
            shrink(&big, 3, &mut icons.small[app.index()]);
        }
        icons
    }

    pub fn draw_medium(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        c.blit_alpha(x, y, 24, 24, &self.medium[app.index()]);
    }

    pub fn draw_small(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        c.blit_alpha(x, y, 16, 16, &self.small[app.index()]);
    }
}

const TRANSPARENT: u32 = 0xffc8_c8c8;

/// Scale a 48x48 picture down by `k`, averaging the opaque pixels of each
/// block and turning how many there were into alpha.
fn shrink(big: &[u32], k: usize, out: &mut [u32]) {
    let n = 48 / k;
    for oy in 0..n {
        for ox in 0..n {
            let (mut r, mut g, mut b, mut count) = (0, 0, 0, 0);
            for y in oy * k..oy * k + k {
                for x in ox * k..ox * k + k {
                    let p = big[y * 48 + x];
                    if p != TRANSPARENT {
                        r += (p >> 16) & 0xff;
                        g += (p >> 8) & 0xff;
                        b += p & 0xff;
                        count += 1;
                    }
                }
            }
            out[oy * n + ox] = if count == 0 {
                0
            } else {
                let alpha = count * 255 / (k * k) as u32;
                alpha << 24 | (r / count) << 16 | (g / count) << 8 | (b / count)
            };
        }
    }
}

/// A 48x48 app icon.
pub fn draw_icon(c: &mut Canvas, app: App, x: i32, y: i32) {
    let tile = Rect::new(x + 2, y + 2, 44, 44);
    match app {
        App::Terminal => {
            c.fill_round(tile, 8, rgb(0x2b, 0x2d, 0x36));
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.fill_rect(tile.x, tile.y, tile.w, 10, rgb(0x4a, 0x4e, 0x5c));
            }
            c.outline_round(tile, 8, rgb(0x16, 0x18, 0x1e));
            c.draw_text(x + 10, y + 20, ">_", rgb(0xe8, 0xe8, 0xf0));
        }
        App::Paint => {
            c.fill_round(tile, 8, rgb(0xfa, 0xfa, 0xfc));
            c.outline_round(tile, 8, rgb(0xb8, 0xbc, 0xc8));
            c.fill_round(Rect::new(x + 9, y + 9, 13, 13), 6, rgb(0xe8, 0x3c, 0x3c));
            c.fill_round(Rect::new(x + 24, y + 10, 13, 13), 6, rgb(0x2c, 0xb8, 0x5c));
            c.fill_round(Rect::new(x + 14, y + 23, 13, 13), 6, rgb(0x1c, 0x8c, 0xf0));
            for i in 0..3 {
                c.line(
                    x + 28 + i,
                    y + 42,
                    x + 41 + i,
                    y + 29,
                    rgb(0xa8, 0x6a, 0x2c),
                );
            }
            c.fill_round(Rect::new(x + 38, y + 25, 6, 6), 2, rgb(0x40, 0x40, 0x48));
        }
        App::Calculator => {
            c.fill_round(tile, 8, rgb(0x3a, 0x3e, 0x4c));
            c.outline_round(tile, 8, rgb(0x20, 0x22, 0x2c));
            c.fill_round(Rect::new(x + 9, y + 8, 30, 9), 2, rgb(0xd8, 0xe4, 0xf4));
            for row in 0..3 {
                for col in 0..3 {
                    let color = if (row, col) == (2, 2) {
                        rgb(0x3a, 0x9c, 0xff)
                    } else {
                        rgb(0xe8, 0xe8, 0xf0)
                    };
                    let r = Rect::new(x + 9 + col * 11, y + 20 + row * 8, 8, 6);
                    c.fill_round(r, 2, color);
                }
            }
        }
        App::Demo => {
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0x16, 0x20, 0x5c), rgb(0x6a, 0x1c, 0x5c));
            }
            c.fill_round(Rect::new(x + 15, y + 15, 18, 18), 9, rgb(0xff, 0xc0, 0x40));
            c.fill_round(Rect::new(x + 9, y + 10, 8, 8), 4, rgb(0x60, 0xc0, 0xff));
            c.fill_round(Rect::new(x + 33, y + 31, 6, 6), 3, rgb(0xff, 0x70, 0x70));
            c.outline_round(tile, 8, rgb(0x10, 0x10, 0x30));
        }
        App::Browser => {
            // a globe on a blue tile
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0x3c, 0x9c, 0xff), rgb(0x10, 0x5c, 0xd8));
            }
            let (cx, cy) = (x + 24, y + 24);
            c.fill_round(
                Rect::new(cx - 14, cy - 14, 28, 28),
                14,
                rgb(0xf4, 0xf8, 0xff),
            );
            let line = rgb(0x1c, 0x6c, 0xe0);
            c.outline_round(Rect::new(cx - 14, cy - 14, 28, 28), 14, line);
            c.outline_round(Rect::new(cx - 6, cy - 14, 12, 28), 6, line);
            c.fill_rect(cx, cy - 13, 1, 26, line);
            c.fill_rect(cx - 13, cy, 26, 1, line);
            c.fill_rect(cx - 11, cy - 7, 22, 1, line);
            c.fill_rect(cx - 11, cy + 7, 22, 1, line);
            c.outline_round(tile, 8, rgb(0x0c, 0x40, 0xa0));
        }
    }
}
