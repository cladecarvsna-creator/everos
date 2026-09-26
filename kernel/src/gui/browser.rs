//! EverBrowser: a toolbar with Back, Reload, Home and the address bar,
//! the page below it and a status bar. Pages come from `crate::web`.
//!
//! Loading blocks the desktop for a moment: a click only records where
//! to go, the desktop draws "Loading...", and the next `tick` fetches it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::text::UI;
use super::theme;
use super::webfont;
use super::{MouseEvent, MouseKind};
use crate::keyboard::Key;
use crate::sync::IrqMutex;
use crate::web::dom::NodeId;
use crate::web::layout::{self, Control, Item};
use crate::web::{self, Nav, Page};
use crate::{interrupts, net};

pub const CLIENT_W: i32 = 1500;
pub const CLIENT_H: i32 = 900;

const TOOLBAR_H: i32 = 48;
const STATUS_H: i32 = 26;
const SCROLLBAR_W: i32 = 12;
const BUTTON: i32 = 34;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Page,
    Address,
    Field(NodeId),
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
    /// The text of the focused form field.
    field_text: String,
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

/// The page's viewport: the content area.
fn viewport() -> (i32, i32) {
    (content_rect().w, content_rect().h)
}

/// A web colour (0xAARRGGBB) as a desktop colour and its opacity (0 to 256).
fn web_color(c: u32) -> (Color, i32) {
    let a = (c >> 24) as i32;
    (c & 0xff_ffff, if a >= 255 { 256 } else { a })
}

