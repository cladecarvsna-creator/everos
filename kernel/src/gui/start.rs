//! The start menu, in the style of Windows 11: a search box, pinned apps,
//! recently opened apps, an "All apps" list and a power button.
//!
//! Typing while the menu is open searches the apps; Enter starts the
//! best match.

use super::canvas::{mix, rgb, Canvas, Rect};
use super::icons::{draw_icon, Icons};
use super::text::{UI, UI_BOLD};
use super::theme;
use super::{App, APPS};
use crate::keyboard::Key;
use crate::StackString;

pub const W: i32 = 640;
pub const H: i32 = 540;
const RADIUS: i32 = 8;
const FOOTER: i32 = 64;
const MAX_RECENT: usize = 4;
const MAX_TARGETS: usize = 12;

/// What the desktop should do after an event.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// The menu changed and must be drawn again.
    Redraw,
    Open(App),
    Close,
    Restart,
    ShutDown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Pinned,
    AllApps,
}

/// Something in the menu that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    App(App),
    AllApps,
    Back,
    Power,
    Restart,
    ShutDown,
}

pub struct StartMenu {
    pub open: bool,
    view: View,
    search: StackString<32>,
    power_open: bool,
    /// Most recently opened first.
    recent: [Option<App>; MAX_RECENT],
    hover: Option<Target>,
}

/// Apps in alphabetical order, for "All apps".
const SORTED: [App; 5] = [
    App::Browser,
    App::Calculator,
    App::Demo,
    App::Paint,
    App::Terminal,
];

impl StartMenu {
    pub fn new() -> Self {
        Self {
            open: false,
            view: View::Pinned,
            search: StackString::new(),
            power_open: false,
            recent: [None; MAX_RECENT],
            hover: None,
        }
    }

    /// Where the menu panel sits on a screen of the given size.
    pub fn panel(width: i32, height: i32, taskbar: i32) -> Rect {
        Rect::new((width - W) / 2, height - taskbar - 12 - H, W, H)
    }

    pub fn show(&mut self) {
        self.open = true;
        self.view = View::Pinned;
        self.search.clear();
        self.power_open = false;
        self.hover = None;
    }

    /// Remember an app for "Recommended".
    pub fn note_opened(&mut self, app: App) {
        let old = self.recent;
        self.recent[0] = Some(app);
        let mut n = 1;
        for a in old.into_iter().flatten() {
            if a != app && n < MAX_RECENT {
                self.recent[n] = Some(a);
                n += 1;
            }
        }
    }

