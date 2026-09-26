//! EverBrowser: a toolbar with Back, Reload, Home and the address bar,
//! the page below it and a status bar. Pages come from `crate::web`.
//!
//! Loading blocks the desktop for a moment: a click only records where
//! to go, the desktop draws "Loading...", and the next `tick` fetches it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::text::{Font, MONO, TITLE, UI};
use super::theme;
use super::web_font_data::{H1, H2, PAGE, PAGE_BOLD};
use super::{MouseEvent, MouseKind};
use crate::keyboard::Key;
use crate::sync::IrqMutex;
use crate::web::html::Style;
use crate::web::layout::{self, Layout, Metrics, RunKind};
use crate::web::url::{self, Url};
use crate::web::{self, Page};
use crate::{interrupts, net};

pub const CLIENT_W: i32 = 1280;
pub const CLIENT_H: i32 = 840;

const TOOLBAR_H: i32 = 48;
const STATUS_H: i32 = 26;
const SCROLLBAR_W: i32 = 12;
const PAD_X: i32 = 24;
const PAD_Y: i32 = 14;
const BUTTON: i32 = 34;

const PAGE_BG: Color = rgb(0xff, 0xff, 0xff);
const LINK: Color = rgb(0x1a, 0x0d, 0xab);
const TEXT: Color = rgb(0x20, 0x21, 0x24);
const FAINT: Color = rgb(0x5f, 0x63, 0x68);
const HEADING: Color = rgb(0x10, 0x10, 0x14);

