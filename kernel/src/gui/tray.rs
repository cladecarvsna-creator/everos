//! The right end of the taskbar, as in Windows 11: the keyboard layout,
//! the network and volume icons that open quick settings, and the clock
//! with the date that opens a calendar.
//!
//! Quick settings has tiles for the network and the layout, and sliders
//! for brightness and volume. Brightness dims the whole picture in
//! software. There is no sound driver yet, so volume is only a setting.

use core::fmt::Write;

use super::canvas::{mix, Canvas, Color, Rect};
use super::text::{UI, UI_BOLD};
use super::theme;
use crate::keyboard::Layout;
use crate::net::wifi::{self, State as WifiState};
use crate::{net, rtc, StackString};

pub const QUICK_W: i32 = 360;
pub const QUICK_H: i32 = 292;
pub const CALENDAR_W: i32 = 336;
pub const CALENDAR_H: i32 = 376;
pub const MIN_BRIGHTNESS: i32 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Net {
    NoCard,
    NoCable,
    Connecting,
    Online,
    /// On a Wi-Fi network, with 1 to 4 bars.
    Wifi(u8),
    WifiConnecting,
    /// Wi-Fi on, but not on a network.
    WifiNone,
    WifiOff,
}

impl Net {
    pub fn now() -> Self {
        if let Some(state) = wifi::state() {
            return match state {
                WifiState::Off => Net::WifiOff,
                WifiState::Disconnected | WifiState::Failed(..) => Net::WifiNone,
                WifiState::Connecting(_) => Net::WifiConnecting,
                WifiState::Connected { signal, .. } if net::configured() => {
                    Net::Wifi(wifi::bars(signal))
                }
                WifiState::Connected { .. } => Net::WifiConnecting,
            };
        }
        match net::link() {
            None => Net::NoCard,
            Some(false) => Net::NoCable,
            Some(true) if net::configured() => Net::Online,
            Some(true) => Net::Connecting,
        }
    }

