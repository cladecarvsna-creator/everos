//! Paint: draw on a picture with the mouse.
//!
//! Left button paints with the chosen colour, right button with white.
//! Tools: brush, eraser and flood fill, four brush sizes, and Clear.

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::theme;
use super::{MouseEvent, MouseKind};
use crate::sync::StaticBuffer;

pub const PICTURE_W: usize = 960;
pub const PICTURE_H: usize = 600;
const TOOLBAR_H: i32 = 54;
pub const CLIENT_W: i32 = PICTURE_W as i32;
pub const CLIENT_H: i32 = TOOLBAR_H + PICTURE_H as i32;

const WHITE: Color = rgb(0xff, 0xff, 0xff);

const PALETTE: [Color; 24] = [
    rgb(0x00, 0x00, 0x00),
    rgb(0x7f, 0x7f, 0x7f),
    rgb(0x88, 0x00, 0x15),
    rgb(0xed, 0x1c, 0x24),
    rgb(0xff, 0x7f, 0x27),
    rgb(0xff, 0xf2, 0x00),
    rgb(0x22, 0xb1, 0x4c),
    rgb(0x00, 0xa2, 0xe8),
    rgb(0x3f, 0x48, 0xcc),
    rgb(0xa3, 0x49, 0xa4),
    rgb(0x6c, 0x3a, 0x1e),
    rgb(0x00, 0x60, 0x40),
    rgb(0xff, 0xff, 0xff),
    rgb(0xc3, 0xc3, 0xc3),
    rgb(0xb9, 0x7a, 0x57),
    rgb(0xff, 0xae, 0xc9),
    rgb(0xff, 0xc9, 0x0e),
    rgb(0xef, 0xe4, 0xb0),
    rgb(0xb5, 0xe6, 0x1d),
    rgb(0x99, 0xd9, 0xea),
    rgb(0x70, 0x92, 0xbe),
    rgb(0xc8, 0xbf, 0xe7),
    rgb(0x40, 0x40, 0x40),
    rgb(0x80, 0xff, 0xc0),
];
const SWATCH: i32 = 22;
const PALETTE_X: i32 = 52;

const SIZES: [i32; 4] = [1, 3, 6, 10];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Brush,
    Eraser,
    Fill,
}

const TOOLS: [(Tool, &str); 3] = [
    (Tool::Brush, "Brush"),
    (Tool::Eraser, "Eraser"),
    (Tool::Fill, "Fill"),
];

static PICTURE: StaticBuffer<{ PICTURE_W * PICTURE_H }> = StaticBuffer::new();
/// Work stack for flood fill, packed as `y << 16 | x`.
static FILL_STACK: StaticBuffer<{ 64 * 1024 }> = StaticBuffer::new();

pub struct Paint {
    picture: &'static mut [u32],
    stack: &'static mut [u32],
    color: Color,
    size: usize,
    tool: Tool,
    /// Last painted point while a button is held, and its colour.
    stroke: Option<(i32, i32, Color)>,
}

fn tool_button(i: usize) -> Rect {
    Rect::new(340 + i as i32 * 64, 5, 60, 20)
}

fn size_button(i: usize) -> Rect {
    Rect::new(340 + i as i32 * 36, 29, 32, 20)
}

fn clear_button() -> Rect {
    Rect::new(CLIENT_W - 76, 5, 70, 20)
}

fn swatch(i: usize) -> Rect {
    let (col, row) = ((i % 12) as i32, (i / 12) as i32);
    Rect::new(
        PALETTE_X + col * SWATCH,
        5 + row * SWATCH,
        SWATCH - 2,
        SWATCH - 2,
    )
}