#[derive(Clone)]
enum Nav {
    Home,
    Get(Url),
    Post(Url, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Page,
    Address,
    Field(u32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pressed {
    None,
    Back,
    Reload,
    Home,
    Go,
    /// Dragging the scroll bar thumb, grabbed this far from its top.
    Thumb(i32),
}

pub struct Browser {
    page: Page,
    layout: Layout,
    scroll: i32,
    history: Vec<Nav>,
    current: Option<Nav>,
    pending: Option<Nav>,
    address: String,
    /// Cursor in the address bar or a field, in characters.
    cursor: usize,
    /// The whole text is selected; typing replaces it.
    select_all: bool,
    focus: Focus,
    pressed: Pressed,
    status: String,
    /// Where the link under the mouse goes.
    hover: Option<String>,
    net_started: bool,
}

/// An address typed in the shell, for the browser to open.
static REQUESTED: IrqMutex<Option<String>> = IrqMutex::new(None);

pub fn request_address(address: &str) {
    *REQUESTED.lock() = Some(address.to_string());
}

struct TextMetrics;

/// The font for a page style.
fn font_for(style: Style) -> &'static Font {
    match style.heading {
        1 => &H1,
        2 => &H2,
        3 => &TITLE,
        4..=6 => &PAGE_BOLD,
        _ if style.mono => &MONO,
        _ if style.bold => &PAGE_BOLD,
        _ => &PAGE,
    }
}

impl Metrics for TextMetrics {
    fn text_width(&self, text: &str, style: Style) -> i32 {
        let f = font_for(style);
        let sixteenths: i32 = text
            .chars()
            .map(|c| f.advance16(substitute(c)) as i32)
            .sum();
        (sixteenths + 8) / 16
    }

    fn line_height(&self, style: Style) -> i32 {
        font_for(style).line_height + if style.heading > 0 { 8 } else { 6 }
    }
}

/// Characters the fonts lack, replaced by ones that look alike.
fn substitute(c: char) -> char {
    match c {
        '\u{a0}' | '\u{2009}' | '\u{202f}' | '\u{2002}' | '\u{2003}' | '\t' => ' ',
        '\u{ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{200e}' | '\u{200f}' | '\u{feff}' => {
            '\u{200b}'
        }
        '′' => '\'',
        '″' => '"',
        '‒' | '―' | '‐' => '-',
        '∙' | '●' => '•',
        '✓' | '✔' => 'v',
        c => c,
    }
}

/// Draw text in a page style. Returns the width.
fn draw_styled(c: &mut Canvas, x: i32, y: i32, text: &str, style: Style, color: Color) -> i32 {
    let f = font_for(style);
    let mut pen = x * 16;
    for ch in text.chars() {
        let ch = substitute(ch);
        if ch == '\u{200b}' {
            continue;
        }
        let shown = if f.glyph(ch).is_some() { ch } else { '?' };
        if ch != ' ' {
            c.draw_glyph(f, (pen + 8) / 16, y, shown, color);
            if style.bold && core::ptr::eq(f, &MONO) {
                c.draw_glyph(f, (pen + 8) / 16 + 1, y, shown, color);
            }
        }
        pen += f.advance16(ch) as i32;
    }
    let w = (pen + 8) / 16 - x;
    if style.underline || style.link.is_some() {
        c.fill_rect(x, y + f.line_height * 13 / 16, w, 1, color);
    }
    w
}

/// An anti-aliased line `width` pixels thick, for toolbar icons.
fn stroke(c: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
    let (minx, maxx) = (x0.min(x1) - width, x0.max(x1) + width);
    let (miny, maxy) = (y0.min(y1) - width, y0.max(y1) + width);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = (dx * dx + dy * dy).max(0.0001);
    for py in miny as i32..=maxy as i32 + 1 {
        for px in minx as i32..=maxx as i32 + 1 {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let t = (((fx - x0) * dx + (fy - y0) * dy) / len2).clamp(0.0, 1.0);
            let (ex, ey) = (x0 + t * dx - fx, y0 + t * dy - fy);
            let d = sqrt(ex * ex + ey * ey);
            let cover = (width / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                c.blend_at(px, py, color, (cover * 256.0) as i32);
            }
        }
    }
}

fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..8 {
        x = 0.5 * (x + v / x);
    }
    x
}

fn back_rect() -> Rect {
    Rect::new(8, 7, BUTTON, BUTTON)
}
fn reload_rect() -> Rect {
    Rect::new(8 + BUTTON + 4, 7, BUTTON, BUTTON)
}
fn home_rect() -> Rect {
    Rect::new(8 + 2 * (BUTTON + 4), 7, BUTTON, BUTTON)
}
fn address_rect() -> Rect {
    let x = 8 + 3 * (BUTTON + 4) + 6;
    Rect::new(x, 7, CLIENT_W - x - 8 - 64 - 8, BUTTON)
}
fn go_rect() -> Rect {
    Rect::new(CLIENT_W - 8 - 64, 7, 64, BUTTON)
}
fn content_rect() -> Rect {
    Rect::new(
        0,
        TOOLBAR_H,
        CLIENT_W - SCROLLBAR_W,
        CLIENT_H - TOOLBAR_H - STATUS_H,
    )
}
fn scrollbar_rect() -> Rect {
    Rect::new(
        CLIENT_W - SCROLLBAR_W,
        TOOLBAR_H,
        SCROLLBAR_W,
        CLIENT_H - TOOLBAR_H - STATUS_H,
    )
}

fn page_width() -> i32 {
    content_rect().w - 2 * PAD_X
}

impl Browser {
    pub fn new() -> Self {
        let page = web::home();
        let layout = layout::layout(&page.doc, page_width(), &TextMetrics);
        Browser {
            page,
            layout,
            scroll: 0,
            history: Vec::new(),
            current: Some(Nav::Home),
            pending: None,
            address: String::new(),
            cursor: 0,
            select_all: false,
            focus: Focus::Address,
            pressed: Pressed::None,
            status: String::new(),
            hover: None,
            net_started: false,
        }
    }

    /// Start the network card when the browser first opens.
    pub fn start(&mut self) {
        if !self.net_started {
            self.net_started = true;
            match net::init() {
                Some(mac) => {
                    self.status = alloc::format!(
                        "Network card {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, getting an address...",
                        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                    );
                }
                None => {
                    self.status = String::from(
                        "No network card found. Start QEMU with -nic user,model=e1000",
                    );
                }
            }
        }
    }

