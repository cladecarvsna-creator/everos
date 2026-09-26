//! Wrapping a document's items into lines of a given width.

use alloc::string::String;
use alloc::vec::Vec;

use super::html::{Document, Item, Style};

/// Text measurements, provided by whoever draws the page.
pub trait Metrics {
    fn text_width(&self, text: &str, style: Style) -> i32;
    /// Height of a line of text in this style, spacing included.
    fn line_height(&self, style: Style) -> i32;
}

pub const INDENT: i32 = 28;
pub const INPUT_W: i32 = 240;
pub const FIELD_H: i32 = 28;

#[derive(Clone, Debug)]
pub enum RunKind {
    Text(String),
    /// Form, field.
    Input(u32, u32),
    /// Form, label.
    Button(u32, String),
    Rule,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub x: i32,
    pub w: i32,
    pub h: i32,
    pub style: Style,
    pub kind: RunKind,
}

#[derive(Debug)]
pub struct Line {
    pub y: i32,
    pub h: i32,
    pub runs: Vec<Run>,
}

#[derive(Default)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub height: i32,
}

impl Layout {
    /// Index of the first line that reaches below `y`.
    pub fn first_line_below(&self, y: i32) -> usize {
        self.lines.partition_point(|l| l.y + l.h <= y)
    }

    /// The run under a point in page coordinates.
    pub fn run_at(&self, x: i32, y: i32) -> Option<&Run> {
        let i = self.first_line_below(y);
        let line = self.lines.get(i)?;
        if y < line.y {
            return None;
        }
        line.runs
            .iter()
            .find(|r| x >= r.x && x < r.x + r.w && y >= line.y + line.h - r.h)
    }
}

struct Builder<'a> {
    m: &'a dyn Metrics,
    width: i32,
    y: i32,
    line: Vec<Run>,
    x: i32,
    indent: i32,
    center: bool,
    out: Vec<Line>,
}

impl Builder<'_> {
    fn left(&self) -> i32 {
        self.indent.min(self.width / 2)
    }

    fn finish_line(&mut self, empty_height: i32) {
        if self.line.is_empty() {
            self.y += empty_height;
            self.x = self.left();
            return;
        }
        // drop trailing spaces
        if let Some(Run {
            kind: RunKind::Text(t),
            w,
            style,
            ..
        }) = self.line.last_mut()
        {
            let trimmed = t.trim_end_matches(' ');
            if trimmed.len() != t.len() {
                let new_w = self.m.text_width(trimmed, *style);
                *w = new_w;
                let len = trimmed.len();
                t.truncate(len);
            }
        }
        let h = self.line.iter().map(|r| r.h).max().unwrap_or(0);
        if self.center {
            let right = self.line.last().map_or(0, |r| r.x + r.w);
            let shift = ((self.width - right) / 2).max(0);
            for r in &mut self.line {
                r.x += shift;
            }
        }
        self.out.push(Line {
            y: self.y,
            h,
            runs: core::mem::take(&mut self.line),
        });
        self.y += h;
        self.x = self.left();
    }

    fn place(&mut self, w: i32, h: i32, style: Style, kind: RunKind) {
        if self.x + w > self.width && !self.line.is_empty() {
            self.finish_line(0);
        }
        self.line.push(Run {
            x: self.x,
            w,
            h,
            style,
            kind,
        });
        self.x += w;
    }

    fn text(&mut self, text: &str, style: Style) {
        let h = self.m.line_height(style);
        // words with the spaces after them
        let mut rest = text;
        while !rest.is_empty() {
            let end = match rest.find(' ') {
                Some(i) => {
                    let mut e = i;
                    while rest.as_bytes().get(e) == Some(&b' ') {
                        e += 1;
                    }
                    e
                }
                None => rest.len(),
            };
            let word = &rest[..end];
            rest = &rest[end..];
            if self.line.is_empty() && word.trim().is_empty() && !style.mono {
                continue;
            }
            let w = self.m.text_width(word, style);
            let word_w = self.m.text_width(word.trim_end(), style);
            if self.x + word_w > self.width && !self.line.is_empty() {
                self.finish_line(0);
                if word.trim().is_empty() {
                    continue;
                }
            }
            if self.x + word_w > self.width {
                // a word wider than the page: break it anywhere
                self.long_word(word, style, h);
                continue;
            }
            self.append(word, w, h, style);
        }
    }

    /// Add text to the line, merging with the previous run of the same style.
    fn append(&mut self, word: &str, w: i32, h: i32, style: Style) {
        if let Some(Run {
            kind: RunKind::Text(t),
            style: s,
            w: rw,
            x,
            ..
        }) = self.line.last_mut()
        {
            if *s == style && *x + *rw == self.x {
                t.push_str(word);
                *rw += w;
                self.x += w;
                return;
            }
        }
        self.line.push(Run {
            x: self.x,
            w,
            h,
            style,
            kind: RunKind::Text(String::from(word)),
        });
        self.x += w;
    }

    fn long_word(&mut self, word: &str, style: Style, h: i32) {
        let mut piece = String::new();
        let mut piece_w = 0;
        for c in word.chars() {
            let mut buf = [0; 4];
            let cw = self.m.text_width(c.encode_utf8(&mut buf), style);
            if self.x + piece_w + cw > self.width && !piece.is_empty() {
                self.append(&piece, piece_w, h, style);
                self.finish_line(0);
                piece.clear();
                piece_w = 0;
            }
            piece.push(c);
            piece_w += cw;
        }
        if !piece.is_empty() {
            self.append(&piece, piece_w, h, style);
        }
    }
}

pub fn layout(doc: &Document, width: i32, m: &dyn Metrics) -> Layout {
    let normal = m.line_height(Style::default());
    let mut b = Builder {
        m,
        width: width.max(100),
        y: 0,
        line: Vec::new(),
        x: 0,
        indent: 0,
        center: false,
        out: Vec::new(),
    };
    for item in &doc.items {
        match item {
            Item::Text(text, style) => b.text(text, *style),
            Item::Newline => b.finish_line(normal),
            Item::Block(gap) => {
                let had_line = !b.line.is_empty();
                b.finish_line(0);
                if had_line || b.y > 0 {
                    b.y += normal * *gap as i32 / 16;
                }
            }
            Item::Indent(level) => {
                b.finish_line(0);
                b.indent = *level as i32 * INDENT;
                b.x = b.left();
            }
            Item::Center(on) => {
                b.finish_line(0);
                b.center = *on;
            }
            Item::Rule => {
                b.finish_line(0);
                let w = b.width - b.left();
                b.place(w, 16, Style::default(), RunKind::Rule);
                b.finish_line(0);
            }
            Item::Input(form, field) => {
                let w = INPUT_W.min(b.width - b.left());
                b.place(
                    w + 6,
                    FIELD_H + 4,
                    Style::default(),
                    RunKind::Input(*form, *field),
                );
            }
            Item::Button(form, label) => {
                let w = m.text_width(label, Style::default()) + 28;
                b.place(
                    w + 6,
                    FIELD_H + 4,
                    Style::default(),
                    RunKind::Button(*form, label.clone()),
                );
            }
        }
    }
    b.finish_line(0);
    Layout {
        height: b.y,
        lines: b.out,
    }
}
