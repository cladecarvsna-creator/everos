//! Settings, like the Windows 11 app: pages in a list on the left and
//! cards of settings on the right. Most rows show how the system is set
//! up; the keyboard layout can be switched here, the About page opens
//! "About EverOS", and Personalization changes the look: light or dark
//! mode, the accent colour and the desktop background.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::icons::{self, Pic, SMALL};
use super::personalize::{self, Background, Prefs, COLORS, FITS};
use super::text::{TITLE, UI, UI_BOLD};
use super::tray::{self, Net};
use super::widgets::{FieldEvent, TextField};
use super::{picture, theme, wallpaper, App, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::fs;
use crate::keyboard::{Key, Layout};
use crate::net::config::{self, Proxy, ProxyKind};
use crate::net::wifi::{self, Adapter, State};
use alloc::rc::Rc;

pub const CLIENT_W: i32 = 940;
pub const CLIENT_H: i32 = 620;

const NAV_W: i32 = 260;
const NAV_TOP: i32 = 112;
const NAV_ROW: i32 = 40;
const PAGE_X: i32 = NAV_W + 24;
const ROW_H: i32 = 56;

/// What the pages show, collected by the desktop.
pub struct Info<'a> {
    pub screen: (i32, i32),
    pub memory_mib: u32,
    pub bootloader: &'a str,
    pub net: Net,
    pub address: &'a str,
    pub layout: Layout,
    pub clock: &'a str,
    pub date: &'a str,
    pub uptime_minutes: u64,
    /// The text caret's blink.
    pub caret: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    System,
    Personalization,
    Network,
    Proxy,
    Time,
    Accounts,
    About,
}

const PAGES: [(Page, &str); 7] = [
    (Page::System, "System"),
    (Page::Personalization, "Personalization"),
    (Page::Network, "Network & internet"),
    (Page::Proxy, "Proxy"),
    (Page::Time, "Time & language"),
    (Page::Accounts, "Accounts"),
    (Page::About, "About"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    SwitchLayout,
    OpenAbout,
    ShowNetworks,
}

pub struct Settings {
    page: Page,
    pressed: Option<Button>,
    /// Asks the desktop to switch the keyboard layout.
    pub switch_layout: bool,
    /// Choosing a picture for the background.
    dialog: RefCell<Option<FileDialog>>,
    /// Why the chosen file can't be the background.
    error: Option<&'static str>,
    /// Small pictures of the backgrounds, made when first shown.
    thumbs: RefCell<Thumbs>,
    /// Asks the desktop to open the list of Wi-Fi networks.
    pub show_wifi: bool,
    proxy: RefCell<ProxyForm>,
}

#[derive(Default)]
struct Thumbs {
    builtin: Vec<Vec<u32>>,
    /// The picture from the disk, and its path.
    picture: Option<(String, Vec<u32>)>,
    /// The whole look, and the settings it shows.
    preview: Option<(Prefs, Vec<u32>)>,
}

/// Something on the Personalization page that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Light (false) or dark (true).
    Mode(bool),
    Accent(usize),
    Slot(usize),
    Browse,
    Fit(usize),
    Color(usize),
}

// the Personalization page
const PAGE_W: i32 = CLIENT_W - PAGE_X - 28;
const PREVIEW: Rect = Rect::new(PAGE_X + 16, 84, 248, 140);
const THUMB_W: i32 = 120;
const THUMB_H: i32 = 68;
const SWATCH: i32 = 26;

fn mode_card() -> Rect {
    Rect::new(PAGE_X, 72, PAGE_W, 164)
}

fn accent_card() -> Rect {
    Rect::new(PAGE_X, 244, PAGE_W, 64)
}

fn background_card() -> Rect {
    Rect::new(PAGE_X, 316, PAGE_W, 176)
}

fn fit_card() -> Rect {
    Rect::new(PAGE_X, 500, PAGE_W, 52)
}

fn color_card() -> Rect {
    Rect::new(PAGE_X, 560, PAGE_W, 52)
}

fn mode_tile(dark: bool) -> Rect {
    let x = PAGE_X + 280 + if dark { 162 } else { 0 };
    Rect::new(x, 116, 150, 100)
}

/// Round colour swatch `i` of `n`, at the right end of card `r`.
fn swatch(r: Rect, i: usize, n: usize) -> Rect {
    let total = n as i32 * (SWATCH + 8) - 8;
    let x = r.right() - 20 - total + i as i32 * (SWATCH + 8);
    Rect::new(x, r.y + (r.h - SWATCH) / 2, SWATCH, SWATCH)
}

fn fit_button(i: usize) -> Rect {
    let r = fit_card();
    let total = FITS.len() as i32 * 78 - 6;
    Rect::new(r.right() - 16 - total + i as i32 * 78, r.y + 11, 72, 30)
}

fn browse_button() -> Rect {
    let r = background_card();
    Rect::new(r.right() - 16 - 130, r.y + 14, 130, 32)
}

fn slot_rect(i: usize) -> Rect {
    let r = background_card();
    Rect::new(
        r.x + 20 + i as i32 * (THUMB_W + 14),
        r.y + 64,
        THUMB_W,
        THUMB_H,
    )
}