    /// Called every pass of the desktop loop. Returns true to redraw.
    pub fn tick(&mut self) -> bool {
        net::poll();
        if let Some(address) = REQUESTED.lock().take() {
            if let Some(u) = web::address_to_url(&address) {
                self.navigate(Nav::Get(u));
                return true; // draw "Loading" first
            }
        }
        if let Some(nav) = self.pending.take() {
            self.load(nav);
            return true;
        }
        if self.status.starts_with("Network card") && net::configured() {
            self.status = alloc::format!("Online, address {}", net::address().unwrap_or_default());
            return true;
        }
        false
    }

    fn navigate(&mut self, nav: Nav) {
        self.status = match &nav {
            Nav::Home => String::from("Opening the home page..."),
            Nav::Get(u) | Nav::Post(u, _) => alloc::format!("Loading {} ...", u),
        };
        if let Nav::Get(u) | Nav::Post(u, _) = &nav {
            self.address = u.to_string();
        }
        self.pending = Some(nav);
        self.focus = Focus::Page;
    }

    fn load(&mut self, nav: Nav) {
        let started = interrupts::ticks();
        let page = match &nav {
            Nav::Home => web::home(),
            Nav::Get(u) => web::load(u, None),
            Nav::Post(u, body) => web::load(u, Some(body)),
        };
        if let Some(prev) = self.current.take() {
            if self.history.len() >= 64 {
                self.history.remove(0);
            }
            self.history.push(prev);
        }
        // a redirect or a POST leaves us at a plain address
        self.current = Some(match (&nav, &page.url) {
            (Nav::Home, _) => Nav::Home,
            (_, Some(u)) => Nav::Get(u.clone()),
            _ => nav.clone(),
        });
        self.set_page(page);
        let secs = (interrupts::ticks() - started) as f32 / interrupts::TIMER_HZ as f32;
        self.status = alloc::format!("Done in {}.{} s", secs as u32, (secs * 10.0) as u32 % 10);
    }

    fn set_page(&mut self, page: Page) {
        self.address = match &page.url {
            Some(u) => u.to_string(),
            None => String::new(),
        };
        self.layout = layout::layout(&page.doc, page_width(), &TextMetrics);
        self.page = page;
        self.scroll = 0;
        self.hover = None;
        self.select_all = false;
    }

