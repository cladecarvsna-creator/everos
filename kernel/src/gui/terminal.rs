//! Terminal: the text shell in a window. The shell prints to the console
//! as before; the console keeps the text off screen and this window
//! draws it.

use super::canvas::{Canvas, Rect};
use crate::console::{Color, CONSOLE};
use crate::font;
use crate::keyboard::Key;
use crate::multiboot::BootInfo;
use crate::shell::Shell;

pub const COLS: usize = 80;
pub const ROWS: usize = 25;
const PAD: i32 = 4;
pub const CLIENT_W: i32 = COLS as i32 * font::WIDTH as i32 + 2 * PAD;
pub const CLIENT_H: i32 = ROWS as i32 * font::HEIGHT as i32 + 2 * PAD;

pub struct Terminal {
    shell: Shell,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            shell: Shell::new(),
        }
    }

    pub fn start(&mut self) {
        self.shell.prompt();
    }

    pub fn on_key(&mut self, key: Key, boot: &BootInfo) {
        self.shell.on_key(key, boot);
    }

    /// Where the text cursor is, in client coordinates.
    pub fn cursor_rect(&self) -> Rect {
        let (row, col) = CONSOLE.lock().cursor();
        Rect::new(
            PAD + col as i32 * font::WIDTH as i32,
            PAD + row as i32 * font::HEIGHT as i32,
            font::WIDTH as i32,
            font::HEIGHT as i32,
        )
    }

    pub fn draw(&self, c: &mut Canvas, show_cursor: bool) {
        let background = Color::Black.rgb().raw();
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, background);
        let con = CONSOLE.lock();
        for row in 0..ROWS {
            let y = PAD + (row * font::HEIGHT) as i32;
            if !c.visible(Rect::new(0, y, CLIENT_W, font::HEIGHT as i32)) {
                continue;
            }
            for col in 0..COLS {
                let (ch, fg, bg) = con.cell(row, col);
                let x = PAD + (col * font::WIDTH) as i32;
                if bg != Color::Black {
                    c.fill_rect(
                        x,
                        y,
                        font::WIDTH as i32,
                        font::HEIGHT as i32,
                        bg.rgb().raw(),
                    );
                }
                if ch != ' ' {
                    c.draw_char(x, y, ch, fg.rgb().raw());
                }
            }
        }
        if show_cursor {
            let (row, col) = con.cursor();
            if col < COLS {
                let (x, y) = (
                    PAD + (col * font::WIDTH) as i32,
                    PAD + (row * font::HEIGHT) as i32,
                );
                let color = con.color().rgb().raw();
                c.fill_rect(x, y + font::HEIGHT as i32 - 3, font::WIDTH as i32, 2, color);
            }
        }
    }
}