impl Browser {
    pub fn new() -> Self {
        Browser {
            page: web::home(viewport()),
            scroll: 0,
            history: Vec::new(),
            current: Some(Nav::Home),
            pending: None,
            address: String::new(),
            cursor: 0,
            select_all: false,
            focus: Focus::Address,
            field_text: String::new(),
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
        let mut redraw = false;
        if self.page.run_timers(false) {
            self.after_script();
            redraw |= self.page.update();
        }
        if self.pending.is_none() && self.page.images_pending() > 0 {
            self.page.load_next_image();
            if self.page.images_pending() == 0 || self.page.images.len().is_multiple_of(4) {
                redraw |= self.page.update();
                self.scroll = self.scroll.clamp(0, self.max_scroll());
            }
        }
        if self.status.starts_with("Network card") && net::configured() {
            self.status = alloc::format!("Online, address {}", net::address().unwrap_or_default());
            return true;
        }
        redraw
    }

    /// Act on what a script asked for: going somewhere, back, scrolling.
    fn after_script(&mut self) {
        if let Some(nav) = self.page.st.nav.take() {
            self.navigate(nav);
        }
        if core::mem::take(&mut self.page.st.back) {
            self.back();
        }
        self.page.update();
        if let Some(n) = self.page.st.scroll_to.take() {
            if let Some(y) = self.page.element_top(n) {
                self.scroll = y.clamp(0, self.max_scroll());
            }
        }
        // the address follows history.pushState
        if self.focus != Focus::Address && self.pending.is_none() {
            if let Some(u) = &self.page.url {
                self.address = u.to_string();
            }
        }
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
            Nav::Home => web::home(viewport()),
            Nav::Get(u) => web::load(u, None, viewport(), true),
            Nav::Post(u, body) => web::load(u, Some(body), viewport(), true),
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
        // a script may have moved on straight away
        self.after_script();
    }

    fn set_page(&mut self, page: Page) {
        self.address = match &page.url {
            Some(u) => u.to_string(),
            None => String::new(),
        };
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

    fn max_scroll(&self) -> i32 {
        (self.page.layout.height - content_rect().h).max(0)
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
            Focus::Field(_) => Some(&mut self.field_text),
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
                    Focus::Field(n) => {
                        if let Some(form) = self.page.form_of(n) {
                            let nav = self.page.submit(form, None);
                            if let Some(nav) = nav {
                                self.navigate(nav);
                            }
                        }
                        self.after_script();
                    }
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
        if let Focus::Field(n) = self.focus {
            let value = self.field_text.clone();
            self.page.set_field(n, &value);
            self.after_script();
        }
        true
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.focus != Focus::Page {
            return self.edit_key(key);
        }
        let name = match key {
            Key::Up => "ArrowUp",
            Key::Down => "ArrowDown",
            Key::Left => "ArrowLeft",
            Key::Right => "ArrowRight",
            Key::Enter => "Enter",
            Key::Escape => "Escape",
            _ => "",
        };
        if !name.is_empty() && !self.page.key_event(None, name) {
            self.after_script();
            return true;
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
            let (px, py) = (x - area.x, y - area.y + self.scroll);
            if let Some(a) = self
                .page
                .element_at(px, py)
                .and_then(|n| self.page.link_of(n))
            {
                hover = self.page.link_target(a);
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
        let total = self.page.layout.height;
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
            let area = content_rect();
            let (px, py) = (x - area.x, y - area.y + self.scroll);
            self.focus = Focus::Page;
            let Some(node) = self.page.element_at(px, py) else {
                return true;
            };
            // a text field takes the focus
            let tag = self.page.dom.tag(node).to_string();
            let ty = self
                .page
                .dom
                .attr(node, "type")
                .unwrap_or("text")
                .to_ascii_lowercase();
            let is_field = tag == "textarea"
                || tag == "input"
                    && !matches!(
                        ty.as_str(),
                        "submit"
                            | "button"
                            | "reset"
                            | "checkbox"
                            | "radio"
                            | "image"
                            | "hidden"
                            | "file"
                    );
            self.page.dispatch(node, "mousedown", px, py);
            self.page.dispatch(node, "mouseup", px, py);
            if is_field {
                self.focus = Focus::Field(node);
                self.field_text = self.page.field_value(node);
                self.cursor = self.field_text.chars().count();
                self.select_all = false;
                self.page.dispatch(node, "focus", px, py);
            }
            if self.page.dispatch(node, "click", px, py) {
                if let Some(nav) = self.page.default_action(node) {
                    self.navigate(nav);
                }
            }
            self.after_script();
            return true;
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
        let mut page = c.sub(area);
        let (bg, _) = web_color(self.page.layout.canvas);
        page.fill(Rect::new(0, 0, area.w, area.h), bg);
        let dy = -self.scroll;
        let view = Rect::new(0, 0, area.w, area.h);
        let mut clips: Vec<Rect> = alloc::vec![view];
        for item in &self.page.layout.items {
            let clip = *clips.last().unwrap();
            let r = |r: &layout::Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
            match item {
                Item::Clip(cr) => {
                    clips.push(clip.intersect(&r(cr)));
                    continue;
                }
                Item::Unclip => {
                    if clips.len() > 1 {
                        clips.pop();
                    }
                    continue;
                }
                Item::None => continue,
                _ => {}
            }
            if clip.is_empty() {
                continue;
            }
            // skip what is off screen
            let bounds = match item {
                Item::Fill { r: b, .. }
                | Item::Border { r: b, .. }
                | Item::Image { r: b, .. }
                | Item::Control { r: b, .. } => r(b),
                Item::Text {
                    x,
                    baseline,
                    w,
                    size,
                    ..
                } => Rect::new(
                    *x - 2,
                    baseline + dy - (*size as i32) * 2,
                    w + 4,
                    (*size as i32) * 3,
                ),
                _ => continue,
            };
            if bounds.intersect(&clip).is_empty() {
                continue;
            }
            let mut sub = page.sub(view);
            sub.clip_to(clip);
            self.draw_item(&mut sub, item, dy);
        }
    }

    fn draw_item(&self, c: &mut Canvas, item: &Item, dy: i32) {
        match item {
            Item::Fill { r, color, radius } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                let (col, a) = web_color(*color);
                if *radius > 0 || a < 256 {
                    c.fill_round_alpha(rr, *radius, col, a);
                } else {
                    c.fill(rr, col);
                }
            }
            Item::Border {
                r,
                widths,
                colors,
                radius,
            } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                let uniform = widths.iter().all(|&w| w == widths[0])
                    && colors.iter().all(|&c| c == colors[0]);
                if *radius > 1 && uniform && widths[0] <= 3 {
                    let (col, _) = web_color(colors[0]);
                    for i in 0..widths[0] {
                        c.outline_round(rr.inset(i), (*radius - i).max(0), col);
                    }
                    return;
                }
                let sides = [
                    Rect::new(rr.x, rr.y, rr.w, widths[0]),
                    Rect::new(rr.right() - widths[1], rr.y, widths[1], rr.h),
                    Rect::new(rr.x, rr.bottom() - widths[2], rr.w, widths[2]),
                    Rect::new(rr.x, rr.y, widths[3], rr.h),
                ];
                for (i, s) in sides.iter().enumerate() {
                    if widths[i] > 0 && colors[i] >> 24 != 0 {
                        let (col, a) = web_color(colors[i]);
                        c.fill_round_alpha(*s, 0, col, a);
                    }
                }
            }
            Item::Text {
                x,
                baseline,
                w,
                text,
                face,
                size,
                color,
                underline,
                strike,
            } => {
                let (col, _) = web_color(*color);
                let f = webfont::Face {
                    bold: face.bold,
                    italic: face.italic,
                    mono: face.mono,
                };
                let by = baseline + dy;
                webfont::draw(c, f, *size, *x, by, text, col);
                let thick = (*size as i32 / 14).max(1);
                if *underline {
                    c.fill_rect(*x, by + (*size as i32) / 8, *w, thick, col);
                }
                if *strike {
                    c.fill_rect(*x, by - (*size as i32) * 3 / 10, *w, thick, col);
                }
            }
            Item::Image { r, src, cover } => {
                if let Some(img) = self.page.images.get(src) {
                    draw_image(c, img, Rect::new(r.x, r.y + dy, r.w, r.h), *cover);
                }
            }
            Item::Control { r, node, kind } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                self.draw_control(c, rr, *node, *kind);
            }
            _ => {}
        }
    }

    fn draw_control(&self, c: &mut Canvas, r: Rect, node: NodeId, kind: Control) {
        let dom = &self.page.dom;
        match kind {
            Control::Checkbox | Control::Radio => {
                let checked = dom.attr(node, "checked").is_some();
                let bx = Rect::new(r.x, r.y, r.w.max(13), r.h.max(13));
                let radius = if kind == Control::Radio { bx.w / 2 } else { 3 };
                if checked {
                    c.fill_round(bx, radius, theme::ACCENT);
                    if kind == Control::Radio {
                        c.fill_round(bx.inset(4), (bx.w - 8) / 2, rgb(255, 255, 255));
                    } else {
                        let (x0, y0) = (bx.x as f32, bx.y as f32);
                        stroke(
                            c,
                            x0 + 3.0,
                            y0 + 7.0,
                            x0 + 5.5,
                            y0 + 10.0,
                            2.0,
                            rgb(255, 255, 255),
                        );
                        stroke(
                            c,
                            x0 + 5.5,
                            y0 + 10.0,
                            x0 + 10.5,
                            y0 + 3.5,
                            2.0,
                            rgb(255, 255, 255),
                        );
                    }
                } else {
                    c.fill_round(bx, radius, rgb(255, 255, 255));
                    c.outline_round(bx, radius, rgb(0x76, 0x76, 0x76));
                }
            }
            Control::Select => {
                let text = self.page.field_value(node);
                let label = dom
                    .descendants(node)
                    .into_iter()
                    .filter(|&o| dom.tag(o) == "option")
                    .find(|&o| {
                        dom.attr(o, "selected").is_some()
                            || dom.attr(o, "value").unwrap_or("") == text
                    })
                    .map(|o| web::text::collapse(&dom.text_content(o)))
                    .unwrap_or(text);
                let ty = r.y + (r.h - UI.line_height) / 2;
                let mut inner = c.sub(Rect::new(r.x, r.y, (r.w - 18).max(0), r.h));
                inner.draw_text(0, ty - r.y, &label, theme::TEXT);
                let (ax, ay) = ((r.right() - 10) as f32, (r.y + r.h / 2) as f32);
                stroke(c, ax - 4.0, ay - 2.0, ax, ay + 2.0, 1.5, theme::TEXT);
                stroke(c, ax, ay + 2.0, ax + 4.0, ay - 2.0, 1.5, theme::TEXT);
            }
            Control::Text | Control::Password | Control::TextArea => {
                let focused = self.focus == Focus::Field(node);
                let value = if focused {
                    self.field_text.clone()
                } else {
                    self.page.field_value(node)
                };
                let shown = if kind == Control::Password {
                    value.chars().map(|_| '•').collect()
                } else if kind == Control::TextArea {
                    value.replace('\n', " ")
                } else {
                    value
                };
                let placeholder = dom.attr(node, "placeholder").unwrap_or("");
                let line = if kind == Control::TextArea {
                    Rect::new(r.x, r.y, r.w, UI.line_height + 4)
                } else {
                    r
                };
                draw_edit(c, line, &shown, placeholder, self.cursor, focused, false);
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
        let title: String = self.page.title().chars().take(80).collect();
        let right_w = UI.width(&title);
        c.draw_text(CLIENT_W - right_w - 14, ty, &title, theme::TEXT);
        let mut left = c.sub(Rect::new(x, bar.y, CLIENT_W - x - right_w - 40, STATUS_H));
        left.draw_text(0, ty - bar.y, status, theme::TEXT_DIM);
    }
}

/// Draw an image scaled into `r`; with `cover`, scaled to cover it and cut.
fn draw_image(c: &mut Canvas, img: &web::image::Image, r: Rect, cover: bool) {
    if img.width == 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    let (iw, ih) = (img.width as i64, img.height as i64);
    // source pixels per destination pixel, in 1/1024
    let (sx, sy, ox, oy) = if cover {
        let scale = ((iw * 1024) / r.w as i64)
            .min((ih * 1024) / r.h as i64)
            .max(1);
        let ox = (iw * 1024 - scale * r.w as i64) / 2;
        let oy = (ih * 1024 - scale * r.h as i64) / 2;
        (scale, scale, ox.max(0), oy.max(0))
    } else {
        ((iw * 1024) / r.w as i64, (ih * 1024) / r.h as i64, 0, 0)
    };
    let step = ((sx.max(sy) + 512) / 1024).max(1) as usize;
    let visible = r.intersect(&c.clip_rect());
    for py in visible.y..visible.bottom() {
        let src_y = ((oy + (py - r.y) as i64 * sy) / 1024).clamp(0, ih - 1) as usize;
        for px in visible.x..visible.right() {
            let src_x = ((ox + (px - r.x) as i64 * sx) / 1024).clamp(0, iw - 1) as usize;
            let p = img.sample(src_x, src_y, step);
            let a = (p >> 24) as i32;
            if a == 255 {
                c.pixel(px, py, p & 0xff_ffff);
            } else if a > 0 {
                c.blend_at(px, py, p & 0xff_ffff, a);
            }
        }
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