/// The backgrounds to pick from: the built-in pictures, the picture
/// from the disk if one is used, and a plain colour.
fn slots(prefs: &Prefs) -> Vec<Background> {
    let mut out: Vec<Background> = (0..wallpaper::BUILTIN.len())
        .map(Background::Builtin)
        .collect();
    if let Background::Picture(_) = prefs.background {
        out.push(prefs.background.clone());
    }
    out.push(Background::Solid);
    out
}

fn targets(prefs: &Prefs) -> Vec<(Target, Rect)> {
    let mut out = Vec::new();
    out.push((Target::Mode(false), mode_tile(false)));
    out.push((Target::Mode(true), mode_tile(true)));
    for i in 0..theme::ACCENTS.len() {
        out.push((
            Target::Accent(i),
            swatch(accent_card(), i, theme::ACCENTS.len()),
        ));
    }
    for i in 0..slots(prefs).len() {
        out.push((Target::Slot(i), slot_rect(i)));
    }
    out.push((Target::Browse, browse_button()));
    for i in 0..FITS.len() {
        out.push((Target::Fit(i), fit_button(i)));
    }
    for i in 0..COLORS.len() {
        out.push((Target::Color(i), swatch(color_card(), i, COLORS.len())));
    }
    out
}

fn dialog_area() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

fn nav_rect(i: usize) -> Rect {
    Rect::new(12, NAV_TOP + i as i32 * NAV_ROW, NAV_W - 24, NAV_ROW - 4)
}

/// Row `i` of the card on the current page.
fn row_rect(i: usize) -> Rect {
    Rect::new(
        PAGE_X,
        72 + i as i32 * (ROW_H + 4),
        CLIENT_W - PAGE_X - 28,
        ROW_H,
    )
}

/// The button at the right end of row `i`.
fn row_button(i: usize) -> Rect {
    let r = row_rect(i);
    Rect::new(r.right() - 16 - 170, r.y + (ROW_H - 32) / 2, 170, 32)
}

impl Settings {
    pub fn new() -> Self {
        Self {
            page: Page::System,
            pressed: None,
            switch_layout: false,
            dialog: RefCell::new(None),
            error: None,
            thumbs: RefCell::new(Thumbs::default()),
            show_wifi: false,
            proxy: RefCell::new(ProxyForm::load()),
        }
    }

    /// Move the proxy check along. Returns whether the page changed.
    pub fn tick(&mut self) -> bool {
        self.proxy.get_mut().tick()
    }

    /// Whether a proxy check is running.
    pub fn busy(&self) -> bool {
        self.proxy.borrow().check.is_some()
    }

    /// Show a page, as "Personalize" on the desktop's menu asks.
    pub fn show_page(&mut self, page: Page) {
        self.page = page;
        self.pressed = None;
        if page == Page::Proxy {
            *self.proxy.get_mut() = ProxyForm::load();
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.page == Page::Proxy && self.dialog.get_mut().is_none() {
            return self.proxy.get_mut().on_key(key);
        }
        let event = match self.dialog.get_mut() {
            Some(d) => d.on_key(key, dialog_area()),
            None => return false,
        };
        self.dialog_event(event)
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        match self.dialog.get_mut() {
            Some(d) => d.on_wheel(clicks, dialog_area()),
            None => false,
        }
    }

    fn dialog_event(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            filedialog::Event::Cancel => {
                *self.dialog.get_mut() = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                *self.dialog.get_mut() = None;
                let readable = fs::read(&path)
                    .ok()
                    .is_some_and(|data| picture::decode(&data).is_some());
                if readable {
                    self.error = None;
                    personalize::set_wallpaper(&path);
                } else {
                    self.error =
                        Some("EverOS can't show that file. Pick a PNG, JPEG or BMP picture.");
                }
                true
            }
        }
    }

    /// A click on the Personalization page.
    fn personalize_click(&mut self, x: i32, y: i32) -> bool {
        let prefs = personalize::get();
        let Some((t, _)) = targets(&prefs).into_iter().find(|(_, r)| r.contains(x, y)) else {
            return false;
        };
        self.error = None;
        match t {
            Target::Mode(dark) => personalize::update(|p| p.dark = dark),
            Target::Accent(i) => personalize::update(|p| p.accent = theme::ACCENTS[i].1),
            Target::Slot(i) => {
                let bg = slots(&prefs)[i].clone();
                personalize::update(|p| p.background = bg);
            }
            Target::Browse => {
                let user = crate::users::current_name().unwrap_or_default();
                let dir = fs::join(&fs::home(user.as_str()), "Pictures");
                *self.dialog.get_mut() = Some(FileDialog::new(Mode::Open, &dir, ""));
            }
            Target::Fit(i) => personalize::update(|p| p.fit = FITS[i].0),
            Target::Color(i) => personalize::update(|p| p.color = COLORS[i]),
        }
        true
    }

