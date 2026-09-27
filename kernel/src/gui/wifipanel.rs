//! The Wi-Fi list in quick settings, as in Windows 11: a switch to turn
//! Wi-Fi on or off, the networks in range with their signal, and under
//! the chosen one a password box or a Connect / Disconnect button.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, Canvas, Rect};
use super::text::{UI, UI_BOLD};
use super::theme;
use super::tray::wifi_icon;
use super::widgets::{FieldEvent, TextField};
use crate::keyboard::Key;
use crate::net::wifi::{self, Network, Security, State};

pub const HEIGHT: i32 = 488;
const HEAD: i32 = 56;
const ROW: i32 = 52;
const FOOT: i32 = 52;
const BUTTON_W: i32 = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Back,
    Switch,
    Row(usize),
    Field,
    Connect,
    Cancel,
    Disconnect,
    Forget,
    More,
}

/// What the desktop should do after a click or a key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    /// Back to quick settings.
    Back,
    /// Open Settings > Network & internet.
    OpenSettings,
}

pub struct WifiPanel {
    nets: Vec<Network>,
    state: Option<State>,
    selected: Option<String>,
    field: RefCell<TextField>,
    /// Why joining failed, for the chosen network.
    error: Option<String>,
}

/// What shows under the chosen network.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Joined,
    Joining,
    Password,
    Join,
}

impl WifiPanel {
    pub fn new() -> Self {
        WifiPanel {
            nets: Vec::new(),
            state: None,
            selected: None,
            field: RefCell::new(TextField::password()),
            error: None,
        }
    }

    /// Read the networks and the state again. Returns whether anything
    /// shown changed.
    pub fn refresh(&mut self) -> bool {
        let state = wifi::state();
        let nets = wifi::networks();
        let failed = wifi::take_failure();
        if let Some((ssid, why)) = &failed {
            self.selected = Some(ssid.clone());
            self.error = Some(why.clone());
            self.field.get_mut().set("");
        }
        let same = failed.is_none()
            && self.state == state
            && nets.len() == self.nets.len()
            && nets
                .iter()
                .zip(&self.nets)
                .all(|(a, b)| a.ssid == b.ssid && a.bars() == b.bars() && a.saved == b.saved);
        self.state = state;
        self.nets = nets;
        !same
    }

    /// The panel just opened: select the joined network.
    pub fn opened(&mut self) {
        self.error = None;
        self.selected = match wifi::state() {
            Some(State::Connected { ssid, .. } | State::Connecting(ssid)) => Some(ssid),
            _ => None,
        };
        // a join that failed meanwhile shows under its network
        self.refresh();
    }