    fn back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.current = None; // do not push the page we leave
            self.navigate(prev);
        }
    }

    fn reload(&mut self) {
        if let Some(cur) = self.current.clone() {
            self.current = None;
            let cur = match cur {
                Nav::Post(u, _) => Nav::Get(u),
                n => n,
            };
            self.navigate(cur);
        }
    }

    fn follow(&mut self, link: u32) {
        let Some(href) = self.page.doc.links.get(link as usize).cloned() else {
            return;
        };
        if href.trim().eq_ignore_ascii_case(web::HOME) {
            self.navigate(Nav::Home);
            return;
        }
        if let Some(fragment) = href.trim().strip_prefix('#') {
            // same page: just go to the top when we can't find the anchor
            let _ = fragment;
            self.scroll = 0;
            return;
        }
        match self.page.resolve(&href) {
            Some(u) => self.navigate(Nav::Get(u)),
            None => self.status = alloc::format!("Can't open {}", href),
        }
    }

    fn submit(&mut self, form: u32) {
        let Some(f) = self.page.doc.forms.get(form as usize) else {
            return;
        };
        let mut query = String::new();
        for field in &f.fields {
            if field.name.is_empty() {
                continue;
            }
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&url::encode_query(&field.name));
            query.push('=');
            query.push_str(&url::encode_query(&field.value));
        }
        let action = if f.action.is_empty() {
            self.page.address()
        } else {
            f.action.clone()
        };
        let Some(mut target) = self.page.resolve(&action) else {
            return;
        };
        if f.post {
            self.navigate(Nav::Post(target, query));
        } else {
            let base = target.path.split('?').next().unwrap_or("/").to_string();
            target.path = alloc::format!("{}?{}", base, query);
            self.navigate(Nav::Get(target));
        }
    }

    fn max_scroll(&self) -> i32 {
        (self.layout.height + 2 * PAD_Y - content_rect().h).max(0)
    }

    fn scroll_by(&mut self, dy: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0, self.max_scroll());
        self.scroll != old
    }

    // ---- editing the address bar and form fields --------------------------

    fn edit_text(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::Address => Some(&mut self.address),
            Focus::Field(form, field) => self
                .page
                .doc
                .forms
                .get_mut(form as usize)
                .and_then(|f| f.fields.get_mut(field as usize))
                .map(|f| &mut f.value),
            Focus::Page => None,
        }
    }

    fn edit_key(&mut self, key: Key) -> bool {
        let mut cursor = self.cursor;
        let select_all = core::mem::replace(&mut self.select_all, false);
        let Some(text) = self.edit_text() else {
            return false;
        };
        let len = text.chars().count();
        cursor = cursor.min(len);
        let byte = |t: &String, i: usize| t.char_indices().nth(i).map_or(t.len(), |(b, _)| b);
        match key {
            Key::Char(c) if c != '\t' => {
                if select_all {
                    text.clear();
                    cursor = 0;
                }
                if text.len() < 1024 {
                    let b = byte(text, cursor);
                    text.insert(b, c);
                    cursor += 1;
                }
            }
            Key::Backspace => {
                if select_all {
                    text.clear();
                    cursor = 0;
                } else if cursor > 0 {
                    let b = byte(text, cursor - 1);
                    text.remove(b);
                    cursor -= 1;
                }
            }
            Key::Delete => {
                if select_all {
                    text.clear();
                    cursor = 0;
                } else if cursor < len {
                    let b = byte(text, cursor);
                    text.remove(b);
                }
            }
            Key::Left => cursor = cursor.saturating_sub(1),
            Key::Right => cursor = (cursor + 1).min(len),
            Key::Home => cursor = 0,
            Key::End => cursor = len,
            Key::Ctrl('a') => {
                self.select_all = true;
                self.cursor = len;
                return true;
            }
            Key::Enter => {
                match self.focus {
                    Focus::Address => {
                        let text = self.address.trim().to_string();
                        if text.is_empty() || text.eq_ignore_ascii_case(web::HOME) {
                            self.navigate(Nav::Home);
                        } else if let Some(u) = web::address_to_url(&text) {
                            self.navigate(Nav::Get(u));
                        }
                    }
                    Focus::Field(form, _) => self.submit(form),
                    Focus::Page => {}
                }
                return true;
            }
            Key::Escape => {
                if self.focus == Focus::Address {
                    self.address = self
                        .page
                        .url
                        .as_ref()
                        .map(|u| u.to_string())
                        .unwrap_or_default();
                }
                self.focus = Focus::Page;
                return true;
            }
            _ => return false,
        }
        self.cursor = cursor;
        true
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.focus != Focus::Page {
            return self.edit_key(key);
        }
        let page = content_rect().h - 40;
        match key {
            Key::Up => self.scroll_by(-40),
            Key::Down => self.scroll_by(40),
            Key::PageUp => self.scroll_by(-page),
            Key::PageDown | Key::Char(' ') => self.scroll_by(page),
            Key::Home => self.scroll_by(-self.scroll),
            Key::End => self.scroll_by(self.max_scroll()),
            Key::Backspace => {
                self.back();
                true
            }
            Key::Ctrl('l') => {
                self.focus_address();
                true
            }
            Key::Ctrl('r') => {
                self.reload();
                true
            }
            _ => false,
        }
    }

    /// Where the address bar shows its text (right of the padlock).
    fn address_text_rect(&self) -> Rect {
        let r = address_rect();
        let lock = self.page.url.as_ref().is_some_and(|u| u.https) && self.focus != Focus::Address;
        let x = r.x + if lock { 32 } else { 14 };
        Rect::new(x, r.y, r.right() - 14 - x, r.h)
    }

    fn focus_address(&mut self) {
        self.focus = Focus::Address;
        self.select_all = true;
        self.cursor = self.address.chars().count();
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        self.scroll_by(clicks * 60)
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } => self.press(ev.x, ev.y),
            MouseKind::Down { right: true } => false,
            MouseKind::Move => {
                if let Pressed::Thumb(grab) = self.pressed {
                    let track = scrollbar_rect();
                    let (_, thumb_h) = self.thumb();
                    let room = (track.h - thumb_h).max(1);
                    let top = ev.y - grab - track.y;
                    let scroll = top * self.max_scroll() / room;
                    let old = self.scroll;
                    self.scroll = scroll.clamp(0, self.max_scroll());
                    return old != self.scroll;
                }
                false
            }
            MouseKind::Up => {
                let pressed = core::mem::replace(&mut self.pressed, Pressed::None);
                let inside = |r: Rect| r.contains(ev.x, ev.y);
                match pressed {
                    Pressed::Back if inside(back_rect()) => self.back(),
                    Pressed::Reload if inside(reload_rect()) => self.reload(),
                    Pressed::Home if inside(home_rect()) => self.navigate(Nav::Home),
                    Pressed::Go if inside(go_rect()) => {
                        self.focus = Focus::Address;
                        self.edit_key(Key::Enter);
                    }
                    _ => {}
                }
                pressed != Pressed::None
            }
        }
    }

    /// The mouse moved over the window without a button held.
    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let area = content_rect();
        let mut hover = None;
        if area.contains(x, y) {
            let px = x - area.x - PAD_X;
            let py = y - area.y - PAD_Y + self.scroll;
            if let Some(link) = self.layout.run_at(px, py).and_then(|r| r.style.link) {
                let href = &self.page.doc.links[link as usize];
                hover = Some(match self.page.resolve(href) {
                    Some(u) => u.to_string(),
                    None => href.clone(),
                });
            }
        }
        if hover != self.hover {
            self.hover = hover;
            return true;
        }
        false
    }

    /// Top and height of the scroll bar thumb.
    fn thumb(&self) -> (i32, i32) {
        let track = scrollbar_rect();
        let total = self.layout.height + 2 * PAD_Y;
        let view = content_rect().h;
        if total <= view {
            return (track.y, track.h);
        }
        let h = (track.h * view / total).max(30);
        let top = track.y + (track.h - h) * self.scroll / self.max_scroll().max(1);
        (top, h)
    }

    fn press(&mut self, x: i32, y: i32) -> bool {
        let hit = |r: Rect| r.contains(x, y);
        if hit(back_rect()) {
            self.pressed = Pressed::Back;
            return true;
        }
        if hit(reload_rect()) {
            self.pressed = Pressed::Reload;
            return true;
        }
        if hit(home_rect()) {
            self.pressed = Pressed::Home;
            return true;
        }
        if hit(go_rect()) {
            self.pressed = Pressed::Go;
            return true;
        }
        if hit(address_rect()) {
            if self.focus == Focus::Address && !self.select_all {
                // place the cursor where clicked
                self.cursor = click_cursor(self.address_text_rect(), &self.address, self.cursor, x);
            } else {
                self.focus_address();
            }
            return true;
        }
        if hit(scrollbar_rect()) {
            let (top, h) = self.thumb();
            if y >= top && y < top + h {
                self.pressed = Pressed::Thumb(y - top);
            } else {
                let page = content_rect().h - 40;
                self.scroll_by(if y < top { -page } else { page });
            }
            return true;
        }
        if hit(content_rect()) {
            let px = x - content_rect().x - PAD_X;
            let py = y - content_rect().y - PAD_Y + self.scroll;
            let run = self.layout.run_at(px, py).cloned();
            let was_editing = self.focus != Focus::Page;
            self.focus = Focus::Page;
            if let Some(run) = run {
                match run.kind {
                    RunKind::Input(form, field) => {
                        self.focus = Focus::Field(form, field);
                        self.select_all = false;
                        self.cursor = self.page.doc.forms[form as usize].fields[field as usize]
                            .value
                            .chars()
                            .count();
                        return true;
                    }
                    RunKind::Button(form, _) => {
                        self.submit(form);
                        return true;
                    }
                    _ => {}
                }
                if let Some(link) = run.style.link {
                    self.follow(link);
                    return true;
                }
            }
            return was_editing;
        }
        false
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas) {
        self.draw_toolbar(c);
        self.draw_page(c);
        self.draw_scrollbar(c);
        self.draw_status(c);
    }

    fn draw_toolbar(&self, c: &mut Canvas) {
        let bar = Rect::new(0, 0, CLIENT_W, TOOLBAR_H);
        c.fill(bar, theme::FACE);
        c.fill_rect(0, TOOLBAR_H - 1, CLIENT_W, 1, theme::STROKE);

        let icon_button = |c: &mut Canvas, r: Rect, pressed: bool, enabled: bool| {
            if pressed {
                c.fill_round(
                    r,
                    theme::CONTROL_RADIUS,
                    mix(theme::FACE, theme::SHADOW, 90),
                );
            }
            if enabled {
                theme::TEXT
            } else {
                mix(theme::TEXT_DIM, theme::FACE, 110)
            }
        };

        // back: an arrow
        let r = back_rect();
        let col = icon_button(
            c,
            r,
            self.pressed == Pressed::Back,
            !self.history.is_empty(),
        );
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        stroke(c, cx - 7.0, cy, cx + 7.0, cy, 2.0, col);
        stroke(c, cx - 7.0, cy, cx - 1.0, cy - 6.0, 2.0, col);
        stroke(c, cx - 7.0, cy, cx - 1.0, cy + 6.0, 2.0, col);

        // reload: an almost closed circle with an arrow head
        let r = reload_rect();
        let col = icon_button(c, r, self.pressed == Pressed::Reload, true);
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        let steps = 20;
        let mut prev = None;
        for i in 0..=steps {
            let a = 0.9 + 5.0 * i as f32 / steps as f32;
            let p = (cx + 7.0 * cos(a), cy - 7.0 * sin(a));
            if let Some((px, py)) = prev {
                stroke(c, px, py, p.0, p.1, 2.0, col);
            }
            prev = Some(p);
        }
        let (ax, ay) = (cx + 7.0 * cos(0.9), cy - 7.0 * sin(0.9));
        stroke(c, ax, ay, ax - 5.0, ay - 1.0, 2.0, col);
        stroke(c, ax, ay, ax + 1.0, ay - 5.5, 2.0, col);

        // home: a house
        let r = home_rect();
        let col = icon_button(c, r, self.pressed == Pressed::Home, true);
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        stroke(c, cx - 8.0, cy - 1.0, cx, cy - 8.0, 2.0, col);
        stroke(c, cx, cy - 8.0, cx + 8.0, cy - 1.0, 2.0, col);
        stroke(c, cx - 5.5, cy - 3.0, cx - 5.5, cy + 7.0, 2.0, col);
        stroke(c, cx + 5.5, cy - 3.0, cx + 5.5, cy + 7.0, 2.0, col);
        stroke(c, cx - 5.5, cy + 7.0, cx + 5.5, cy + 7.0, 2.0, col);

        // address bar
        let r = address_rect();
        let focused = self.focus == Focus::Address;
        c.fill_round(r, r.h / 2, theme::LIGHT);
        c.outline_round(
            r,
            r.h / 2,
            if focused {
                theme::ACCENT
            } else {
                theme::STROKE
            },
        );
        if focused {
            c.outline_round(r.inset(1), r.h / 2 - 1, theme::ACCENT);
        }
        let https = self.page.url.as_ref().is_some_and(|u| u.https) && !focused;
        let mut tx = r.x + 14;
        if https {
            // a small padlock
            let (lx, ly) = (tx, r.y + 10);
            c.fill_round(Rect::new(lx, ly + 6, 11, 9), 2, rgb(0x2e, 0x7d, 0x32));
            c.outline_round(Rect::new(lx + 2, ly, 7, 12), 3, rgb(0x2e, 0x7d, 0x32));
            c.outline_round(Rect::new(lx + 3, ly + 1, 5, 10), 2, rgb(0x2e, 0x7d, 0x32));
            tx += 18;
        }
        let _ = tx;
        draw_edit(
            c,
            self.address_text_rect(),
            &self.address,
            "Search or type a web address",
            self.cursor,
            focused,
            self.select_all,
        );

        let loading = self.pending.is_some();
        theme::accent_button(
            c,
            go_rect(),
            if loading { "..." } else { "Go" },
            self.pressed == Pressed::Go,
        );
    }

    fn draw_page(&self, c: &mut Canvas) {
        let area = content_rect();
        let mut c = c.sub(area);
        c.fill(Rect::new(0, 0, area.w, area.h), PAGE_BG);
        let top = self.scroll - PAD_Y;
        let first = self.layout.first_line_below(top);
        for line in &self.layout.lines[first..] {
            let y = line.y - top;
            if y > area.h {
                break;
            }
            for run in &line.runs {
                let x = PAD_X + run.x;
                let ry = y + line.h - run.h;
                match &run.kind {
                    RunKind::Text(text) => {
                        let color = if run.style.link.is_some() {
                            LINK
                        } else if run.style.faint {
                            FAINT
                        } else if run.style.heading > 0 {
                            HEADING
                        } else {
                            TEXT
                        };
                        let style = run.style;
                        let th = TextMetrics.line_height(style);
                        let text_h = font_for(style).line_height;
                        let ty = ry + (th - text_h) / 2;
                        if style.mono && !style.link.is_some() {
                            c.fill_rect(x, ty - 1, run.w, text_h + 2, rgb(0xf1, 0xf3, 0xf4));
                        }
                        draw_styled(&mut c, x, ty, text, style, color);
                    }
                    RunKind::Rule => {
                        c.fill_rect(x, ry + run.h / 2, run.w, 1, rgb(0xd0, 0xd0, 0xd4));
                    }
                    RunKind::Input(form, field) => {
                        let r = Rect::new(x + 2, ry + 2, run.w - 6, layout::FIELD_H);
                        let focused = self.focus == Focus::Field(*form, *field);
                        c.fill_round(r, 4, theme::LIGHT);
                        c.outline_round(
                            r,
                            4,
                            if focused {
                                theme::ACCENT
                            } else {
                                rgb(0x9a, 0xa0, 0xa6)
                            },
                        );
                        let f = &self.page.doc.forms[*form as usize].fields[*field as usize];
                        let inner = Rect::new(r.x + 8, r.y, r.w - 16, r.h);
                        draw_edit(
                            &mut c,
                            inner,
                            &f.value,
                            &f.placeholder,
                            self.cursor,
                            focused,
                            false,
                        );
                    }
                    RunKind::Button(_, label) => {
                        let r = Rect::new(x + 2, ry + 2, run.w - 6, layout::FIELD_H);
                        theme::button(&mut c, r, label, false);
                    }
                }
            }
        }
    }

    fn draw_scrollbar(&self, c: &mut Canvas) {
        let track = scrollbar_rect();
        c.fill(track, rgb(0xf6, 0xf6, 0xf6));
        c.fill_rect(track.x, track.y, 1, track.h, rgb(0xe6, 0xe6, 0xe6));
        if self.max_scroll() > 0 {
            let (top, h) = self.thumb();
            let active = matches!(self.pressed, Pressed::Thumb(_));
            let color = if active {
                rgb(0x80, 0x80, 0x84)
            } else {
                rgb(0xb8, 0xb8, 0xbc)
            };
            c.fill_round(
                Rect::new(track.x + 3, top + 2, track.w - 5, h - 4),
                3,
                color,
            );
        }
    }

    fn draw_status(&self, c: &mut Canvas) {
        let bar = Rect::new(0, CLIENT_H - STATUS_H, CLIENT_W, STATUS_H);
        c.fill(bar, theme::FACE);
        c.fill_rect(0, bar.y, CLIENT_W, 1, theme::STROKE);
        let ty = bar.y + (STATUS_H - UI.line_height) / 2;
        let loading = self.pending.is_some();
        let mut x = 10;
        if loading {
            c.fill_round(Rect::new(x, bar.y + 8, 10, 10), 5, theme::ACCENT);
            x += 18;
        }
        let status = match (&self.hover, loading) {
            (Some(link), false) => link.as_str(),
            _ => self.status.as_str(),
        };
        let title: String = self.page.doc.title.chars().take(80).collect();
        let right_w = UI.width(&title);
        c.draw_text(CLIENT_W - right_w - 14, ty, &title, theme::TEXT);
        let mut left = c.sub(Rect::new(x, bar.y, CLIENT_W - x - right_w - 40, STATUS_H));
        left.draw_text(0, ty - bar.y, status, theme::TEXT_DIM);
    }
}

