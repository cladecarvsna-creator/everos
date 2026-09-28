//! Colour emoji for Telegram: 18x18 pictures of Noto Color Emoji made by
//! scripts/gen-emoji.py (kernel/assets/emoji.png, one atlas, and
//! emoji.bin, what each picture is).
//!
//! Text that shows emoji keeps each one as a single character of the
//! private use area (U+F0000 + its number), so wrapping and cutting text
//! works on characters as before; `width` and `draw` know to put the
//! picture there.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::canvas::{Canvas, Color};
use super::text::Font;

pub const SIZE: i32 = 18;
/// The room an emoji takes in a line.
pub const ADVANCE: i32 = 20;
const COLS: usize = 64;
const BASE: u32 = 0xF_0000;

static KEYS: &[u8] = include_bytes!("../../assets/emoji.bin");
static ATLAS_PNG: &[u8] = include_bytes!("../../assets/emoji.png");

struct Atlas {
    width: usize,
    pixels: Vec<u32>,
}

static ATLAS: AtomicPtr<Atlas> = AtomicPtr::new(core::ptr::null_mut());

fn atlas() -> Option<&'static Atlas> {
    let p = ATLAS.load(Ordering::Acquire);
    if !p.is_null() {
        return Some(unsafe { &*p });
    }
    let img = crate::web::image::decode(ATLAS_PNG)?;
    let a = Box::leak(Box::new(Atlas {
        width: img.width,
        pixels: img.pixels,
    }));
    ATLAS.store(a, Ordering::Release);
    Some(a)
}

fn count() -> usize {
    KEYS.len() / 4
}

fn key(i: usize) -> u32 {
    u32::from_le_bytes([
        KEYS[i * 4],
        KEYS[i * 4 + 1],
        KEYS[i * 4 + 2],
        KEYS[i * 4 + 3],
    ])
}

/// The number of the picture for a key (see gen-emoji.py).
pub fn find(k: u32) -> Option<usize> {
    let (mut lo, mut hi) = (0, count());
    while lo < hi {
        let mid = (lo + hi) / 2;
        match key(mid).cmp(&k) {
            core::cmp::Ordering::Less => lo = mid + 1,
            core::cmp::Ordering::Greater => hi = mid,
            core::cmp::Ordering::Equal => return Some(mid),
        }
    }
    None
}

/// The character that stands for picture `i` in shown text.
pub fn to_char(i: usize) -> char {
    char::from_u32(BASE + i as u32).unwrap_or('?')
}

/// Which picture a shown character stands for.
pub fn index(c: char) -> Option<usize> {
    let n = (c as u32).checked_sub(BASE)? as usize;
    (n < count()).then_some(n)
}

/// Turn the emoji in `s` into picture characters. A character the font
/// has stays text unless an emoji variation selector asks otherwise;
/// skin tones are dropped and a joined sequence shows its first part.
/// `has` says whether the font can draw a character.
///
/// Also returns, for each character of `s` (and its end), where it went
/// in the result, counted in characters.
pub fn convert(s: &str, has: impl Fn(char) -> bool) -> (String, Vec<usize>) {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut n = 0;
    let mut map = Vec::with_capacity(chars.len() + 1);
    let mut i = 0;
    while i < chars.len() {
        let from = i;
        let at = n;
        let (used, shown) = one(&chars, i, &has, out.ends_with('\u{2022}'));
        i += used;
        if let Some(c) = shown {
            out.push(c);
            n += 1;
        }
        for _ in from..i {
            map.push(at);
        }
    }
    map.push(n);
    (out, map)
}