    pub fn is_wifi(self) -> bool {
        matches!(
            self,
            Net::Wifi(_) | Net::WifiConnecting | Net::WifiNone | Net::WifiOff
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Net::NoCard => "No network adapter",
            Net::NoCable => "Cable unplugged",
            Net::Connecting | Net::WifiConnecting => "Connecting...",
            Net::Online | Net::Wifi(_) => "Connected",
            Net::WifiNone => "Not connected",
            Net::WifiOff => "Wi-Fi is off",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Quick,
    /// The list of Wi-Fi networks, opened from the Wi-Fi tile.
    Wifi,
    Calendar,
    /// The icons behind the ^ button.
    Hidden,
}

/// Icons behind the ^ button, in a row.
pub const HIDDEN_ICONS: i32 = 3;
const HIDDEN_CELL: i32 = 40;

impl Panel {
    pub fn size(self) -> (i32, i32) {
        match self {
            Panel::Quick => (QUICK_W, QUICK_H),
            Panel::Wifi => (QUICK_W, super::wifipanel::HEIGHT),
            Panel::Calendar => (CALENDAR_W, CALENDAR_H),
            Panel::Hidden => (HIDDEN_ICONS * HIDDEN_CELL + 16, HIDDEN_CELL + 16),
        }
    }
}

/// Where hidden icon `i` is in its flyout `p`.
pub fn hidden_icon_rect(p: Rect, i: i32) -> Rect {
    Rect::new(p.x + 8 + i * HIDDEN_CELL, p.y + 8, HIDDEN_CELL, HIDDEN_CELL)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slider {
    Brightness,
    Volume,
}

/// Something in quick settings that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    NetworkTile,
    /// The arrow on the Wi-Fi tile, which opens the list of networks.
    WifiArrow,
    LayoutTile,
    Slider(Slider),
}

pub struct Tray {
    /// Percent.
    pub volume: i32,
    /// Percent, MIN_BRIGHTNESS to 100.
    pub brightness: i32,
    pub net: Net,
    pub address: StackString<20>,
    /// The Wi-Fi network joined, if any.
    pub ssid: StackString<40>,
    pub wifi: super::wifipanel::WifiPanel,
}

impl Tray {
    pub fn new() -> Self {
        Self {
            volume: 60,
            brightness: 100,
            net: Net::NoCard,
            address: StackString::new(),
            ssid: StackString::new(),
            wifi: super::wifipanel::WifiPanel::new(),
        }
    }

    /// Read the network state. Returns whether it changed.
    pub fn update_net(&mut self) -> bool {
        let net = Net::now();
        let mut address = StackString::<20>::new();
        if matches!(net, Net::Online | Net::Wifi(_)) {
            address.push_str(&net::address().unwrap_or_default());
        }
        let mut ssid = StackString::<40>::new();
        if let Some(WifiState::Connected { ssid: name, .. }) = wifi::state() {
            ssid.push_str(&name);
        }
        let changed = net != self.net
            || address.as_str() != self.address.as_str()
            || ssid.as_str() != self.ssid.as_str();
        self.net = net;
        self.address = address;
        self.ssid = ssid;
        changed
    }

    // ---- quick settings layout, relative to the panel `p` -------------------

    fn tile(p: Rect, i: i32) -> Rect {
        Rect::new(p.x + 24 + i * 108, p.y + 24, 96, 48)
    }

    fn track(p: Rect, s: Slider) -> Rect {
        let y = match s {
            Slider::Brightness => p.y + 136,
            Slider::Volume => p.y + 184,
        };
        Rect::new(p.x + 60, y, p.w - 128, 4)
    }

    fn footer(p: Rect) -> Rect {
        Rect::new(p.x, p.bottom() - 52, p.w, 52)
    }

    /// The right part of the Wi-Fi tile, with the arrow to the list.
    fn arrow(p: Rect) -> Rect {
        let t = Self::tile(p, 0);
        Rect::new(t.right() - 30, t.y, 30, t.h)
    }

    pub fn target_at(&self, p: Rect, x: i32, y: i32) -> Option<Target> {
        if self.net.is_wifi() && Self::arrow(p).contains(x, y) {
            return Some(Target::WifiArrow);
        }
        let targets = [
            (Target::NetworkTile, Self::tile(p, 0)),
            (Target::LayoutTile, Self::tile(p, 1)),
            (
                Target::Slider(Slider::Brightness),
                Self::track(p, Slider::Brightness).inset(-14),
            ),
            (
                Target::Slider(Slider::Volume),
                Self::track(p, Slider::Volume).inset(-14),
            ),
        ];
        targets
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(t, _)| t)
    }

    /// Move a slider to the pointer's x. Returns whether it changed.
    pub fn drag(&mut self, p: Rect, s: Slider, x: i32) -> bool {
        let track = Self::track(p, s);
        let percent = ((x - track.x) * 100 / track.w).clamp(0, 100);
        let value = match s {
            Slider::Brightness => &mut self.brightness,
            Slider::Volume => &mut self.volume,
        };
        let percent = if s == Slider::Brightness {
            percent.max(MIN_BRIGHTNESS)
        } else {
            percent
        };
        let changed = *value != percent;
        *value = percent;
        changed
    }

    // ---- drawing -------------------------------------------------------------

    /// The panel's background, shadowless: the desktop adds the shadow.
    fn panel_face(c: &mut Canvas, p: Rect) {
        c.fill(p, theme::panel());
    }

    pub fn draw_quick(&self, c: &mut Canvas, p: Rect, layout: Layout, hover: Option<Target>) {
        Self::panel_face(c, p);
        let online = matches!(self.net, Net::Online | Net::Wifi(_));
        let wifi = self.net.is_wifi();
        let net_label = if !wifi {
            "Ethernet"
        } else if self.ssid.as_str().is_empty() {
            "Wi-Fi"
        } else {
            self.ssid.as_str()
        };
        let net_on = if wifi {
            self.net != Net::WifiOff
        } else {
            online
        };
        let tiles = [
            (Target::NetworkTile, net_on, net_label),
            (Target::LayoutTile, true, "Keyboard"),
        ];
        for (i, (t, on, label)) in tiles.into_iter().enumerate() {
            let r = Self::tile(p, i as i32);
            let lit = hover == Some(t);
            let (face, ink) = if on {
                let face = if lit {
                    mix(theme::accent(), theme::on_accent(), 40)
                } else {
                    theme::accent()
                };
                (face, theme::on_accent())
            } else {
                let face = if lit {
                    theme::control_lit()
                } else {
                    theme::control()
                };
                (face, theme::text())
            };
            c.fill_round(r, 5, face);
            if !on {
                c.outline_round(r, 5, theme::stroke());
            }
            let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
            match t {
                Target::NetworkTile if wifi => {
                    // the icon on the left part, an arrow on the right one
                    let a = Self::arrow(p);
                    let lit_arrow = hover == Some(Target::WifiArrow);
                    if lit_arrow {
                        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
                        m.clip_round(r, 5);
                        m.fill(a, mix(face, ink, 30));
                    }
                    c.fill_rect(a.x, a.y + 8, 1, a.h - 16, mix(face, ink, 60));
                    let (ax, ay) = (a.x + a.w / 2 - 2, a.y + a.h / 2);
                    c.line(ax, ay - 4, ax + 4, ay, ink);
                    c.line(ax + 4, ay, ax, ay + 4, ink);
                    network_icon(c, r.x + (r.w - 30) / 2 - 8, cy - 8, self.net, ink, face);
                }
                Target::NetworkTile => network_icon(c, cx - 8, cy - 8, self.net, ink, face),
                _ => c.text_centered_in(&UI_BOLD, r, layout_label(layout), ink),
            }
            let mut label = alloc::string::String::from(label);
            if UI.width(&label) > r.w + 12 {
                while UI.width(&label) > r.w && label.pop().is_some() {}
                label.push('…');
            }
            c.text_centered(
                Rect::new(r.x - 6, r.bottom() + 4, r.w + 12, 20),
                &label,
                theme::text(),
            );
        }

        for s in [Slider::Brightness, Slider::Volume] {
            let track = Self::track(p, s);
            let percent = match s {
                Slider::Brightness => self.brightness,
                Slider::Volume => self.volume,
            };
            let icon_y = track.y + 2 - 8;
            match s {
                Slider::Brightness => sun_icon(c, p.x + 24, icon_y, theme::text()),
                Slider::Volume => volume_icon(c, p.x + 24, icon_y, self.volume, theme::text()),
            }
            c.fill_round(track, 2, theme::thumb());
            let filled = percent * track.w / 100;
            c.fill_round(
                Rect::new(track.x, track.y, filled, track.h),
                2,
                theme::accent(),
            );
            // the thumb: white ring, accent dot that grows under the mouse
            let (tx, ty) = (track.x + filled, track.y + track.h / 2);
            let knob = Rect::new(tx - 10, ty - 10, 20, 20);
            c.fill_round(knob, 10, theme::control_lit());
            c.outline_round(knob, 10, theme::stroke());
            let dot = if hover == Some(Target::Slider(s)) {
                6
            } else {
                5
            };
            c.fill_round(
                Rect::new(tx - dot, ty - dot, 2 * dot, 2 * dot),
                dot,
                theme::accent(),
            );
            let mut text = StackString::<8>::new();
            let _ = write!(text, "{}%", percent);
            c.text_centered(
                Rect::new(track.right() + 14, ty - 10, 44, 20),
                text.as_str(),
                theme::text(),
            );
        }

        let f = Self::footer(p);
        c.fill(f, theme::footer());
        c.fill_rect(f.x, f.y, f.w, 1, theme::stroke());
        network_icon(
            c,
            f.x + 24,
            f.y + 18,
            self.net,
            theme::text(),
            theme::footer(),
        );
        let mut status = StackString::<80>::new();
        if self.ssid.as_str().is_empty() {
            status.push_str(self.net.label());
        } else {
            status.push_str(self.ssid.as_str());
        }
        if !self.address.as_str().is_empty() {
            let _ = write!(status, " - {}", self.address.as_str());
        }
        c.draw_text(
            f.x + 52,
            f.y + (f.h - UI.line_height) / 2,
            status.as_str(),
            theme::text(),
        );
    }

    pub fn draw_calendar(c: &mut Canvas, p: Rect) {
        Self::panel_face(c, p);
        let (year, month, day) = rtc::date();
        let weekday = rtc::weekday(year, month, day) as usize;
        let mut text = StackString::<48>::new();
        let _ = write!(
            text,
            "{}, {} {}",
            DAYS[weekday],
            MONTHS[month as usize - 1],
            day
        );
        c.draw_text_in(&UI_BOLD, p.x + 24, p.y + 20, text.as_str(), theme::text());
        c.fill_rect(p.x, p.y + 56, p.w, 1, theme::stroke());
        text.clear();
        let _ = write!(text, "{} {}", MONTHS[month as usize - 1], year);
        c.draw_text_in(&UI_BOLD, p.x + 24, p.y + 72, text.as_str(), theme::text());

        let cell = 40;
        let x0 = p.x + (p.w - 7 * cell) / 2;
        for (i, name) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
            .iter()
            .enumerate()
        {
            let r = Rect::new(x0 + i as i32 * cell, p.y + 104, cell, 24);
            c.text_centered(r, name, theme::text_dim());
        }
        // Monday first
        let first = (rtc::weekday(year, month, 1) as i32 + 6) % 7;
        let days = days_in_month(year, month);
        for d in 1..=days {
            let i = first + d - 1;
            let r = Rect::new(x0 + (i % 7) * cell, p.y + 132 + (i / 7) * cell, cell, cell);
            let mut label = StackString::<4>::new();
            let _ = write!(label, "{}", d);
            if d == day as i32 {
                c.fill_round(r.inset(3), (cell - 6) / 2, theme::accent());
                c.text_centered(r, label.as_str(), theme::on_accent());
            } else {
                c.text_centered(r, label.as_str(), theme::text());
            }
        }
    }
}

pub fn layout_label(layout: Layout) -> &'static str {
    match layout {
        Layout::Us => "ENG",
        Layout::Ru => "РУС",
    }
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn days_in_month(year: u16, month: u8) -> i32 {
    match month {
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// ---- icons, 16x16 with the top left corner at (x, y) --------------------------

/// A Wi-Fi fan: a dot and three arcs, the first `bars` of the four in
/// `ink` and the rest faint.
pub fn wifi_icon(c: &mut Canvas, x: i32, y: i32, bars: u8, ink: Color, bg: Color) {
    let faint = mix(bg, ink, 70);
    // centre of the fan, at the bottom middle; distances in quarter pixels
    let (cx, cy) = (x * 4 + 32, y * 4 + 56);
    for py in y..y + 16 {
        for px in x..x + 16 {
            let (dx, dy) = (px * 4 + 2 - cx, py * 4 + 2 - cy);
            // only the upward quarter, 45 degrees each side
            if dy > 0 || dx.abs() > -dy + 2 {
                continue;
            }
            let d = libm::sqrtf((dx * dx + dy * dy) as f32) as i32;
            let bar = match d {
                0..=9 => 1,
                20..=27 => 2,
                36..=43 => 3,
                52..=59 => 4,
                _ => continue,
            };
            c.fill_rect(px, py, 1, 1, if bar <= bars { ink } else { faint });
        }
    }
}

/// A monitor with a cable, or the Wi-Fi fan; crossed out when there is
/// no connection.
pub fn network_icon(c: &mut Canvas, x: i32, y: i32, net: Net, ink: Color, bg: Color) {
    match net {
        Net::Wifi(bars) => return wifi_icon(c, x, y, bars, ink, bg),
        Net::WifiConnecting => {
            wifi_icon(c, x, y, 0, ink, bg);
            for i in 0..3 {
                c.fill_rect(x + 4 + i * 3, y + 13, 2, 2, ink);
            }
            return;
        }
        Net::WifiNone | Net::WifiOff => {
            wifi_icon(c, x, y, 0, ink, bg);
            let badge = Rect::new(x + 8, y + 6, 10, 10);
            c.fill_round(badge, 5, bg);
            c.line(x + 10, y + 8, x + 15, y + 13, ink);
            c.line(x + 15, y + 8, x + 10, y + 13, ink);
            return;
        }
        _ => {}
    }
    c.outline_round(Rect::new(x + 1, y + 1, 14, 10), 2, ink);
    c.fill_rect(x + 7, y + 11, 2, 3, ink);
    c.fill_rect(x + 4, y + 14, 8, 1, ink);
    match net {
        Net::Online => {}
        Net::Connecting => {
            for i in 0..3 {
                c.fill_rect(x + 4 + i * 3, y + 6, 2, 2, ink);
            }
        }
        _ => {
            // a small cross badge in the corner
            let badge = Rect::new(x + 8, y + 6, 10, 10);
            c.fill_round(badge, 5, bg);
            c.line(x + 10, y + 8, x + 15, y + 13, ink);
            c.line(x + 15, y + 8, x + 10, y + 13, ink);
        }
    }
}

/// A speaker with one to three sound waves, or a cross when muted.
pub fn volume_icon(c: &mut Canvas, x: i32, y: i32, volume: i32, ink: Color) {
    c.fill_rect(x, y + 5, 4, 6, ink);
    c.fill_polygon(
        &[
            (x + 3, y + 5),
            (x + 8, y + 1),
            (x + 8, y + 15),
            (x + 3, y + 11),
        ],
        ink,
    );
    if volume == 0 {
        c.line(x + 10, y + 5, x + 15, y + 10, ink);
        c.line(x + 15, y + 5, x + 10, y + 10, ink);
        return;
    }
    let waves = 1 + (volume > 33) as i32 + (volume > 66) as i32;
    for i in 0..waves {
        let r = 4 + i * 3;
        // the right half of a ring around the speaker's mouth
        let ring = Rect::new(x + 8 - r, y + 8 - r, 2 * r, 2 * r);
        let mut half = c.sub(Rect::new(0, 0, c.width, c.height));
        half.clip_to(Rect::new(x + 10, y - 2, 10, 20));
        half.outline_round(ring, r, ink);
    }
}

/// A sun: a disc and eight rays.
pub fn sun_icon(c: &mut Canvas, x: i32, y: i32, ink: Color) {
    c.fill_round(Rect::new(x + 5, y + 5, 6, 6), 3, ink);
    for (dx, dy) in [
        (0, -1),
        (1, -1),
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
    ] {
        let (cx, cy) = (x + 8, y + 8);
        c.line(cx + dx * 5, cy + dy * 5, cx + dx * 7, cy + dy * 7, ink);
    }
}