/// Cumulative advance before each character of `text`, in 1/16 pixels.
fn advances(text: &str) -> Vec<i32> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut pen = 0;
    out.push(0);
    for ch in text.chars() {
        pen += UI.advance16(ch) as i32;
        out.push(pen);
    }
    out
}

/// The first character a text box shows, so that the cursor is visible.
fn first_visible(adv: &[i32], cursor: usize, width: i32) -> usize {
    let mut start = 0;
    while start < cursor && adv[cursor] - adv[start] > (width - 4) * 16 {
        start += 1;
    }
    start
}

/// A one-line text box: `r` is the area for the text.
fn draw_edit(
    c: &mut Canvas,
    r: Rect,
    text: &str,
    placeholder: &str,
    cursor: usize,
    focused: bool,
    select_all: bool,
) {
    let mut inner = c.sub(r);
    let ty = (r.h - UI.line_height) / 2;
    if text.is_empty() {
        if !focused {
            inner.draw_text(0, ty, placeholder, rgb(0x80, 0x86, 0x8c));
        } else {
            inner.fill_rect(0, ty, 1, UI.line_height, theme::TEXT);
        }
        return;
    }
    let adv = advances(text);
    let cursor = cursor.min(adv.len() - 1);
    let start = if focused {
        first_visible(&adv, cursor, r.w)
    } else {
        0
    };
    let shown: String = text.chars().skip(start).collect();
    if focused && select_all {
        let w = ((adv[adv.len() - 1] - adv[start]) / 16).min(r.w);
        inner.fill_rect(
            0,
            ty,
            w,
            UI.line_height,
            mix(theme::ACCENT, theme::LIGHT, 60),
        );
    }
    inner.draw_text(0, ty, &shown, theme::TEXT);
    if focused && !select_all {
        let x = (adv[cursor] - adv[start] + 8) / 16;
        inner.fill_rect(x, ty, 1, UI.line_height, theme::TEXT);
    }
}

/// Where a click at `x` puts the cursor in a text box.
fn click_cursor(r: Rect, text: &str, cursor: usize, x: i32) -> usize {
    let adv = advances(text);
    let cursor = cursor.min(adv.len() - 1);
    let start = first_visible(&adv, cursor, r.w);
    let target = (x - r.x) * 16 + adv[start];
    (start..adv.len())
        .min_by_key(|&i| (adv[i] - target).abs())
        .unwrap_or(0)
}

fn sin(x: f32) -> f32 {
    // reduce to [-pi, pi], then a Taylor series
    let pi = core::f32::consts::PI;
    let mut x = x % (2.0 * pi);
    if x > pi {
        x -= 2.0 * pi;
    } else if x < -pi {
        x += 2.0 * pi;
    }
    let x2 = x * x;
    x * (1.0
        - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0 * (1.0 - x2 / 110.0)))))
}

fn cos(x: f32) -> f32 {
    sin(x + core::f32::consts::FRAC_PI_2)
}