    fn matches(&self) -> impl Iterator<Item = App> + '_ {
        SORTED
            .into_iter()
            .filter(|a| contains_ignore_case(a.title(), self.search.as_str()))
    }

    // ---- layout ------------------------------------------------------------

    fn search_box(p: Rect) -> Rect {
        Rect::new(p.x + 32, p.y + 24, p.w - 64, 36)
    }

    fn footer(p: Rect) -> Rect {
        Rect::new(p.x, p.bottom() - FOOTER, p.w, FOOTER)
    }

    fn power_flyout(p: Rect) -> Rect {
        Rect::new(p.right() - 196, p.bottom() - FOOTER - 92, 180, 88)
    }

    /// Everything clickable, and where it is.
    fn targets(&self, p: Rect) -> ([(Target, Rect); MAX_TARGETS], usize) {
        let mut out = [(Target::Back, Rect::default()); MAX_TARGETS];
        let mut n = 0;
        let mut push = |t: Target, r: Rect| {
            if n < MAX_TARGETS {
                out[n] = (t, r);
                n += 1;
            }
        };
        if self.power_open {
            let f = Self::power_flyout(p);
            push(Target::Restart, Rect::new(f.x + 4, f.y + 4, f.w - 8, 38));
            push(Target::ShutDown, Rect::new(f.x + 4, f.y + 46, f.w - 8, 38));
        }
        let footer = Self::footer(p);
        push(
            Target::Power,
            Rect::new(footer.right() - 64, footer.y + 12, 40, 40),
        );

        if !self.search.as_str().is_empty() {
            for (i, app) in self.matches().enumerate() {
                push(
                    Target::App(app),
                    Rect::new(p.x + 32, p.y + 112 + i as i32 * 52, p.w - 64, 48),
                );
            }
            return (out, n);
        }
        match self.view {
            View::Pinned => {
                push(
                    Target::AllApps,
                    Rect::new(p.right() - 144, p.y + 80, 112, 28),
                );
                for (i, app) in APPS.into_iter().enumerate() {
                    push(
                        Target::App(app),
                        Rect::new(p.x + 32 + i as i32 * 96, p.y + 120, 96, 88),
                    );
                }
                for (i, app) in self.recent.into_iter().flatten().enumerate() {
                    let (col, row) = ((i % 2) as i32, (i / 2) as i32);
                    let w = (p.w - 64) / 2;
                    push(
                        Target::App(app),
                        Rect::new(p.x + 32 + col * w, p.y + 268 + row * 56, w - 8, 52),
                    );
                }
            }
            View::AllApps => {
                push(Target::Back, Rect::new(p.right() - 120, p.y + 80, 88, 28));
                for (i, app) in SORTED.into_iter().enumerate() {
                    push(
                        Target::App(app),
                        Rect::new(p.x + 32, p.y + 120 + i as i32 * 48, p.w - 64, 44),
                    );
                }
            }
        }
        (out, n)
    }

    fn target_at(&self, p: Rect, x: i32, y: i32) -> Option<Target> {
        let (targets, n) = self.targets(p);
        targets[..n]
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|&(t, _)| t)
    }

    // ---- input -------------------------------------------------------------

    /// Update the hover highlight. Returns whether it changed.
    pub fn set_hover(&mut self, p: Rect, x: i32, y: i32) -> bool {
        let hover = self.target_at(p, x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    pub fn on_click(&mut self, p: Rect, x: i32, y: i32) -> Action {
        let target = self.target_at(p, x, y);
        if self.power_open && !matches!(target, Some(Target::Restart | Target::ShutDown)) {
            self.power_open = false;
            return Action::Redraw;
        }
        match target {
            Some(Target::App(app)) => Action::Open(app),
            Some(Target::AllApps) => {
                self.view = View::AllApps;
                Action::Redraw
            }
            Some(Target::Back) => {
                self.view = View::Pinned;
                Action::Redraw
            }
            Some(Target::Power) => {
                self.power_open = true;
                Action::Redraw
            }
            Some(Target::Restart) => Action::Restart,
            Some(Target::ShutDown) => Action::ShutDown,
            None => Action::None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Escape | Key::Super => {
                if self.power_open {
                    self.power_open = false;
                    Action::Redraw
                } else {
                    Action::Close
                }
            }
            Key::Enter => match self.matches().next() {
                Some(app) if !self.search.as_str().is_empty() => Action::Open(app),
                _ => Action::None,
            },
            Key::Backspace => {
                self.search.pop();
                Action::Redraw
            }
            Key::Char(c) if !c.is_control() && self.search.len() + c.len_utf8() < 32 => {
                let mut buf = [0u8; 4];
                self.search.push_str(c.encode_utf8(&mut buf));
                self.hover = None;
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, p: Rect, icons: &Icons, blink: bool) {
        c.shadow(p, RADIUS, 16, 4, 120);
        {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, RADIUS);
            m.fill_round_alpha(p, 0, rgb(0xf5, 0xf6, 0xfa), 250);
            self.draw_search(&mut m, p, blink);
            if !self.search.as_str().is_empty() {
                self.draw_results(&mut m, p, icons);
            } else {
                match self.view {
                    View::Pinned => self.draw_pinned(&mut m, p, icons),
                    View::AllApps => self.draw_all_apps(&mut m, p, icons),
                }
            }
            self.draw_footer(&mut m, p);
        }
        c.outline_round(p, RADIUS, rgb(0xc8, 0xca, 0xd2));
    }

    fn highlight(&self, c: &mut Canvas, target: Target, r: Rect) {
        if self.hover == Some(target) {
            c.fill_round(r, 6, 0xffffff);
            c.outline_round(r, 6, theme::STROKE);
        }
    }

    fn draw_search(&self, c: &mut Canvas, p: Rect, blink: bool) {
        let r = Self::search_box(p);
        c.fill_round(r, r.h / 2, 0xffffff);
        c.outline_round(r, r.h / 2, rgb(0xd0, 0xd2, 0xda));
        c.fill_rect(r.x + 18, r.bottom() - 1, r.w - 36, 1, theme::ACCENT);
        // magnifying glass
        let lens = Rect::new(r.x + 16, r.y + 10, 12, 12);
        c.outline_round(lens, 6, theme::TEXT);
        c.outline_round(lens.inset(1), 5, theme::TEXT);
        for i in 0..2 {
            c.line(r.x + 26 + i, r.y + 21, r.x + 30 + i, r.y + 25, theme::TEXT);
        }
        let ty = r.y + (r.h - UI.line_height) / 2;
        if self.search.as_str().is_empty() {
            c.draw_text(r.x + 40, ty, "Type here to search apps", theme::TEXT_DIM);
            if blink {
                c.fill_rect(r.x + 40, ty, 1, UI.line_height, theme::TEXT);
            }
        } else {
            let w = c.draw_text(r.x + 40, ty, self.search.as_str(), theme::TEXT);
            if blink {
                c.fill_rect(r.x + 41 + w, ty, 1, UI.line_height, theme::TEXT);
            }
        }
    }

    fn draw_results(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 40, p.y + 84, "Best match", theme::TEXT);
        let (targets, n) = self.targets(p);
        let mut first = true;
        for &(t, r) in &targets[..n] {
            let Target::App(app) = t else {
                continue;
            };
            if first {
                // Enter opens this one
                c.fill_round(r, 6, theme::ACCENT_LIGHT);
                first = false;
            }
            self.highlight(c, t, r);
            icons.draw_medium(c, app, r.x + 12, r.y + 12);
            c.draw_text(r.x + 48, r.y + 6, app.title(), theme::TEXT);
            c.draw_text(r.x + 48, r.y + 25, "App", theme::TEXT_DIM);
        }
        if first {
            let text = "No apps match your search";
            c.draw_text(p.x + 40, p.y + 120, text, theme::TEXT_DIM);
        }
    }

    fn draw_pinned(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 86, "Pinned", theme::TEXT);
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 240, "Recommended", theme::TEXT);
        let (targets, n) = self.targets(p);
        let mut recent = 0;
        for &(t, r) in &targets[..n] {
            match t {
                Target::AllApps => {
                    let face = if self.hover == Some(t) {
                        0xffffff
                    } else {
                        rgb(0xfb, 0xfb, 0xfd)
                    };
                    c.fill_round(r, 4, face);
                    c.outline_round(r, 4, theme::STROKE);
                    c.text_centered(r, "All apps  ›", theme::TEXT);
                }
                // pinned apps are in the grid, recent ones below it
                Target::App(app) if r.y < p.y + 240 => {
                    self.highlight(c, t, r);
                    draw_icon(c, app, r.x + 24, r.y + 8);
                    c.text_centered(Rect::new(r.x, r.y + 60, r.w, 20), app.title(), theme::TEXT);
                }
                Target::App(app) => {
                    recent += 1;
                    self.highlight(c, t, r);
                    icons.draw_medium(c, app, r.x + 12, r.y + 14);
                    c.draw_text(r.x + 48, r.y + 7, app.title(), theme::TEXT);
                    c.draw_text(r.x + 48, r.y + 26, "Recently opened", theme::TEXT_DIM);
                }
                _ => {}
            }
        }
        if recent == 0 {
            let text = "Apps you open will show up here.";
            c.draw_text(p.x + 56, p.y + 276, text, theme::TEXT_DIM);
        }
    }

    fn draw_all_apps(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 86, "All apps", theme::TEXT);
        let (targets, n) = self.targets(p);
        for &(t, r) in &targets[..n] {
            match t {
                Target::Back => {
                    let face = if self.hover == Some(t) {
                        0xffffff
                    } else {
                        rgb(0xfb, 0xfb, 0xfd)
                    };
                    c.fill_round(r, 4, face);
                    c.outline_round(r, 4, theme::STROKE);
                    c.text_centered(r, "‹  Back", theme::TEXT);
                }
                Target::App(app) => {
                    self.highlight(c, t, r);
                    icons.draw_medium(c, app, r.x + 12, r.y + 10);
                    c.draw_text(r.x + 48, r.y + 13, app.title(), theme::TEXT);
                }
                _ => {}
            }
        }
    }

    fn draw_footer(&self, c: &mut Canvas, p: Rect) {
        let f = Self::footer(p);
        c.fill(f, rgb(0xec, 0xee, 0xf4));
        c.fill_rect(f.x, f.y, f.w, 1, theme::STROKE);
        let avatar = Rect::new(f.x + 48, f.y + 16, 32, 32);
        c.fill_round(avatar, 16, theme::ACCENT);
        c.text_centered_in(&UI_BOLD, avatar, "E", 0xffffff);
        c.draw_text(f.x + 92, f.y + 23, "EverOS", theme::TEXT);

        let (targets, n) = self.targets(p);
        for &(t, r) in &targets[..n] {
            match t {
                Target::Power => {
                    let bg = if self.hover == Some(t) || self.power_open {
                        c.fill_round(r, 6, 0xffffff);
                        0xffffff
                    } else {
                        rgb(0xec, 0xee, 0xf4)
                    };
                    power_symbol(c, r.x + 20, r.y + 20, bg);
                }
                Target::Restart | Target::ShutDown => {}
                _ => {}
            }
        }

        if self.power_open {
            let fl = Self::power_flyout(p);
            c.shadow(fl, 8, 10, 2, 90);
            c.fill_round(fl, 8, 0xfbfbfd);
            c.outline_round(fl, 8, theme::STROKE);
            for &(t, r) in &targets[..n] {
                let label = match t {
                    Target::Restart => "Restart",
                    Target::ShutDown => "Shut down",
                    _ => continue,
                };
                let bg = if self.hover == Some(t) {
                    c.fill_round(r, 5, mix(theme::ACCENT_LIGHT, 0xffffff, 60));
                    mix(theme::ACCENT_LIGHT, 0xffffff, 60)
                } else {
                    0xfbfbfd
                };
                if t == Target::Restart {
                    restart_symbol(c, r.x + 20, r.y + 19, bg);
                } else {
                    power_symbol(c, r.x + 20, r.y + 19, bg);
                }
                c.draw_text(r.x + 40, r.y + 10, label, theme::TEXT);
            }
        }
    }
}

/// A power symbol centred at (x, y): a ring open at the top and a bar.
fn power_symbol(c: &mut Canvas, x: i32, y: i32, bg: u32) {
    let ring = Rect::new(x - 8, y - 8, 16, 16);
    c.outline_round(ring, 8, theme::TEXT);
    c.outline_round(ring.inset(1), 7, theme::TEXT);
    c.fill_rect(x - 3, y - 9, 6, 7, bg);
    c.fill_rect(x - 1, y - 10, 2, 9, theme::TEXT);
}

/// A circular arrow centred at (x, y).
fn restart_symbol(c: &mut Canvas, x: i32, y: i32, bg: u32) {
    let ring = Rect::new(x - 8, y - 8, 16, 16);
    c.outline_round(ring, 8, theme::TEXT);
    c.outline_round(ring.inset(1), 7, theme::TEXT);
    c.fill_rect(x, y - 9, 8, 7, bg);
    c.fill_round(Rect::new(x + 1, y - 10, 6, 6), 2, theme::TEXT);
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}