impl Paint {
    pub fn new() -> Self {
        let picture = PICTURE.take();
        picture.fill(WHITE);
        Self {
            picture,
            stack: FILL_STACK.take(),
            color: PALETTE[0],
            size: 1,
            tool: Tool::Brush,
            stroke: None,
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let (x, y) = (ev.x, ev.y - TOOLBAR_H);
        match ev.kind {
            MouseKind::Down { right } => {
                if ev.y < TOOLBAR_H {
                    return self.toolbar_click(ev.x, ev.y, right);
                }
                if !(0..PICTURE_W as i32).contains(&x) || !(0..PICTURE_H as i32).contains(&y) {
                    return false;
                }
                let color = match (self.tool, right) {
                    (Tool::Eraser, _) | (_, true) => WHITE,
                    _ => self.color,
                };
                if self.tool == Tool::Fill {
                    self.flood_fill(x, y, color);
                } else {
                    self.stamp(x, y, color);
                    self.stroke = Some((x, y, color));
                }
                true
            }
            MouseKind::Move => match self.stroke {
                Some((lx, ly, color)) => {
                    Canvas::trace_line(lx, ly, x, y, |px, py| self.stamp(px, py, color));
                    self.stroke = Some((x, y, color));
                    true
                }
                None => false,
            },
            MouseKind::Up => {
                self.stroke = None;
                false
            }
        }
    }

    fn toolbar_click(&mut self, x: i32, y: i32, right: bool) -> bool {
        if let Some(i) = (0..PALETTE.len()).find(|&i| swatch(i).contains(x, y)) {
            if !right {
                self.color = PALETTE[i];
                if self.tool == Tool::Eraser {
                    self.tool = Tool::Brush;
                }
            }
            return true;
        }
        if let Some(i) = (0..TOOLS.len()).find(|&i| tool_button(i).contains(x, y)) {
            self.tool = TOOLS[i].0;
            return true;
        }
        if let Some(i) = (0..SIZES.len()).find(|&i| size_button(i).contains(x, y)) {
            self.size = i;
            return true;
        }
        if clear_button().contains(x, y) {
            self.picture.fill(WHITE);
            return true;
        }
        false
    }

    /// Paint a round dot of the brush size at a picture point.
    fn stamp(&mut self, cx: i32, cy: i32, color: Color) {
        let mut r = SIZES[self.size];
        if self.tool == Tool::Eraser {
            r = r * 2 + 2;
        }
        let r = r - 1;
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx + dx, cy + dy);
                if dx * dx + dy * dy <= r * r + r
                    && (0..PICTURE_W as i32).contains(&x)
                    && (0..PICTURE_H as i32).contains(&y)
                {
                    self.picture[y as usize * PICTURE_W + x as usize] = color;
                }
            }
        }
    }

    /// Scanline flood fill from a point, replacing its colour.
    fn flood_fill(&mut self, x: i32, y: i32, color: Color) {
        let (w, h) = (PICTURE_W, PICTURE_H);
        let target = self.picture[y as usize * w + x as usize];
        if target == color {
            return;
        }
        let mut top = 0;
        self.stack[top] = (y as u32) << 16 | x as u32;
        top += 1;
        while top > 0 {
            top -= 1;
            let (sx, sy) = (
                (self.stack[top] & 0xffff) as usize,
                (self.stack[top] >> 16) as usize,
            );
            let row = sy * w;
            if self.picture[row + sx] != target {
                continue;
            }
            let mut left = sx;
            while left > 0 && self.picture[row + left - 1] == target {
                left -= 1;
            }
            let mut right = sx;
            while right + 1 < w && self.picture[row + right + 1] == target {
                right += 1;
            }
            self.picture[row + left..=row + right].fill(color);
            // queue one seed per run of target pixels above and below
            for ny in [sy.wrapping_sub(1), sy + 1] {
                if ny >= h {
                    continue;
                }
                let mut in_run = false;
                for nx in left..=right {
                    let matches = self.picture[ny * w + nx] == target;
                    if matches && !in_run && top < self.stack.len() {
                        self.stack[top] = (ny as u32) << 16 | nx as u32;
                        top += 1;
                    }
                    in_run = matches;
                }
            }
        }
    }

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, TOOLBAR_H, theme::FACE);
        c.fill_rect(0, TOOLBAR_H - 1, CLIENT_W, 1, theme::STROKE);

        // current colour
        let current = Rect::new(8, 7, 36, 36);
        c.fill_round(current, 8, self.color);
        c.outline_round(current, 8, theme::SHADOW);
        for (i, &color) in PALETTE.iter().enumerate() {
            let r = swatch(i);
            if color == self.color {
                c.fill_round(r.inset(-2), 12, theme::ACCENT);
                c.fill_round(r, 10, theme::FACE);
            }
            let dot = r.inset(1);
            c.fill_round(dot, 9, color);
            c.outline_round(dot, 9, mix(color, theme::TEXT, 60));
        }

        for (i, &(tool, name)) in TOOLS.iter().enumerate() {
            theme::toggle_button(c, tool_button(i), name, self.tool == tool);
        }
        for (i, &size) in SIZES.iter().enumerate() {
            let r = size_button(i);
            theme::toggle_button(c, r, "", self.size == i);
            let d = (2 * size - 1).clamp(1, 14);
            let dot = Rect::new(r.x + (r.w - d) / 2, r.y + (r.h - d) / 2, d, d);
            c.fill_round(dot, d / 2, theme::TEXT);
        }
        theme::button(c, clear_button(), "Clear", false);

        c.blit(
            0,
            TOOLBAR_H,
            PICTURE_W as i32,
            PICTURE_H as i32,
            self.picture,
            PICTURE_W,
        );
    }
}