/// What the characters at `i` show: how many are used, and the one
/// shown character (none for joiners and the like).
fn one(
    chars: &[char],
    i: usize,
    has: &impl Fn(char) -> bool,
    after_dot: bool,
) -> (usize, Option<char>) {
    let c = chars[i];
    let next = chars.get(i + 1).copied();
    // a flag: two regional letters
    if ('\u{1f1e6}'..='\u{1f1ff}').contains(&c) {
        if let Some(n) = next.filter(|n| ('\u{1f1e6}'..='\u{1f1ff}').contains(n)) {
            let k = 0x1000_0000 | (c as u32 - 0x1f1e6) << 8 | (n as u32 - 0x1f1e6);
            if let Some(e) = find(k) {
                return (2, Some(to_char(e)));
            }
        }
        return (1, None);
    }
    // a keycap: 1, U+FE0F, U+20E3
    if c.is_ascii_digit() || c == '#' || c == '*' {
        let len = match next {
            Some('\u{20e3}') => 2,
            Some('\u{fe0f}') if chars.get(i + 2) == Some(&'\u{20e3}') => 3,
            _ => 0,
        };
        if len > 0 {
            if let Some(e) = find(0x2000_0000 | c as u32) {
                return (len, Some(to_char(e)));
            }
        }
    }
    if c == '\n' {
        return (1, Some(c));
    }
    if matches!(c, '\r' | '\u{200b}' | '\u{200c}' | '\u{200e}' | '\u{200f}') {
        return (1, None);
    }
    if c == '\u{200d}' {
        // the rest of a joined emoji (a family, a profession)
        return (if next.is_some() { 2 } else { 1 }, None);
    }
    if matches!(c, '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{20e3}')
        || ('\u{e0020}'..='\u{e007f}').contains(&c)
    {
        return (1, None);
    }
    let wants_picture = next == Some('\u{fe0f}');
    if has(c) && !wants_picture {
        return (1, Some(c));
    }
    match find(c as u32) {
        Some(e) => (1, Some(to_char(e))),
        None if has(c) => (1, Some(c)),
        None if after_dot => (1, None),
        None => (1, Some('\u{2022}')),
    }
}

/// Width of shown text in pixels.
pub fn width(f: &Font, s: &str) -> i32 {
    let mut sixteenths = 0i32;
    for c in s.chars() {
        sixteenths += if index(c).is_some() {
            ADVANCE * 16
        } else {
            f.advance16(c) as i32
        };
    }
    (sixteenths + 8) / 16
}

/// Width of one shown character.
pub fn char_width(f: &Font, c: char) -> i32 {
    width(f, c.encode_utf8(&mut [0; 4]))
}

/// Draw one picture with its top left at (x, y).
pub fn draw_picture(c: &mut Canvas, x: i32, y: i32, i: usize) {
    let Some(a) = atlas() else {
        return;
    };
    let (col, row) = (i % COLS, i / COLS);
    let size = SIZE as usize;
    let mut px = [0u32; 18 * 18];
    for dy in 0..size {
        let from = (row * size + dy) * a.width + col * size;
        px[dy * size..(dy + 1) * size].copy_from_slice(&a.pixels[from..from + size]);
    }
    c.blit_alpha(x, y, SIZE, SIZE, &px);
}

/// Draw shown text and return its width.
pub fn draw(c: &mut Canvas, f: &Font, x: i32, y: i32, s: &str, color: Color) -> i32 {
    let mut pen = x;
    let mut run = String::new();
    let top = y + (f.line_height - SIZE) / 2 - 1;
    for ch in s.chars() {
        match index(ch) {
            Some(i) => {
                if !run.is_empty() {
                    pen += c.draw_text_in(f, pen, y, &run, color);
                    run.clear();
                }
                draw_picture(c, pen + (ADVANCE - SIZE) / 2, top, i);
                pen += ADVANCE;
            }
            None => run.push(ch),
        }
    }
    if !run.is_empty() {
        pen += c.draw_text_in(f, pen, y, &run, color);
    }
    pen - x
}

/// The emoji in the picker, most used first.
pub const PICKER: &[&str] = &[
    "😀", "😂", "🤣", "😊", "😍", "🥰", "😘", "😎", "🤔", "😏", "😅", "😉", "🙂", "🙃", "😇", "🤗",
    "😢", "😭", "😡", "😱", "🥺", "😴", "🤯", "🥳", "🤩", "😬", "🙄", "😐", "🤝", "👍", "👎", "👌",
    "✌️", "🤞", "👏", "🙏", "💪", "👋", "🤙", "👀", "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "💔",
    "🔥", "✨", "⭐", "🎉", "🎁", "💯", "✅", "❌", "⚡", "💡", "🚀", "🎮", "💻", "📱", "☕", "🍕",
    "🍺", "🎂", "🌹", "🌞", "🌙", "⛄", "🐱", "🐶", "🦊", "🐸", "🇷🇺",
];
