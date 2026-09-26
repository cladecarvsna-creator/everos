//! Drawing into a pixel buffer in memory. Pixels are `0x00RRGGBB`.
//!
//! A `Canvas` is a view into a buffer with its own origin and clip
//! rectangle, so a window can draw in its own coordinates without
//! touching anything outside its area.

use crate::font;

/// A colour as `0x00RRGGBB`.
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Mix two colours; `t` goes from 0 (all `a`) to 255 (all `b`).
pub fn mix(a: Color, b: Color, t: u32) -> Color {
    let t = t.min(255);
    let channel = |shift: u32| {
        let (x, y) = ((a >> shift) & 0xff, (b >> shift) & 0xff);
        ((x * (255 - t) + y * t) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    /// The smallest rectangle covering both. Empty rectangles are ignored.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Shrink by `n` pixels on every side.
    pub fn inset(&self, n: i32) -> Rect {
        Rect::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    stride: usize,
    /// Where (0, 0) of this view is in the buffer.
    ox: i32,
    oy: i32,
    /// Drawable area in buffer coordinates.
    clip: Rect,
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self {
        assert!(pixels.len() >= width * height);
        Self {
            pixels,
            stride: width,
            ox: 0,
            oy: 0,
            clip: Rect::new(0, 0, width as i32, height as i32),
            width: width as i32,
            height: height as i32,
        }
    }

    /// A view of the area `r` (in this view's coordinates) that draws
    /// relative to `r`'s corner and only inside `r` and the current clip.
    pub fn sub(&mut self, r: Rect) -> Canvas<'_> {
        let area = r.offset(self.ox, self.oy);
        Canvas {
            pixels: self.pixels,
            stride: self.stride,
            ox: area.x,
            oy: area.y,
            clip: self.clip.intersect(&area),
            width: r.w,
            height: r.h,
        }
    }

    /// Limit drawing to `r` (in this view's coordinates) as well.
    pub fn clip_to(&mut self, r: Rect) {
        self.clip = self.clip.intersect(&r.offset(self.ox, self.oy));
    }

    /// Whether anything inside `r` could be drawn.
    pub fn visible(&self, r: Rect) -> bool {
        !self.clip.intersect(&r.offset(self.ox, self.oy)).is_empty()
    }

    pub fn pixel(&mut self, x: i32, y: i32, c: Color) {
        let (x, y) = (x + self.ox, y + self.oy);
        if self.clip.contains(x, y) {
            self.pixels[y as usize * self.stride + x as usize] = c;
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let r = Rect::new(x + self.ox, y + self.oy, w, h).intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        for row in r.y..r.bottom() {
            let start = row as usize * self.stride + r.x as usize;
            self.pixels[start..start + r.w as usize].fill(c);
        }
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        self.fill_rect(r.x, r.y, r.w, r.h, c);
    }

    /// Darken what is already drawn in `r`, by `amount` out of 255.
    pub fn darken(&mut self, r: Rect, amount: u32) {
        let r = r.offset(self.ox, self.oy).intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        for row in r.y..r.bottom() {
            let start = row as usize * self.stride + r.x as usize;
            for p in &mut self.pixels[start..start + r.w as usize] {
                *p = mix(*p, 0, amount);
            }
        }
    }

    /// One pixel wide outline.
    pub fn outline(&mut self, r: Rect, c: Color) {
        self.fill_rect(r.x, r.y, r.w, 1, c);
        self.fill_rect(r.x, r.bottom() - 1, r.w, 1, c);
        self.fill_rect(r.x, r.y, 1, r.h, c);
        self.fill_rect(r.right() - 1, r.y, 1, r.h, c);
    }

    /// A raised (or, with `pressed`, sunken) 3D frame.
    pub fn bevel(&mut self, r: Rect, light: Color, dark: Color, pressed: bool) {
        let (top, bottom) = if pressed {
            (dark, light)
        } else {
            (light, dark)
        };
        self.fill_rect(r.x, r.y, r.w, 1, top);
        self.fill_rect(r.x, r.y, 1, r.h, top);
        self.fill_rect(r.x, r.bottom() - 1, r.w, 1, bottom);
        self.fill_rect(r.right() - 1, r.y, 1, r.h, bottom);
    }

    pub fn vertical_gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let first = (self.clip.y - self.oy).max(r.y);
        let last = (self.clip.bottom() - self.oy).min(r.bottom());
        for y in first..last {
            let t = ((y - r.y) * 255 / r.h.max(1)) as u32;
            self.fill_rect(r.x, y, r.w, 1, mix(top, bottom, t));
        }
    }

    pub fn horizontal_gradient(&mut self, r: Rect, left: Color, right: Color) {
        let first = (self.clip.x - self.ox).max(r.x);
        let last = (self.clip.right() - self.ox).min(r.right());
        for x in first..last {
            let t = ((x - r.x) * 255 / r.w.max(1)) as u32;
            self.fill_rect(x, r.y, 1, r.h, mix(left, right, t));
        }
    }

    /// Bresenham line, calling `plot` for every point.
    pub fn trace_line(x0: i32, y0: i32, x1: i32, y1: i32, mut plot: impl FnMut(i32, i32)) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            plot(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Color) {
        Self::trace_line(x0, y0, x1, y1, |x, y| self.pixel(x, y, c));
    }

    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: i32, c: Color) {
        for dy in -radius..=radius {
            // widest dx on this row
            let mut dx = 0;
            while (dx + 1) * (dx + 1) + dy * dy <= radius * radius {
                dx += 1;
            }
            self.fill_rect(cx - dx, cy + dy, 2 * dx + 1, 1, c);
        }
    }

    pub fn draw_char(&mut self, x: i32, y: i32, ch: char, c: Color) {
        if !self.visible(Rect::new(x, y, font::WIDTH as i32, font::HEIGHT as i32)) {
            return;
        }
        for (row, bits) in font::glyph(ch).iter().enumerate() {
            for col in 0..font::WIDTH {
                if bits & (0x80 >> col) != 0 {
                    self.pixel(x + col as i32, y + row as i32, c);
                }
            }
        }
    }

    /// Draw text and return its width in pixels.
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, c: Color) -> i32 {
        let mut n = 0;
        for ch in text.chars() {
            self.draw_char(x + n * font::WIDTH as i32, y, ch, c);
            n += 1;
        }
        n * font::WIDTH as i32
    }

    /// Draw text centred in `r`.
    pub fn text_centered(&mut self, r: Rect, text: &str, c: Color) {
        let w = text_width(text);
        let x = r.x + (r.w - w) / 2;
        let y = r.y + (r.h - font::HEIGHT as i32) / 2;
        self.draw_text(x, y, text, c);
    }

    /// Copy a `w` x `h` block of pixels with row length `src_stride`.
    pub fn blit(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u32], src_stride: usize) {
        let dst = Rect::new(x + self.ox, y + self.oy, w, h);
        let r = dst.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let (sx, sy) = ((r.x - dst.x) as usize, (r.y - dst.y) as usize);
        for row in 0..r.h as usize {
            let from = (sy + row) * src_stride + sx;
            let to = (r.y as usize + row) * self.stride + r.x as usize;
            self.pixels[to..to + r.w as usize].copy_from_slice(&src[from..from + r.w as usize]);
        }
    }

    /// Draw a small picture given as strings: each character is looked up
    /// in `palette` as (char, colour); characters not in it are skipped.
    pub fn sprite(&mut self, x: i32, y: i32, rows: &[&[u8]], palette: &[(u8, Color)]) {
        for (dy, row) in rows.iter().enumerate() {
            for (dx, ch) in row.iter().enumerate() {
                if let Some(&(_, c)) = palette.iter().find(|(p, _)| p == ch) {
                    self.pixel(x + dx as i32, y + dy as i32, c);
                }
            }
        }
    }
}

pub fn text_width(text: &str) -> i32 {
    (text.chars().count() * font::WIDTH) as i32
}