    /// Whether keys go to the password box.
    pub fn typing(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|s| self.nets.iter().find(|n| n.ssid == *s))
            .is_some_and(|n| self.detail(n) == Detail::Password)
    }

    fn on(&self) -> bool {
        !matches!(self.state, None | Some(State::Off))
    }

    fn detail(&self, n: &Network) -> Detail {
        match &self.state {
            Some(State::Connected { ssid, .. }) if *ssid == n.ssid => Detail::Joined,
            Some(State::Connecting(ssid)) if *ssid == n.ssid => Detail::Joining,
            _ if n.security == Security::Wpa2 && (!n.saved || self.error.is_some()) => {
                Detail::Password
            }
            _ => Detail::Join,
        }
    }

    fn detail_height(d: Detail) -> i32 {
        match d {
            Detail::Joined | Detail::Join => 48,
            Detail::Joining => 32,
            Detail::Password => 136,
        }
    }

    fn switch_rect(p: Rect) -> Rect {
        Rect::new(p.right() - 20 - 40, p.y + 18, 40, 20)
    }

    fn footer(p: Rect) -> Rect {
        Rect::new(p.x, p.bottom() - FOOT, p.w, FOOT)
    }

    /// Everything clickable, and the rows with their details.
    fn layout(&self, p: Rect) -> Vec<(Hit, Rect)> {
        let mut out = Vec::new();
        out.push((Hit::Back, Rect::new(p.x + 12, p.y + 14, 32, 28)));
        out.push((Hit::Switch, Self::switch_rect(p)));
        out.push((Hit::More, Self::footer(p)));
        if !self.on() {
            return out;
        }
        let bottom = p.bottom() - FOOT - 8;
        let mut y = p.y + HEAD + 4;
        for (i, n) in self.nets.iter().enumerate() {
            let chosen = self.selected.as_deref() == Some(n.ssid.as_str());
            let extra = if chosen {
                Self::detail_height(self.detail(n))
            } else {
                0
            };
            if y + ROW + extra > bottom {
                break;
            }
            let row = Rect::new(p.x + 8, y, p.w - 16, ROW + extra);
            out.push((Hit::Row(i), row));
            if chosen {
                let right = row.right() - 12;
                let by = row.bottom() - 12 - 32;
                match self.detail(n) {
                    Detail::Joined => {
                        out.push((
                            Hit::Disconnect,
                            Rect::new(right - BUTTON_W, by, BUTTON_W, 32),
                        ));
                        if n.saved {
                            out.push((
                                Hit::Forget,
                                Rect::new(right - 2 * BUTTON_W - 8, by, BUTTON_W, 32),
                            ));
                        }
                    }
                    Detail::Join => {
                        out.push((Hit::Connect, Rect::new(right - BUTTON_W, by, BUTTON_W, 32)));
                        if n.saved {
                            out.push((
                                Hit::Forget,
                                Rect::new(right - 2 * BUTTON_W - 8, by, BUTTON_W, 32),
                            ));
                        }
                    }
                    Detail::Password => {
                        out.push((
                            Hit::Field,
                            Rect::new(row.x + 44, row.y + ROW + 22, row.w - 56, 32),
                        ));
                        out.push((Hit::Cancel, Rect::new(right - BUTTON_W, by, BUTTON_W, 32)));
                        out.push((
                            Hit::Connect,
                            Rect::new(right - 2 * BUTTON_W - 8, by, BUTTON_W, 32),
                        ));
                    }
                    Detail::Joining => {}
                }
            }
            y += ROW + extra + 4;
        }
        out
    }

    fn chosen(&self) -> Option<Network> {
        let s = self.selected.as_ref()?;
        self.nets.iter().find(|n| n.ssid == *s).cloned()
    }

    fn join(&mut self) -> Action {
        let Some(n) = self.chosen() else {
            return Action::None;
        };
        let password = self.field.get_mut().string();
        let pass = (self.detail(&n) == Detail::Password).then_some(password.as_str());
        match wifi::connect(&n.ssid, pass) {
            Ok(()) => {
                self.error = None;
                self.field.get_mut().set("");
            }
            Err(e) => self.error = Some(e),
        }
        self.refresh();
        Action::Redraw
    }

    pub fn click(&mut self, p: Rect, x: i32, y: i32) -> Action {
        // the most specific target: buttons and the box sit inside rows
        let Some((hit, r)) = self
            .layout(p)
            .into_iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
        else {
            return Action::None;
        };
        match hit {
            Hit::Back => Action::Back,
            Hit::More => Action::OpenSettings,
            Hit::Switch => {
                wifi::set_on(!self.on());
                self.selected = None;
                self.error = None;
                self.refresh();
                Action::Redraw
            }
            Hit::Row(i) => {
                let ssid = self.nets[i].ssid.clone();
                if self.selected.as_ref() != Some(&ssid) {
                    self.selected = Some(ssid);
                    self.error = None;
                    self.field.get_mut().set("");
                }
                Action::Redraw
            }
            Hit::Field => {
                self.field.get_mut().click(r, x);
                Action::Redraw
            }
            Hit::Connect => self.join(),
            Hit::Cancel => {
                self.selected = None;
                self.error = None;
                self.field.get_mut().set("");
                Action::Redraw
            }
            Hit::Disconnect => {
                wifi::disconnect();
                self.refresh();
                Action::Redraw
            }
            Hit::Forget => {
                if let Some(n) = self.chosen() {
                    wifi::forget(&n.ssid);
                }
                self.error = None;
                self.refresh();
                Action::Redraw
            }
        }
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        if !self.typing() {
            return Action::None;
        }
        match self.field.get_mut().on_key(key) {
            FieldEvent::None => Action::None,
            FieldEvent::Changed => Action::Redraw,
            FieldEvent::Enter => self.join(),
            FieldEvent::Escape => {
                self.selected = None;
                Action::Redraw
            }
        }
    }

    pub fn draw(&self, c: &mut Canvas, p: Rect, caret: bool) {
        c.fill(p, theme::panel());
        // the header: back arrow, title and the switch
        let (ax, ay) = (p.x + 22, p.y + 28);
        c.line(ax + 6, ay - 6, ax, ay, theme::text());
        c.line(ax, ay, ax + 6, ay + 6, theme::text());
        c.line(ax, ay, ax + 12, ay, theme::text());
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 19, "Wi-Fi", theme::text());
        theme::switch(c, Self::switch_rect(p), self.on());
        c.fill_rect(p.x, p.y + HEAD - 1, p.w, 1, theme::stroke());

        let layout = self.layout(p);
        let rect = |h: Hit| layout.iter().find(|(k, _)| *k == h).map(|(_, r)| *r);
        if !self.on() {
            let note = if self.state.is_none() {
                "No Wi-Fi adapter"
            } else {
                "Wi-Fi is off"
            };
            c.text_centered(
                Rect::new(p.x, p.y + HEAD + 40, p.w, 24),
                note,
                theme::text_dim(),
            );
        } else if self.nets.is_empty() {
            c.text_centered(
                Rect::new(p.x, p.y + HEAD + 40, p.w, 24),
                "Looking for networks...",
                theme::text_dim(),
            );
        }
        for (i, n) in self.nets.iter().enumerate() {
            let Some(row) = rect(Hit::Row(i)) else {
                break;
            };
            let chosen = self.selected.as_deref() == Some(n.ssid.as_str());
            if chosen {
                c.fill_round(row, 6, theme::hover());
            }
            let (ix, iy) = (row.x + 14, row.y + (ROW - 16) / 2);
            let bg = if chosen {
                theme::hover()
            } else {
                theme::panel()
            };
            wifi_icon(c, ix, iy, n.bars(), theme::text(), bg);
            if n.security == Security::Wpa2 {
                lock_icon(c, ix + 10, iy + 9, theme::text(), bg);
            }
            let detail = self.detail(n);
            let font = if detail == Detail::Joined {
                &UI_BOLD
            } else {
                &UI
            };
            c.draw_text_in(font, row.x + 44, row.y + 8, &n.ssid, theme::text());
            let sub = match (detail, n.security) {
                (Detail::Joined, Security::Wpa2) => "Connected, secured",
                (Detail::Joined, Security::Open) => "Connected, open",
                (Detail::Joining, _) => "Connecting...",
                (_, Security::Wpa2) if n.saved => "Secured, saved",
                (_, Security::Wpa2) => "Secured",
                (_, Security::Open) => "Open",
            };
            c.draw_text(row.x + 44, row.y + 27, sub, theme::text_dim());
            if !chosen {
                continue;
            }
            if detail == Detail::Password {
                c.draw_text(
                    row.x + 44,
                    row.y + ROW,
                    "Enter the network security key",
                    theme::text(),
                );
                if let Some(f) = rect(Hit::Field) {
                    self.field.borrow_mut().draw(c, f, true, caret);
                }
            }
            if detail == Detail::Joining {
                c.draw_text(
                    row.x + 44,
                    row.y + ROW + 4,
                    "Checking the password...",
                    theme::text_dim(),
                );
            }
            if let Some(e) = &self.error {
                if detail != Detail::Joining {
                    let y = if detail == Detail::Password {
                        row.y + ROW + 58
                    } else {
                        row.y + ROW - 2
                    };
                    c.draw_text(row.x + 44, y, e, theme::error());
                }
            }
            for (h, label, primary) in [
                (
                    Hit::Connect,
                    if detail == Detail::Password {
                        "Next"
                    } else {
                        "Connect"
                    },
                    true,
                ),
                (Hit::Disconnect, "Disconnect", false),
                (Hit::Cancel, "Cancel", false),
                (Hit::Forget, "Forget", false),
            ] {
                if let Some(b) = rect(h) {
                    if primary {
                        theme::accent_button(c, b, label, false);
                    } else {
                        theme::button(c, b, label, false);
                    }
                }
            }
        }

        let f = Self::footer(p);
        c.fill(f, theme::footer());
        c.fill_rect(f.x, f.y, f.w, 1, theme::stroke());
        c.draw_text(
            f.x + 24,
            f.y + (f.h - UI.line_height) / 2,
            "More Wi-Fi settings",
            theme::text(),
        );
    }
}

/// A small padlock with its bottom right corner near (x + 7, y + 7).
fn lock_icon(c: &mut Canvas, x: i32, y: i32, ink: u32, bg: u32) {
    c.fill_rect(x - 1, y - 1, 10, 10, bg);
    c.outline_round(Rect::new(x + 2, y, 4, 6), 2, ink);
    c.fill_rect(x + 1, y + 3, 6, 5, ink);
    c.fill_rect(x + 3, y + 5, 2, 1, mix(ink, bg, 150));
}