    /// Where the page's button is, if it has one.
    fn button(&self) -> Option<(Button, Rect)> {
        match self.page {
            Page::Time => Some((Button::SwitchLayout, row_button(1))),
            Page::Network if wifi::state().is_some() => Some((Button::ShowNetworks, row_button(0))),
            Page::About => Some((Button::OpenAbout, row_button(4))),
            _ => None,
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        if self.dialog.get_mut().is_some() {
            if let MouseKind::Down { right: false } = ev.kind {
                let event = match self.dialog.get_mut() {
                    Some(d) => d.on_click(dialog_area(), ev.x, ev.y),
                    None => filedialog::Event::None,
                };
                return self.dialog_event(event);
            }
            return false;
        }
        match ev.kind {
            MouseKind::Down { right: false } => {
                if let Some(i) = (0..PAGES.len()).find(|&i| nav_rect(i).contains(ev.x, ev.y)) {
                    let changed = self.page != PAGES[i].0;
                    if changed {
                        self.show_page(PAGES[i].0);
                    }
                    return changed;
                }
                if self.page == Page::Personalization {
                    return self.personalize_click(ev.x, ev.y);
                }
                if self.page == Page::Proxy {
                    return self.proxy.get_mut().click(ev.x, ev.y);
                }
                if self.page == Page::Network && self.network_click(ev.x, ev.y) {
                    return true;
                }
                match self.button() {
                    Some((b, r)) if r.contains(ev.x, ev.y) => {
                        self.pressed = Some(b);
                        true
                    }
                    _ => false,
                }
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if self.button().is_some_and(|(_, r)| r.contains(ev.x, ev.y)) {
                    match b {
                        Button::SwitchLayout => self.switch_layout = true,
                        Button::OpenAbout => {
                            super::request_open(App::About);
                        }
                        Button::ShowNetworks => self.show_wifi = true,
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas, info: &Info) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::face());
        self.draw_nav(c, info);
        let title = PAGES.iter().find(|p| p.0 == self.page).map_or("", |p| p.1);
        c.draw_text_in(&TITLE, PAGE_X, 26, title, theme::text());

        let user = crate::users::current_name();
        let user = user.as_ref().map_or("nobody", |n| n.as_str());
        match self.page {
            Page::Personalization => self.draw_personalization(c),
            Page::System => {
                let storage = match fs::storage() {
                    fs::Storage::Disk => format!(
                        "Local Disk (C:), FAT32, {} MB",
                        fs::capacity() / (1024 * 1024)
                    ),
                    _ => String::from("No disk: files are kept in memory"),
                };
                let uptime = format!(
                    "{} h {} min",
                    info.uptime_minutes / 60,
                    info.uptime_minutes % 60
                );
                self.rows(
                    c,
                    &[
                        (
                            "Display",
                            "Resolution",
                            &format!("{} x {}", info.screen.0, info.screen.1),
                        ),
                        (
                            "Memory",
                            "Installed RAM",
                            &format!("{} MB", info.memory_mib),
                        ),
                        ("Storage", "Where your files are saved", &storage),
                        ("Uptime", "Time since EverOS started", &uptime),
                    ],
                );
            }
            Page::Network => self.draw_network(c, info),
            Page::Proxy => self.proxy.borrow_mut().draw(c, info.caret),
            Page::Time => {
                let layout = match info.layout {
                    Layout::Us => "English (ENG)",
                    Layout::Ru => "Russian (РУС)",
                };
                let now = format!("{}   {}", info.clock, info.date);
                self.rows(
                    c,
                    &[
                        ("Date and time", "From the computer's clock", &now),
                        ("Keyboard layout", "Alt+Shift also switches it", layout),
                    ],
                );
                let label = match info.layout {
                    Layout::Us => "Switch to Russian",
                    Layout::Ru => "Switch to English",
                };
                self.draw_button(c, Button::SwitchLayout, label);
            }
            Page::Accounts => {
                let mut rows: [(String, &str); 8] = Default::default();
                let n = crate::users::count().min(rows.len());
                for (i, row) in rows.iter_mut().enumerate().take(n) {
                    let name = crate::users::name(i);
                    row.0 = String::from(name.as_ref().map_or("", |n| n.as_str()));
                    row.1 = if crate::users::has_password(i) {
                        "Password set"
                    } else {
                        "No password"
                    };
                }
                for (i, (name, password)) in rows[..n].iter().enumerate() {
                    let r = row_rect(i);
                    card(c, r);
                    avatar(c, r.x + 16, r.y + 12, 32, name);
                    c.draw_text(r.x + 60, r.y + 10, name, theme::text());
                    let kind = if name == user {
                        "Signed in, local account"
                    } else {
                        "Local account"
                    };
                    c.draw_text(r.x + 60, r.y + 29, kind, theme::text_dim());
                    let w = UI.width(password);
                    c.draw_text(r.right() - 20 - w, r.y + 19, password, theme::text_dim());
                }
            }
            Page::About => {
                self.rows(
                    c,
                    &[
                        ("Device name", "", "EVEROS-PC"),
                        (
                            "Operating system",
                            "",
                            &format!("EverOS {}", super::about::VERSION),
                        ),
                        ("Processor", "", "x86_64, long mode"),
                        ("Bootloader", "", info.bootloader),
                        ("About EverOS", "Version, license and this computer", ""),
                    ],
                );
                self.draw_button(c, Button::OpenAbout, "Open");
            }
        }
    }

    fn draw_nav(&self, c: &mut Canvas, info: &Info) {
        // the signed-in user at the top, like Windows
        let user = crate::users::current_name();
        let user = user.as_ref().map_or("nobody", |n| n.as_str());
        avatar(c, 20, 24, 56, user);
        c.draw_text_in(&UI_BOLD, 88, 34, user, theme::text());
        c.draw_text(88, 54, "Local account", theme::text_dim());

        for (i, &(page, label)) in PAGES.iter().enumerate() {
            let r = nav_rect(i);
            if page == self.page {
                c.fill_round(r, 5, theme::hover());
                c.fill_round(Rect::new(r.x, r.y + 10, 3, r.h - 20), 1, theme::accent());
            }
            let (x, y) = (r.x + 14, r.y + (r.h - 16) / 2);
            let pics = icons::get();
            match page {
                Page::System => pics.draw_pic(c, Pic::Computer, SMALL, x, y),
                Page::Network => {
                    let bg = if page == self.page {
                        theme::hover()
                    } else {
                        theme::face()
                    };
                    tray::network_icon(c, x, y + 1, info.net, theme::text(), bg);
                }
                Page::Proxy => globe_icon(c, x, y, theme::text()),
                Page::Time => {
                    c.outline_round(Rect::new(x, y, 16, 16), 8, theme::text());
                    c.fill_rect(x + 8, y + 3, 1, 6, theme::text());
                    c.fill_rect(x + 8, y + 8, 4, 1, theme::text());
                }
                Page::Accounts => {
                    c.fill_round(Rect::new(x + 4, y, 8, 8), 4, theme::accent());
                    c.fill_round(Rect::new(x + 1, y + 9, 14, 7), 3, theme::accent());
                }
                Page::About => pics.draw_small(c, App::About, x, y),
                Page::Personalization => brush_icon(c, x, y),
            }
            c.draw_text(
                r.x + 42,
                r.y + (r.h - UI.line_height) / 2,
                label,
                theme::text(),
            );
        }
    }

    /// One card per row: a title, a line under it and a value on the right.
    fn rows(&self, c: &mut Canvas, rows: &[(&str, &str, &str)]) {
        for (i, (title, note, value)) in rows.iter().enumerate() {
            let r = row_rect(i);
            card(c, r);
            if note.is_empty() {
                c.draw_text(
                    r.x + 20,
                    r.y + (ROW_H - UI.line_height) / 2,
                    title,
                    theme::text(),
                );
            } else {
                c.draw_text(r.x + 20, r.y + 10, title, theme::text());
                c.draw_text(r.x + 20, r.y + 29, note, theme::text_dim());
            }
            // values go left of a button in the same row
            let right = match self.button() {
                Some((_, b)) if r.contains(b.x, b.y) => b.x - 16,
                _ => r.right() - 20,
            };
            let w = UI.width(value);
            let y = r.y + (ROW_H - UI.line_height) / 2;
            c.draw_text(right - w, y, value, theme::text_dim());
        }
    }

    fn draw_button(&self, c: &mut Canvas, b: Button, label: &str) {
        if let Some((_, r)) = self.button() {
            theme::button(c, r, label, self.pressed == Some(b));
        }
    }
}

impl Settings {
    fn draw_personalization(&self, c: &mut Canvas) {
        let prefs = personalize::get();
        let mut thumbs = self.thumbs.borrow_mut();

        // the look now, with a little window and taskbar on it
        let r = mode_card();
        card(c, r);
        let fresh = matches!(&thumbs.preview, Some((p, _)) if p.background == prefs.background
            && p.fit == prefs.fit && p.color == prefs.color);
        if !fresh {
            let pixels = wallpaper::thumbnail(&prefs.background, &prefs, PREVIEW.w, PREVIEW.h);
            thumbs.preview = Some((prefs.clone(), pixels));
        }
        if let Some((_, pixels)) = &thumbs.preview {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(PREVIEW, 6);
            m.blit(
                PREVIEW.x,
                PREVIEW.y,
                PREVIEW.w,
                PREVIEW.h,
                pixels,
                PREVIEW.w as usize,
            );
            let bar = Rect::new(PREVIEW.x, PREVIEW.bottom() - 12, PREVIEW.w, 12);
            m.fill_round_alpha(bar, 0, theme::taskbar(), 220);
            for k in 0..4 {
                let dot = Rect::new(PREVIEW.x + PREVIEW.w / 2 - 22 + k * 12, bar.y + 3, 7, 6);
                m.fill_round(
                    dot,
                    1,
                    if k == 0 {
                        theme::accent()
                    } else {
                        theme::thumb()
                    },
                );
            }
            let win = Rect::new(PREVIEW.x + 60, PREVIEW.y + 26, 128, 76);
            mini_window(&mut m, win, prefs.dark);
        }
        c.outline_round(PREVIEW, 6, theme::stroke());

        let x0 = mode_tile(false).x;
        c.draw_text_in(&UI_BOLD, x0, 84, "Choose your mode", theme::text());
        for dark in [false, true] {
            let t = mode_tile(dark);
            let on = prefs.dark == dark;
            c.fill_round(t, 6, theme::control());
            if on {
                c.outline_round(t, 6, theme::accent());
                c.outline_round(t.inset(1), 5, theme::accent());
            } else {
                c.outline_round(t, 6, theme::stroke());
            }
            let pic = Rect::new(t.x + 12, t.y + 10, t.w - 24, 56);
            mini_window(c, pic, dark);
            let label = if dark { "Dark" } else { "Light" };
            c.text_centered(
                Rect::new(t.x, t.bottom() - 30, t.w, 24),
                label,
                theme::text(),
            );
        }

        // accent colours
        let r = accent_card();
        card(c, r);
        c.draw_text(r.x + 20, r.y + 12, "Accent color", theme::text());
        c.draw_text(
            r.x + 20,
            r.y + 31,
            "Buttons, highlights and selections",
            theme::text_dim(),
        );
        for (i, &(_, color)) in theme::ACCENTS.iter().enumerate() {
            let s = swatch(r, i, theme::ACCENTS.len());
            color_dot(c, s, color, color == prefs.accent);
        }

        // backgrounds
        let r = background_card();
        card(c, r);
        c.draw_text(r.x + 20, r.y + 12, "Background", theme::text());
        let note = self
            .error
            .unwrap_or("Pictures are scaled to fit any screen size");
        let note_color = if self.error.is_some() {
            theme::error()
        } else {
            theme::text_dim()
        };
        c.draw_text(r.x + 20, r.y + 31, note, note_color);
        theme::button(c, browse_button(), "Browse photos", false);
        if thumbs.builtin.len() != wallpaper::BUILTIN.len() {
            let fill = Prefs {
                fit: personalize::Fit::Fill,
                ..prefs.clone()
            };
            thumbs.builtin = (0..wallpaper::BUILTIN.len())
                .map(|i| wallpaper::thumbnail(&Background::Builtin(i), &fill, THUMB_W, THUMB_H))
                .collect();
        }
        if let Background::Picture(path) = &prefs.background {
            if thumbs.picture.as_ref().is_none_or(|(p, _)| p != path) {
                let fill = Prefs {
                    fit: personalize::Fit::Fill,
                    ..prefs.clone()
                };
                let pixels = wallpaper::thumbnail(&prefs.background, &fill, THUMB_W, THUMB_H);
                thumbs.picture = Some((path.clone(), pixels));
            }
        }
        for (i, bg) in slots(&prefs).iter().enumerate() {
            let s = slot_rect(i);
            let label = match bg {
                Background::Builtin(k) => {
                    c.blit(s.x, s.y, s.w, s.h, &thumbs.builtin[*k], THUMB_W as usize);
                    String::from(wallpaper::BUILTIN[*k].0)
                }
                Background::Picture(path) => {
                    if let Some((_, pixels)) = &thumbs.picture {
                        c.blit(s.x, s.y, s.w, s.h, pixels, THUMB_W as usize);
                    }
                    String::from(fs::file_name(path))
                }
                Background::Solid => {
                    c.fill(s, prefs.color);
                    String::from("Solid color")
                }
            };
            if *bg == prefs.background {
                c.outline_round(s.inset(-3), 6, theme::accent());
                c.outline_round(s.inset(-2), 5, theme::accent());
            } else {
                c.outline_round(s, 2, theme::stroke());
            }
            let mut label = label;
            while UI.width(&label) > THUMB_W && label.pop().is_some() {}
            c.text_centered(
                Rect::new(s.x, s.bottom() + 6, s.w, 20),
                &label,
                theme::text(),
            );
        }

        // how pictures fit, and the colour behind them
        let r = fit_card();
        card(c, r);
        c.draw_text(
            r.x + 20,
            r.y + (r.h - UI.line_height) / 2,
            "Fit to screen",
            theme::text(),
        );
        for (i, &(fit, name)) in FITS.iter().enumerate() {
            theme::toggle_button(c, fit_button(i), name, prefs.fit == fit);
        }
        let r = color_card();
        card(c, r);
        c.draw_text(
            r.x + 20,
            r.y + (r.h - UI.line_height) / 2,
            "Background color",
            theme::text(),
        );
        for (i, &color) in COLORS.iter().enumerate() {
            color_dot(c, swatch(r, i, COLORS.len()), color, color == prefs.color);
        }

        if let Some(d) = self.dialog.borrow_mut().as_mut() {
            d.draw(c, dialog_area(), true);
        }
    }
}

/// A round colour to pick, with a ring when it is the chosen one.
fn color_dot(c: &mut Canvas, r: Rect, color: u32, on: bool) {
    if on {
        c.fill_round(r.inset(-3), (r.w + 6) / 2, theme::text());
        c.fill_round(r.inset(-1), (r.w + 2) / 2, theme::light());
    }
    c.fill_round(r, r.w / 2, color);
    c.outline_round(r, r.w / 2, mix(color, theme::text(), 50));
}

/// A tiny window in the light or dark look, for the previews.
fn mini_window(c: &mut Canvas, r: Rect, dark: bool) {
    let (face, bar, ink) = if dark {
        (
            rgb(0x2b, 0x2b, 0x2b),
            rgb(0x1c, 0x1c, 0x1c),
            rgb(0xe0, 0xe0, 0xe0),
        )
    } else {
        (
            rgb(0xff, 0xff, 0xff),
            rgb(0xee, 0xf1, 0xf8),
            rgb(0x30, 0x30, 0x30),
        )
    };
    c.fill_round(r, 5, face);
    {
        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
        m.clip_round(r, 5);
        m.fill(Rect::new(r.x, r.y, r.w, 12), bar);
    }
    c.outline_round(
        r,
        5,
        if dark {
            rgb(0x50, 0x50, 0x54)
        } else {
            rgb(0xc0, 0xc2, 0xc8)
        },
    );
    for k in 0..3 {
        let w = if k == 2 { r.w / 3 } else { r.w - 24 };
        c.fill_round(
            Rect::new(r.x + 10, r.y + 20 + k * 9, w, 4),
            2,
            mix(face, ink, 90),
        );
    }
    let b = Rect::new(r.right() - 38, r.bottom() - 16, 28, 9);
    c.fill_round(b, 3, theme::accent_base());
}

/// A paint brush, for the Personalization page.
fn brush_icon(c: &mut Canvas, x: i32, y: i32) {
    c.fill_polygon(
        &[
            (x + 9, y + 9),
            (x + 14, y + 1),
            (x + 16, y + 3),
            (x + 11, y + 11),
        ],
        theme::text(),
    );
    c.fill_round(Rect::new(x + 2, y + 9, 8, 7), 3, theme::accent());
}

fn card(c: &mut Canvas, r: Rect) {
    c.fill_round(r, 6, theme::light());
    c.outline_round(r, 6, theme::stroke());
}

/// A round picture with the first letter of the name.
fn avatar(c: &mut Canvas, x: i32, y: i32, size: i32, name: &str) {
    let r = Rect::new(x, y, size, size);
    c.fill_round(r, size / 2, rgb(0x3a, 0x7c, 0xd0));
    let mut first = String::new();
    first.extend(name.chars().next().map(|ch| ch.to_ascii_uppercase()));
    let font = if size >= 48 { &TITLE } else { &UI_BOLD };
    c.text_centered_in(font, r, &first, 0xffffff);
}

/// A globe: a circle with a meridian and the equator, for Proxy.
fn globe_icon(c: &mut Canvas, x: i32, y: i32, ink: u32) {
    c.outline_round(Rect::new(x, y, 16, 16), 8, ink);
    c.outline_round(Rect::new(x + 4, y, 8, 16), 4, ink);
    c.fill_rect(x + 1, y + 8, 14, 1, ink);
    c.fill_rect(x + 8, y, 1, 16, ink);
}

// ---- Network & internet ----------------------------------------------------------

/// The on/off switch at the right end of row `i`.
fn row_switch(i: usize) -> Rect {
    let r = row_rect(i);
    Rect::new(r.right() - 20 - 40, r.y + (ROW_H - 20) / 2, 40, 20)
}

impl Settings {
    fn draw_network(&self, c: &mut Canvas, info: &Info) {
        let address = if info.address.is_empty() {
            "-"
        } else {
            info.address
        };
        let adapter = wifi::adapter();
        let state = wifi::state();
        let wifi_status = match &state {
            None => match &adapter {
                Adapter::Unsupported(_) => String::from("EverOS has no driver for this card yet"),
                _ => String::from("No Wi-Fi adapter"),
            },
            Some(State::Off) => String::from("Off"),
            Some(State::Disconnected) => String::from("Not connected"),
            Some(State::Connecting(s)) => format!("Connecting to {}...", s),
            Some(State::Connected { ssid, signal }) => {
                format!("Connected to {}, signal {}%", ssid, signal)
            }
            Some(State::Failed(s, why)) => format!("{}: {}", s, why),
        };
        let adapter_name = match &adapter {
            Adapter::None => String::from("None found"),
            Adapter::Unsupported(name) => name.clone(),
            Adapter::Virtual => String::from("Virtual adapter (test)"),
        };
        let ethernet = if adapter == Adapter::Virtual {
            "Carries the virtual Wi-Fi"
        } else {
            info.net.label()
        };
        let card = if info.net == Net::NoCard {
            "None found"
        } else {
            "Intel PRO/1000 (e1000)"
        };
        let p = config::get().proxy;
        let proxy = match p.kind {
            ProxyKind::Off => String::from("Off"),
            k if p.host.is_empty() => String::from(k.name()),
            k => format!("{} {}:{}", k.name().to_ascii_uppercase(), p.host, p.port),
        };
        self.rows(
            c,
            &[
                ("Wi-Fi", &wifi_status, ""),
                (
                    "Wi-Fi adapter",
                    "The card that talks to the air",
                    &adapter_name,
                ),
                (
                    "Virtual Wi-Fi adapter",
                    "For testing in QEMU: pretend networks, traffic goes over the cable",
                    "",
                ),
                ("Ethernet", "Status", ethernet),
                ("IPv4 address", "Given by DHCP", address),
                ("Network adapter", "The wired card", card),
                ("Proxy", "Used by the browser and Telegram", &proxy),
            ],
        );
        if let Some(st) = &state {
            // Wi-Fi on/off, left of the button
            let b = row_button(0);
            let sw = Rect::new(b.x - 16 - 40, b.y + 6, 40, 20);
            theme::switch(c, sw, *st != State::Off);
        }
        self.draw_button(c, Button::ShowNetworks, "Show networks");
        theme::switch(c, row_switch(2), adapter == Adapter::Virtual);
    }

    /// A click on the Network page's switches and the proxy row.
    fn network_click(&mut self, x: i32, y: i32) -> bool {
        if row_switch(2).contains(x, y) {
            wifi::set_virtual(wifi::adapter() != Adapter::Virtual);
            return true;
        }
        if let Some(st) = wifi::state() {
            let b = row_button(0);
            let sw = Rect::new(b.x - 16 - 40, b.y + 6, 40, 20);
            if sw.inset(-4).contains(x, y) {
                wifi::set_on(st == State::Off);
                return true;
            }
        }
        if row_rect(6).contains(x, y) {
            self.show_page(Page::Proxy);
            return true;
        }
        false
    }
}

// ---- Proxy ----------------------------------------------------------------------------

const FIELDS: usize = 5;
const F_HOST: usize = 0;
const F_PORT: usize = 1;
const F_USER: usize = 2;
const F_PASS: usize = 3;
const F_BYPASS: usize = 4;

const FIELD_LABELS: [&str; FIELDS] = [
    "Proxy IP address or name",
    "Port",
    "User name (optional)",
    "Password",
    "Don't use the proxy for these addresses (use ; between them, * matches anything)",
];

/// Where the proxy check's fiber leaves its answer.
type CheckResult = Rc<RefCell<Option<Result<(), String>>>>;

/// The proxy page: the kind of proxy, where it is, a login, and hosts
/// reached directly. Nothing changes until Save.
struct ProxyForm {
    kind: ProxyKind,
    fields: [TextField; FIELDS],
    focus: Option<usize>,
    /// What the last Save or Check said, and whether it went wrong.
    note: Option<(String, bool)>,
    check: Option<(Fiber, CheckResult)>,
    pressed: Option<ProxyHit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProxyHit {
    Kind(ProxyKind),
    Field(usize),
    Save,
    Check,
}

const KINDS: [(ProxyKind, &str); 3] = [
    (ProxyKind::Off, "Off"),
    (ProxyKind::Http, "HTTP"),
    (ProxyKind::Socks5, "SOCKS5"),
];

fn kind_card() -> Rect {
    Rect::new(PAGE_X, 72, PAGE_W, 64)
}

fn form_card() -> Rect {
    Rect::new(PAGE_X, 144, PAGE_W, 300)
}

fn kind_button(i: usize) -> Rect {
    let r = kind_card();
    let w = 84;
    let total = KINDS.len() as i32 * (w + 6) - 6;
    Rect::new(r.right() - 16 - total + i as i32 * (w + 6), r.y + 16, w, 32)
}

fn field_rect(i: usize) -> Rect {
    let r = form_card();
    let left = r.x + 20;
    let wide = r.w - 40;
    match i {
        F_HOST => Rect::new(left, r.y + 36, wide - 140, 32),
        F_PORT => Rect::new(left + wide - 120, r.y + 36, 120, 32),
        F_USER => Rect::new(left, r.y + 106, (wide - 20) / 2, 32),
        F_PASS => Rect::new(left + (wide + 20) / 2, r.y + 106, (wide - 20) / 2, 32),
        _ => Rect::new(left, r.y + 176, wide, 32),
    }
}

fn save_button() -> Rect {
    let r = form_card();
    Rect::new(r.x + 20, r.bottom() - 20 - 32, 110, 32)
}

fn check_button() -> Rect {
    let s = save_button();
    Rect::new(s.right() + 8, s.y, 150, 32)
}

impl ProxyForm {
    fn load() -> Self {
        let p = config::get().proxy;
        let port = if p.port == 0 {
            String::new()
        } else {
            format!("{}", p.port)
        };
        let mut pass = TextField::password();
        pass.set(&p.pass);
        ProxyForm {
            kind: p.kind,
            fields: [
                TextField::new(&p.host),
                TextField::new(&port),
                TextField::new(&p.user),
                pass,
                TextField::new(&p.bypass),
            ],
            focus: None,
            note: None,
            check: None,
            pressed: None,
        }
    }

    /// The proxy as typed in, or why it can't be one.
    fn proxy(&self) -> Result<Proxy, String> {
        let host = String::from(self.fields[F_HOST].string().trim());
        let port_text = self.fields[F_PORT].string();
        let port: u16 = match port_text.trim() {
            "" if self.kind == ProxyKind::Off => 0,
            t => t
                .parse()
                .ok()
                .filter(|&p| p > 0)
                .ok_or_else(|| String::from("The port is a number from 1 to 65535"))?,
        };
        if self.kind != ProxyKind::Off && host.is_empty() {
            return Err(String::from("Type the proxy's address"));
        }
        Ok(Proxy {
            kind: self.kind,
            host,
            port,
            user: String::from(self.fields[F_USER].string().trim()),
            pass: self.fields[F_PASS].string(),
            bypass: String::from(self.fields[F_BYPASS].string().trim()),
        })
    }

    fn save(&mut self) {
        match self.proxy() {
            Ok(p) => {
                config::update(|c| c.proxy = p);
                crate::web::http::clear_pool();
                self.note = Some((String::from("Saved"), false));
            }
            Err(e) => self.note = Some((e, true)),
        }
    }

    fn start_check(&mut self) {
        if self.check.is_some() {
            return;
        }
        let p = match self.proxy() {
            Ok(p) => p,
            Err(e) => {
                self.note = Some((e, true));
                return;
            }
        };
        let result = Rc::new(RefCell::new(None));
        let out = result.clone();
        let fiber = Fiber::new(move || {
            let r = crate::net::proxy::check(&p, "example.com", 443);
            *out.borrow_mut() = Some(r);
        });
        self.check = Some((fiber, result));
        self.note = Some((String::from("Checking..."), false));
    }

    fn tick(&mut self) -> bool {
        let Some((fiber, result)) = self.check.as_mut() else {
            return false;
        };
        if !fiber.resume() {
            return false;
        }
        let r = result.borrow_mut().take();
        self.check = None;
        self.note = Some(match r {
            Some(Ok(())) => (String::from("Works: reached example.com"), false),
            Some(Err(e)) => (e, true),
            None => (String::from("The check stopped"), true),
        });
        true
    }

    fn hits(&self) -> Vec<(ProxyHit, Rect)> {
        let mut out = Vec::new();
        for (i, &(k, _)) in KINDS.iter().enumerate() {
            out.push((ProxyHit::Kind(k), kind_button(i)));
        }
        for i in 0..FIELDS {
            out.push((ProxyHit::Field(i), field_rect(i)));
        }
        out.push((ProxyHit::Save, save_button()));
        out.push((ProxyHit::Check, check_button()));
        out
    }

    fn click(&mut self, x: i32, y: i32) -> bool {
        let Some((hit, r)) = self.hits().into_iter().find(|(_, r)| r.contains(x, y)) else {
            let changed = self.focus.is_some();
            self.focus = None;
            return changed;
        };
        self.focus = None;
        match hit {
            ProxyHit::Kind(k) => {
                self.kind = k;
                self.note = None;
            }
            ProxyHit::Field(i) => {
                self.focus = Some(i);
                self.fields[i].click(r, x);
            }
            ProxyHit::Save => {
                self.pressed = Some(hit);
                self.save();
            }
            ProxyHit::Check => {
                self.pressed = Some(hit);
                self.start_check();
            }
        }
        true
    }

    fn on_key(&mut self, key: Key) -> bool {
        let Some(i) = self.focus else {
            return false;
        };
        if let Key::Char('\t') = key {
            let back = crate::keyboard::shift_held();
            self.focus = Some(if back {
                (i + FIELDS - 1) % FIELDS
            } else {
                (i + 1) % FIELDS
            });
            let f = &mut self.fields[self.focus.unwrap()];
            f.select_all();
            return true;
        }
        match self.fields[i].on_key(key) {
            FieldEvent::None => false,
            FieldEvent::Changed => true,
            FieldEvent::Enter => {
                self.save();
                true
            }
            FieldEvent::Escape => {
                self.focus = None;
                true
            }
        }
    }

    fn draw(&mut self, c: &mut Canvas, caret: bool) {
        self.pressed = None;
        let r = kind_card();
        card(c, r);
        c.draw_text(r.x + 20, r.y + 12, "Use a proxy server", theme::text());
        c.draw_text(
            r.x + 20,
            r.y + 31,
            "For the browser and Telegram",
            theme::text_dim(),
        );
        for (i, &(k, name)) in KINDS.iter().enumerate() {
            theme::toggle_button(c, kind_button(i), name, self.kind == k);
        }

        let r = form_card();
        card(c, r);
        let off = self.kind == ProxyKind::Off;
        for (i, (field, label)) in self.fields.iter_mut().zip(FIELD_LABELS).enumerate() {
            let f = field_rect(i);
            let ink = if off {
                theme::text_dim()
            } else {
                theme::text()
            };
            c.draw_text(f.x, f.y - 22, label, ink);
            field.draw(c, f, self.focus == Some(i), caret);
        }
        theme::accent_button(c, save_button(), "Save", false);
        let checking = self.check.is_some();
        theme::button(
            c,
            check_button(),
            if checking {
                "Checking..."
            } else {
                "Check proxy"
            },
            checking,
        );
        if let Some((note, bad)) = &self.note {
            let b = check_button();
            let color = if *bad {
                theme::error()
            } else {
                theme::text_dim()
            };
            let mut text = note.clone();
            let room = r.right() - 20 - (b.right() + 16);
            while UI.width(&text) > room && text.pop().is_some() {}
            c.draw_text(
                b.right() + 16,
                b.y + (b.h - UI.line_height) / 2,
                &text,
                color,
            );
        }

        let r = Rect::new(PAGE_X, 452, PAGE_W, 110);
        card(c, r);
        let lines = [
            "HTTP: pages over https:// and Telegram go through a CONNECT tunnel,",
            "plain http:// pages are sent to the proxy as whole addresses.",
            "SOCKS5: everything goes through the proxy; it looks the names up.",
            "Check proxy asks the proxy to open a connection to example.com.",
        ];
        for (i, line) in lines.iter().enumerate() {
            c.draw_text(r.x + 20, r.y + 12 + i as i32 * 22, line, theme::text_dim());
        }
    }
}
