//! Settings, like the Windows 11 app: pages in a list on the left and
//! cards of settings on the right. Most rows show how the system is set
//! up; the keyboard layout can be switched here, and the About page
//! opens "About EverOS".

use alloc::format;
use alloc::string::String;

use super::canvas::{rgb, Canvas, Rect};
use super::icons::{self, Pic, SMALL};
use super::text::{TITLE, UI, UI_BOLD};
use super::tray::{self, Net};
use super::{theme, App, MouseEvent, MouseKind};
use crate::fs;
use crate::keyboard::Layout;

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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    System,
    Network,
    Time,
    Accounts,
    About,
}

const PAGES: [(Page, &str); 5] = [
    (Page::System, "System"),
    (Page::Network, "Network & internet"),
    (Page::Time, "Time & language"),
    (Page::Accounts, "Accounts"),
    (Page::About, "About"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    SwitchLayout,
    OpenAbout,
}

pub struct Settings {
    page: Page,
    pressed: Option<Button>,
    /// Asks the desktop to switch the keyboard layout.
    pub switch_layout: bool,
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
        }
    }

    /// Where the page's button is, if it has one.
    fn button(&self) -> Option<(Button, Rect)> {
        match self.page {
            Page::Time => Some((Button::SwitchLayout, row_button(1))),
            Page::About => Some((Button::OpenAbout, row_button(4))),
            _ => None,
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } => {
                if let Some(i) = (0..PAGES.len()).find(|&i| nav_rect(i).contains(ev.x, ev.y)) {
                    let changed = self.page != PAGES[i].0;
                    self.page = PAGES[i].0;
                    return changed;
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
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas, info: &Info) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, rgb(0xf3, 0xf3, 0xf3));
        self.draw_nav(c, info);
        let title = PAGES.iter().find(|p| p.0 == self.page).map_or("", |p| p.1);
        c.draw_text_in(&TITLE, PAGE_X, 26, title, theme::TEXT);

        let user = crate::users::current_name();
        let user = user.as_ref().map_or("nobody", |n| n.as_str());
        match self.page {
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
            Page::Network => {
                let address = if info.address.is_empty() {
                    "-"
                } else {
                    info.address
                };
                let adapter = if info.net == Net::NoCard {
                    "None found"
                } else {
                    "Intel PRO/1000 (e1000)"
                };
                self.rows(
                    c,
                    &[
                        ("Ethernet", "Status", info.net.label()),
                        ("IPv4 address", "Given by DHCP", address),
                        ("Network adapter", "The card EverOS talks to", adapter),
                    ],
                );
            }
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
                    c.draw_text(r.x + 60, r.y + 10, name, theme::TEXT);
                    let kind = if name == user {
                        "Signed in, local account"
                    } else {
                        "Local account"
                    };
                    c.draw_text(r.x + 60, r.y + 29, kind, theme::TEXT_DIM);
                    let w = UI.width(password);
                    c.draw_text(r.right() - 20 - w, r.y + 19, password, theme::TEXT_DIM);
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
        c.draw_text_in(&UI_BOLD, 88, 34, user, theme::TEXT);
        c.draw_text(88, 54, "Local account", theme::TEXT_DIM);

        for (i, &(page, label)) in PAGES.iter().enumerate() {
            let r = nav_rect(i);
            if page == self.page {
                c.fill_round(r, 5, rgb(0xe6, 0xe8, 0xee));
                c.fill_round(Rect::new(r.x, r.y + 10, 3, r.h - 20), 1, theme::ACCENT);
            }
            let (x, y) = (r.x + 14, r.y + (r.h - 16) / 2);
            let pics = icons::get();
            match page {
                Page::System => pics.draw_pic(c, Pic::Computer, SMALL, x, y),
                Page::Network => {
                    let bg = if page == self.page {
                        rgb(0xe6, 0xe8, 0xee)
                    } else {
                        rgb(0xf3, 0xf3, 0xf3)
                    };
                    tray::network_icon(c, x, y + 1, info.net, theme::TEXT, bg);
                }
                Page::Time => {
                    c.outline_round(Rect::new(x, y, 16, 16), 8, theme::TEXT);
                    c.fill_rect(x + 8, y + 3, 1, 6, theme::TEXT);
                    c.fill_rect(x + 8, y + 8, 4, 1, theme::TEXT);
                }
                Page::Accounts => {
                    c.fill_round(Rect::new(x + 4, y, 8, 8), 4, theme::ACCENT);
                    c.fill_round(Rect::new(x + 1, y + 9, 14, 7), 3, theme::ACCENT);
                }
                Page::About => pics.draw_small(c, App::About, x, y),
            }
            c.draw_text(
                r.x + 42,
                r.y + (r.h - UI.line_height) / 2,
                label,
                theme::TEXT,
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
                    theme::TEXT,
                );
            } else {
                c.draw_text(r.x + 20, r.y + 10, title, theme::TEXT);
                c.draw_text(r.x + 20, r.y + 29, note, theme::TEXT_DIM);
            }
            // values go left of a button in the same row
            let right = match self.button() {
                Some((_, b)) if r.contains(b.x, b.y) => b.x - 16,
                _ => r.right() - 20,
            };
            let w = UI.width(value);
            let y = r.y + (ROW_H - UI.line_height) / 2;
            c.draw_text(right - w, y, value, theme::TEXT_DIM);
        }
    }

    fn draw_button(&self, c: &mut Canvas, b: Button, label: &str) {
        if let Some((_, r)) = self.button() {
            theme::button(c, r, label, self.pressed == Some(b));
        }
    }
}

fn card(c: &mut Canvas, r: Rect) {
    c.fill_round(r, 6, theme::LIGHT);
    c.outline_round(r, 6, rgb(0xe5, 0xe5, 0xe5));
}

/// A round picture with the first letter of the name.
fn avatar(c: &mut Canvas, x: i32, y: i32, size: i32, name: &str) {
    let r = Rect::new(x, y, size, size);
    c.fill_round(r, size / 2, rgb(0x3a, 0x7c, 0xd0));
    let mut first = String::new();
    first.extend(name.chars().next().map(|ch| ch.to_ascii_uppercase()));
    let font = if size >= 48 { &TITLE } else { &UI_BOLD };
    c.text_centered_in(font, r, &first, theme::LIGHT);
}
