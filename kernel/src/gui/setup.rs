//! The installer, in the style of Windows 11 Setup: a window over the
//! blurred wallpaper that walks through the language, the license, the
//! disk and the user's account, then shows the installation's progress
//! and restarts into the installed EverOS. The disc can also start
//! EverOS without installing it ("Try EverOS").
//!
//! The work itself is in setup.rs at the top of the kernel.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{isqrt, mix, rgb, Canvas, Dirty, Rect};
use super::power::draw_spinner;
use super::text::{HEADING, TITLE, UI, UI_BOLD};
use super::theme;
use super::widgets::TextField;
use crate::keyboard::{self, Key};
use crate::setup::{self, Disk, Files, Install, Step};
use crate::{interrupts, serial};

pub enum Outcome {
    None,
    /// Start EverOS from the disc without installing it.
    Try,
    Restart,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Language,
    Start,
    License,
    Disk,
    Account,
    Ready,
    Installing,
    Done,
    Failed,
}

/// Something on a page that can be clicked or get the keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Lang(bool),
    Install,
    Try,
    Accept,
    Disk(usize),
    Keep(bool),
    Field(usize),
    Back,
    Next,
}

const LICENSE: &str = include_str!("../../../LICENSE");
const DIALOG_W: i32 = 880;
const DIALOG_H: i32 = 640;
const TITLE_H: i32 = 44;
const FOOTER_H: i32 = 72;
const BUTTON_W: i32 = 150;
const ROW_H: i32 = 44;
const FIELD_W: i32 = 360;
const FIELD_H: i32 = 34;
/// Seconds before the computer restarts once EverOS is installed.
const RESTART_SECONDS: u64 = 10;
const BLINK_TICKS: u64 = interrupts::TIMER_HZ / 2;
const NAME_MAX: usize = crate::users::MAX_NAME;
const PASSWORD_MAX: usize = 32;
const AVATAR: i32 = 150;

pub struct Setup {
    width: i32,
    height: i32,
    files: Option<Files>,
    page: Page,
    russian: bool,
    accepted: bool,
    disks: Vec<Disk>,
    disk: usize,
    keep: bool,
    /// Name, password and the password again.
    fields: [TextField; 3],
    /// Why the account page can't go on, in both languages.
    problem: Option<(&'static str, &'static str)>,
    /// Why the installation failed.
    failure: &'static str,
    focus: Ctl,
    hover: Option<Ctl>,
    install: Option<Install>,
    /// When the installation finished.
    done_at: u64,
    /// Seconds left before the restart, as last drawn.
    countdown: u64,
    caret_on: bool,
    next_blink: u64,
    /// Keyboard layout, shown in the corner.
    pub layout: &'static str,
    pub dirty: Dirty,
    avatar: Vec<u32>,
}

fn page_name(p: Page) -> &'static str {
    match p {
        Page::Language => "language",
        Page::Start => "start",
        Page::License => "license",
        Page::Disk => "disk",
        Page::Account => "account",
        Page::Ready => "ready",
        Page::Installing => "installing",
        Page::Done => "done",
        Page::Failed => "failed",
    }
}

/// A size in megabytes or gigabytes.
fn size_text(bytes: u64, russian: bool) -> String {
    let mib = bytes / (1024 * 1024);
    let (mb, gb) = if russian {
        ("МБ", "ГБ")
    } else {
        ("MB", "GB")
    };
    if mib >= 1024 {
        let tenths = mib * 10 / 1024;
        let sep = if russian { ',' } else { '.' };
        format!("{}{}{} {}", tenths / 10, sep, tenths % 10, gb)
    } else {
        format!("{} {}", mib, mb)
    }
}

impl Setup {
    pub fn new(files: Option<Files>, width: i32, height: i32) -> Self {
        serial::write_str("setup: welcome\n");
        if files.is_none() {
            serial::write_str("setup: the installation files are missing\n");
        }
        Self {
            width,
            height,
            files,
            page: Page::Language,
            russian: true,
            accepted: false,
            disks: Vec::new(),
            disk: 0,
            keep: false,
            fields: [
                TextField::default(),
                TextField::default(),
                TextField::default(),
            ],
            problem: None,
            failure: "",
            focus: Ctl::Next,
            hover: None,
            install: None,
            done_at: 0,
            countdown: RESTART_SECONDS,
            caret_on: true,
            next_blink: 0,
            layout: "EN",
            dirty: Dirty::default(),
            avatar: super::login::avatar(AVATAR),
        }
    }

    /// The installation is running, so the main loop should not sleep.
    pub fn busy(&self) -> bool {
        self.page == Page::Installing
    }

    fn t(&self, ru: &'static str, en: &'static str) -> &'static str {
        if self.russian {
            ru
        } else {
            en
        }
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn redraw(&mut self) {
        self.dirty.add(self.dialog());
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.hover = None;
        self.focus = match page {
            Page::Language | Page::Ready | Page::Done => Ctl::Next,
            Page::Start => Ctl::Install,
            Page::License => Ctl::Accept,
            Page::Disk => Ctl::Disk(self.disk),
            Page::Account => Ctl::Field(0),
            Page::Installing => Ctl::Next,
            Page::Failed => Ctl::Back,
        };
        serial::write_str("setup: page ");
        serial::write_str(page_name(page));
        serial::write_str("\n");
        self.redraw();
    }

    // ---- layout ------------------------------------------------------------

    fn dialog(&self) -> Rect {
        let w = DIALOG_W.min(self.width - 32);
        let h = DIALOG_H.min(self.height - 64);
        Rect::new((self.width - w) / 2, (self.height - h) / 2 - 12, w, h)
    }

    /// Where a page draws, between the title bar and the buttons.
    fn content(&self) -> Rect {
        let d = self.dialog();
        Rect::new(
            d.x + 48,
            d.y + TITLE_H + 28,
            d.w - 96,
            d.h - TITLE_H - 28 - FOOTER_H - 12,
        )
    }

    fn body_top(&self) -> i32 {
        self.content().y + TITLE.line_height + 20
    }

    fn controls(&self) -> Vec<Ctl> {
        let mut out = Vec::new();
        match self.page {
            Page::Language => out.extend([Ctl::Lang(true), Ctl::Lang(false), Ctl::Next]),
            Page::Start => out.extend([Ctl::Install, Ctl::Try, Ctl::Back]),
            Page::License => out.extend([Ctl::Accept, Ctl::Back, Ctl::Next]),
            Page::Disk => {
                out.extend((0..self.disks.len()).map(Ctl::Disk));
                if self.can_keep() {
                    out.extend([Ctl::Keep(true), Ctl::Keep(false)]);
                }
                out.extend([Ctl::Back, Ctl::Next]);
            }
            Page::Account => out.extend([
                Ctl::Field(0),
                Ctl::Field(1),
                Ctl::Field(2),
                Ctl::Back,
                Ctl::Next,
            ]),
            Page::Ready | Page::Failed => out.extend([Ctl::Back, Ctl::Next]),
            Page::Done => out.push(Ctl::Next),
            Page::Installing => {}
        }
        out
    }

    fn rect(&self, ctl: Ctl) -> Rect {
        let d = self.dialog();
        let c = self.content();
        let top = self.body_top();
        let next = Rect::new(d.right() - 24 - BUTTON_W, d.bottom() - 52, BUTTON_W, 32);
        match ctl {
            Ctl::Next => next,
            Ctl::Back => next.offset(-BUTTON_W - 12, 0),
            Ctl::Lang(ru) => Rect::new(c.x, top + if ru { 0 } else { ROW_H + 8 }, 420, ROW_H),
            Ctl::Install => Rect::new(c.x + (c.w - 280) / 2, c.y + 300, 280, 44),
            Ctl::Try => Rect::new(c.x + (c.w - 320) / 2, c.y + 356, 320, 32),
            Ctl::Accept => Rect::new(c.x, c.bottom() - 28, 400, 28),
            Ctl::Disk(i) => Rect::new(c.x, top + 32 + i as i32 * ROW_H, c.w, ROW_H),
            Ctl::Keep(keep) => {
                let y = self.rect(Ctl::Disk(self.disks.len().max(1) - 1)).bottom() + 24;
                Rect::new(c.x, y + if keep { 0 } else { 40 }, c.w, 36)
            }
            Ctl::Field(i) => Rect::new(c.x, top + 60 + i as i32 * 74, FIELD_W, FIELD_H),
        }
    }

    fn ctl_at(&self, x: i32, y: i32) -> Option<Ctl> {
        self.controls()
            .into_iter()
            .find(|&k| self.rect(k).contains(x, y) && self.enabled(k))
    }

    fn enabled(&self, ctl: Ctl) -> bool {
        match (self.page, ctl) {
            (Page::License, Ctl::Next) => self.accepted,
            (Page::Disk, Ctl::Next) => self.disk_ok(),
            (Page::Disk, Ctl::Disk(i)) => i < self.disks.len(),
            _ => true,
        }
    }

    fn selected_disk(&self) -> Option<&Disk> {
        self.disks.get(self.disk)
    }

    fn can_keep(&self) -> bool {
        match (self.selected_disk(), &self.files) {
            (Some(d), Some(f)) => d.can_keep(f),
            _ => false,
        }
    }

    fn disk_ok(&self) -> bool {
        self.files.is_some() && self.selected_disk().is_some_and(|d| d.big_enough())
    }

    // ---- actions -----------------------------------------------------------

    fn scan_disks(&mut self) {
        self.disks = setup::disks();
        serial::write_str(&format!("setup: {} disk(s) found\n", self.disks.len()));
        // the first disk EverOS fits on
        self.disk = self.disks.iter().position(|d| d.big_enough()).unwrap_or(0);
        self.choose_disk(self.disk);
    }

    fn choose_disk(&mut self, i: usize) {
        self.disk = i;
        // keep the files on a disk that has some, unless the user says so
        self.keep = self.can_keep();
        self.redraw();
    }

    fn activate(&mut self, ctl: Ctl) -> Outcome {
        if !self.enabled(ctl) {
            return Outcome::None;
        }
        self.focus = ctl;
        match ctl {
            Ctl::Lang(ru) => {
                self.russian = ru;
                self.redraw();
            }
            Ctl::Install => self.go(Page::License),
            Ctl::Try => {
                serial::write_str("setup: trying EverOS without installing\n");
                return Outcome::Try;
            }
            Ctl::Accept => {
                self.accepted = !self.accepted;
                self.redraw();
            }
            Ctl::Disk(i) => self.choose_disk(i),
            Ctl::Keep(keep) => {
                self.keep = keep;
                self.redraw();
            }
            Ctl::Field(_) => self.redraw(),
            Ctl::Back => self.back(),
            Ctl::Next => return self.next(),
        }
        Outcome::None
    }

    fn back(&mut self) {
        match self.page {
            Page::Start => self.go(Page::Language),
            Page::License => self.go(Page::Start),
            Page::Disk => self.go(Page::License),
            Page::Account => self.go(Page::Disk),
            Page::Ready => self.go(Page::Account),
            Page::Failed => {
                self.scan_disks();
                self.go(Page::Disk);
            }
            _ => {}
        }
    }

    fn next(&mut self) -> Outcome {
        match self.page {
            Page::Language => self.go(Page::Start),
            Page::Start => self.go(Page::License),
            Page::License if self.accepted => {
                self.scan_disks();
                self.go(Page::Disk);
            }
            Page::Disk if self.disk_ok() => self.go(Page::Account),
            Page::Account => {
                self.problem = self.check_account();
                if self.problem.is_none() {
                    self.go(Page::Ready);
                } else {
                    self.redraw();
                }
            }
            Page::Ready => self.start_install(),
            Page::Done | Page::Failed => return Outcome::Restart,
            _ => {}
        }
        Outcome::None
    }

    fn check_account(&self) -> Option<(&'static str, &'static str)> {
        let name = self.fields[0].string();
        if name.is_empty() {
            return Some(("Введите имя пользователя.", "Enter a user name."));
        }
        if !crate::users::name_ok(&name) {
            return Some((
                "В имени можно использовать латинские буквы, цифры, - и _.",
                "A name can have Latin letters, digits, - and _.",
            ));
        }
        if self.fields[1].text != self.fields[2].text {
            return Some(("Пароли не совпадают.", "The passwords don't match."));
        }
        None
    }

    fn start_install(&mut self) {
        let (Some(files), Some(disk)) = (self.files, self.selected_disk()) else {
            return;
        };
        let name = self.fields[0].string();
        let password = self.fields[1].string();
        self.install = Some(Install::new(
            files,
            disk.ata.clone(),
            self.keep,
            &name,
            &password,
        ));
        self.go(Page::Installing);
    }

    // ---- time --------------------------------------------------------------

    /// Do a piece of the installation and move animations on.
    pub fn tick(&mut self) -> Outcome {
        let now = interrupts::ticks();
        match self.page {
            Page::Installing => {
                if let Some(install) = &mut self.install {
                    match install.work() {
                        Err(msg) => {
                            self.failure = msg;
                            self.install = None;
                            self.go(Page::Failed);
                        }
                        Ok(()) if install.step == Step::Done => {
                            self.done_at = now;
                            self.countdown = RESTART_SECONDS;
                            self.go(Page::Done);
                        }
                        Ok(()) => self.redraw(),
                    }
                }
            }
            Page::Done => {
                let passed = (now - self.done_at) / interrupts::TIMER_HZ;
                let left = RESTART_SECONDS.saturating_sub(passed);
                if left != self.countdown {
                    self.countdown = left;
                    self.redraw();
                    if left == 0 {
                        return Outcome::Restart;
                    }
                }
            }
            _ => {}
        }
        if matches!(self.focus, Ctl::Field(_)) && now >= self.next_blink {
            self.next_blink = now + BLINK_TICKS;
            self.caret_on = !self.caret_on;
            self.dirty.add(self.rect(self.focus).inset(-2));
        }
        Outcome::None
    }

    // ---- input -------------------------------------------------------------

    fn move_focus(&mut self, back: bool) {
        let list = self.controls();
        let list: Vec<Ctl> = list.into_iter().filter(|&k| self.enabled(k)).collect();
        if list.is_empty() {
            return;
        }
        let at = list.iter().position(|&k| k == self.focus).unwrap_or(0);
        let n = list.len();
        self.focus = list[if back { (at + n - 1) % n } else { (at + 1) % n }];
        self.caret_on = true;
        self.redraw();
    }

    /// Up and Down move between the choices of a group and pick them.
    fn step_choice(&mut self, down: bool) -> bool {
        match self.focus {
            Ctl::Lang(_) => {
                let _ = self.activate(Ctl::Lang(!down));
            }
            Ctl::Keep(_) => {
                let _ = self.activate(Ctl::Keep(!down));
            }
            Ctl::Disk(i) => {
                let n = self.disks.len();
                let j = if down {
                    (i + 1).min(n - 1)
                } else {
                    i.saturating_sub(1)
                };
                let _ = self.activate(Ctl::Disk(j));
            }
            Ctl::Field(i) => {
                let j = if down {
                    (i + 1).min(2)
                } else {
                    i.saturating_sub(1)
                };
                self.focus = Ctl::Field(j);
                self.redraw();
            }
            _ => return false,
        }
        true
    }

    pub fn on_key(&mut self, key: Key) -> Outcome {
        match key {
            Key::LayoutChanged => {
                self.dirty.add(self.layout_rect());
                return Outcome::None;
            }
            Key::Char('\t') => {
                self.move_focus(keyboard::shift_held());
                return Outcome::None;
            }
            Key::Up | Key::Down if self.step_choice(matches!(key, Key::Down)) => {
                return Outcome::None;
            }
            Key::Enter => {
                return match self.focus {
                    Ctl::Install | Ctl::Try | Ctl::Back | Ctl::Next => self.activate(self.focus),
                    Ctl::Field(i) if i < 2 && self.page == Page::Account => {
                        self.focus = Ctl::Field(i + 1);
                        self.redraw();
                        Outcome::None
                    }
                    _ => self.next(),
                };
            }
            Key::Escape if self.page != Page::Installing => {
                self.back();
                return Outcome::None;
            }
            _ => {}
        }
        if let Ctl::Field(i) = self.focus {
            if self.page == Page::Account {
                let max = if i == 0 { NAME_MAX } else { PASSWORD_MAX };
                let before = self.fields[i].clone();
                self.fields[i].on_key(key);
                if self.fields[i].text.len() > max {
                    self.fields[i] = before;
                }
                self.problem = None;
                self.caret_on = true;
                self.next_blink = interrupts::ticks() + BLINK_TICKS;
                self.redraw();
            }
            return Outcome::None;
        }
        if let Key::Char(' ') = key {
            return self.activate(self.focus);
        }
        Outcome::None
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        let hover = self.ctl_at(x, y);
        if hover != self.hover {
            self.hover = hover;
            self.redraw();
        }
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Outcome {
        let Some(ctl) = self.ctl_at(x, y) else {
            return Outcome::None;
        };
        if let Ctl::Field(i) = ctl {
            self.fields[i].click(self.rect(ctl), x);
            self.caret_on = true;
            self.next_blink = interrupts::ticks() + BLINK_TICKS;
        }
        self.activate(ctl)
    }

    // ---- drawing -----------------------------------------------------------

    fn layout_rect(&self) -> Rect {
        Rect::new(self.width - 80, self.height - 48, 64, 32)
    }

    pub fn draw(&self, c: &mut Canvas, backdrop: &[u32]) {
        c.blit(0, 0, self.width, self.height, backdrop, self.width as usize);
        let white = 0xffffff;
        let l = self.layout_rect();
        c.text_centered(l, self.layout, white);

        let d = self.dialog();
        if !c.visible(d.inset(-24)) {
            return;
        }
        c.shadow(d, 8, 18, 6, 120);
        c.fill_round(d, 8, theme::face());
        c.outline_round(d, 8, theme::stroke());

        // the title bar with the logo
        super::draw_start_logo_at(c, d.x + 16, d.y + 12, 1, 255);
        c.draw_text(
            d.x + 50,
            d.y + (TITLE_H - UI.line_height) / 2,
            self.t("Установка EverOS", "EverOS Setup"),
            theme::text(),
        );
        c.fill_rect(d.x + 1, d.y + TITLE_H, d.w - 2, 1, theme::stroke());

        match self.page {
            Page::Language => self.draw_language(c),
            Page::Start => self.draw_start(c),
            Page::License => self.draw_license(c),
            Page::Disk => self.draw_disk(c),
            Page::Account => self.draw_account(c),
            Page::Ready => self.draw_ready(c),
            Page::Installing => self.draw_installing(c),
            Page::Done => self.draw_done(c),
            Page::Failed => self.draw_failed(c),
        }

        // the buttons at the bottom
        let buttons = self.controls();
        if buttons.iter().any(|k| matches!(k, Ctl::Back | Ctl::Next)) {
            let y = d.bottom() - FOOTER_H;
            c.fill_rect(d.x + 1, y, d.w - 2, 1, theme::stroke());
            let footer = Rect::new(d.x + 1, y + 1, d.w - 2, FOOTER_H - 9);
            c.fill(footer, theme::footer());
            c.fill_round(
                Rect::new(d.x + 1, d.bottom() - 16, d.w - 2, 15),
                7,
                theme::footer(),
            );
        }
        for k in buttons {
            match k {
                Ctl::Back => self.draw_button(c, k, self.t("Назад", "Back"), false),
                Ctl::Next => {
                    let label = match self.page {
                        Page::Ready => self.t("Установить", "Install"),
                        Page::Done | Page::Failed => self.t("Перезагрузить", "Restart now"),
                        _ => self.t("Далее", "Next"),
                    };
                    self.draw_button(c, k, label, true);
                }
                _ => {}
            }
        }
    }

    fn heading(&self, c: &mut Canvas, text: &str) {
        let r = self.content();
        c.draw_text_in(&TITLE, r.x, r.y, text, theme::text());
    }

    fn draw_focus(&self, c: &mut Canvas, ctl: Ctl, radius: i32) {
        if self.focus == ctl {
            c.outline_round(self.rect(ctl).inset(-3), radius + 3, theme::text());
        }
    }

    fn draw_button(&self, c: &mut Canvas, ctl: Ctl, label: &str, primary: bool) {
        let r = self.rect(ctl);
        if !self.enabled(ctl) {
            c.fill_round(r, theme::CONTROL_RADIUS, theme::control());
            c.outline_round(r, theme::CONTROL_RADIUS, theme::stroke());
            c.text_centered(r, label, theme::text_dim());
            return;
        }
        let lit = self.hover == Some(ctl);
        if primary {
            let face = if lit {
                mix(theme::accent(), 0xffffff, 30)
            } else {
                theme::accent()
            };
            c.fill_round(r, theme::CONTROL_RADIUS, face);
            c.text_centered(r, label, theme::on_accent());
        } else {
            let face = if lit {
                theme::control_lit()
            } else {
                theme::control()
            };
            theme::colored_button(c, r, label, face, false);
        }
        self.draw_focus(c, ctl, theme::CONTROL_RADIUS);
    }

    /// A round choice with its label.
    fn draw_radio(&self, c: &mut Canvas, ctl: Ctl, on: bool, label: &str, note: Option<&str>) {
        let r = self.rect(ctl);
        if self.hover == Some(ctl) {
            c.fill_round(r, 6, theme::row_hover());
        }
        let (cx, cy) = (r.x + 22, r.y + r.h / 2);
        let ring = Rect::new(cx - 10, cy - 10, 20, 20);
        if on {
            c.fill_round(ring, 10, theme::accent());
            c.fill_round(Rect::new(cx - 4, cy - 4, 8, 8), 4, theme::on_accent());
        } else {
            c.fill_round(ring, 10, theme::light());
            c.outline_round(ring, 10, theme::text_dim());
        }
        let x = r.x + 46;
        match note {
            Some(note) => {
                c.draw_text(x, r.y + 3, label, theme::text());
                c.draw_text(x, r.y + 3 + UI.line_height, note, theme::text_dim());
            }
            None => {
                c.draw_text(x, r.y + (r.h - UI.line_height) / 2, label, theme::text());
            }
        }
        self.draw_focus(c, ctl, 6);
    }

    fn draw_check(&self, c: &mut Canvas, x: i32, y: i32, s: i32, color: u32) {
        // a tick: two thick strokes that meet at the bottom
        let half = (s / 7).max(1);
        let points = [
            (x, y + s / 2),
            (x + s * 3 / 8, y + s * 7 / 8),
            (x + s, y + s / 8),
        ];
        for pair in points.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            let (dx, dy) = (x1 - x0, y1 - y0);
            let len = isqrt((dx * dx + dy * dy) as u64).max(1) as i32;
            // across the stroke, `half` pixels long
            let (nx, ny) = (-dy * half / len, dx * half / len);
            let quad = [
                (x0 + nx, y0 + ny),
                (x1 + nx, y1 + ny),
                (x1 - nx, y1 - ny),
                (x0 - nx, y0 - ny),
            ];
            c.fill_polygon(&quad, color);
            // a round joint where the strokes meet
            c.fill_round(
                Rect::new(x1 - half, y1 - half, 2 * half, 2 * half),
                half,
                color,
            );
        }
    }

    fn draw_language(&self, c: &mut Canvas) {
        self.heading(c, "Выберите язык  ·  Choose your language");
        self.draw_radio(c, Ctl::Lang(true), self.russian, "Русский", None);
        self.draw_radio(c, Ctl::Lang(false), !self.russian, "English", None);
        let y = self.rect(Ctl::Lang(false)).bottom() + 32;
        let x = self.content().x;
        c.draw_text(
            x,
            y,
            self.t(
                "Раскладка клавиатуры: английская и русская, Alt+Shift переключает их.",
                "Keyboard layouts: English and Russian, Alt+Shift switches between them.",
            ),
            theme::text_dim(),
        );
    }

    fn draw_start(&self, c: &mut Canvas) {
        let r = self.content();
        let s = 5;
        super::draw_start_logo_at(c, r.x + (r.w - 23 * s) / 2, r.y + 40, s, 255);
        let y = r.y + 40 + 23 * s + 20;
        c.text_centered_in(
            &HEADING,
            Rect::new(r.x, y, r.w, 40),
            "EverOS",
            theme::text(),
        );
        c.text_centered(
            Rect::new(r.x, y + 42, r.w, 24),
            self.t(
                "Операционная система на ассемблере и Rust",
                "An operating system in assembly and Rust",
            ),
            theme::text_dim(),
        );
        self.draw_button(c, Ctl::Install, self.t("Установить", "Install now"), true);
        let t = self.rect(Ctl::Try);
        let label = self.t(
            "Попробовать EverOS без установки",
            "Try EverOS without installing",
        );
        let color = theme::accent();
        c.text_centered(t, label, color);
        if self.hover == Some(Ctl::Try) {
            let w = UI.width(label);
            c.fill_rect(t.x + (t.w - w) / 2, t.y + t.h / 2 + 9, w, 1, color);
        }
        self.draw_focus(c, Ctl::Try, 4);
    }

    fn draw_license(&self, c: &mut Canvas) {
        self.heading(c, self.t("Условия лицензии", "License terms"));
        let r = self.content();
        let top = self.body_top();
        let bx = Rect::new(r.x, top, r.w, r.bottom() - 44 - top);
        c.fill_round(bx, 6, theme::light());
        c.outline_round(bx, 6, theme::stroke());
        let mut t = c.sub(Rect::new(0, 0, c.width, c.height));
        t.clip_to(bx.inset(1));
        let mut y = bx.y + 14;
        let para = self.t(
            "EverOS распространяется по лицензии MIT:",
            "EverOS is distributed under the MIT license:",
        );
        t.draw_text_in(&UI_BOLD, bx.x + 16, y, para, theme::text());
        y += UI.line_height + 10;
        for line in LICENSE.lines() {
            if line.is_empty() {
                y += 8;
                continue;
            }
            t.draw_text(bx.x + 16, y, line, theme::text());
            y += UI.line_height;
        }

        let a = self.rect(Ctl::Accept);
        let b = Rect::new(a.x + 2, a.y + 5, 18, 18);
        if self.accepted {
            c.fill_round(b, 4, theme::accent());
            self.draw_check(c, b.x + 4, b.y + 4, 10, theme::on_accent());
        } else {
            c.fill_round(b, 4, theme::light());
            c.outline_round(b, 4, theme::text_dim());
        }
        let label = self.t("Я принимаю условия лицензии", "I accept the license terms");
        c.draw_text(
            a.x + 30,
            a.y + (a.h - UI.line_height) / 2,
            label,
            theme::text(),
        );
        self.draw_focus(c, Ctl::Accept, 4);
    }

    fn disk_state(&self, d: &Disk) -> String {
        if !d.big_enough() {
            return String::from(
                self.t("Слишком мал: нужно от 64 МБ", "Too small: 64 MB is needed"),
            );
        }
        if d.probe.installed {
            return String::from(self.t("Установлена EverOS", "EverOS is installed"));
        }
        if d.probe.start.is_some() {
            let free = size_text(d.probe.free, self.russian);
            return format!("FAT32, {} {}", self.t("свободно", "free"), free);
        }
        String::from(if d.probe.blank {
            self.t("Незанятое пространство", "Unallocated space")
        } else {
            self.t("Неизвестный формат", "Unknown format")
        })
    }

    fn draw_disk(&self, c: &mut Canvas) {
        self.heading(
            c,
            self.t(
                "Куда установить EverOS?",
                "Where do you want to install EverOS?",
            ),
        );
        let r = self.content();
        let top = self.body_top();
        if self.files.is_none() {
            c.draw_text(
                r.x,
                top,
                self.t(
                    "На диске установки нет файлов EverOS. Скачайте ISO заново.",
                    "The installation disc has no EverOS files. Download the ISO again.",
                ),
                theme::error(),
            );
            return;
        }
        // the table's header
        let cols = [r.x + 16, r.x + 360, r.x + 470];
        let dim = theme::text_dim();
        c.draw_text(cols[0], top + 6, self.t("Диск", "Disk"), dim);
        c.draw_text(cols[1], top + 6, self.t("Размер", "Size"), dim);
        c.draw_text(cols[2], top + 6, self.t("Что на нём", "Contents"), dim);
        c.fill_rect(r.x, top + 30, r.w, 1, theme::stroke());
        if self.disks.is_empty() {
            c.draw_text(
                cols[0],
                top + 48,
                self.t(
                    "Жёсткий диск не найден. Подключите диск и перезагрузите компьютер.",
                    "No hard disk was found. Connect one and restart the computer.",
                ),
                theme::error(),
            );
            return;
        }
        for (i, d) in self.disks.iter().enumerate() {
            let row = self.rect(Ctl::Disk(i));
            if i == self.disk {
                c.fill_round(row, 6, theme::accent_light());
                c.fill_round(
                    Rect::new(row.x + 2, row.y + 12, 3, row.h - 24),
                    1,
                    theme::accent(),
                );
            } else if self.hover == Some(Ctl::Disk(i)) {
                c.fill_round(row, 6, theme::row_hover());
            }
            let y = row.y + (row.h - UI.line_height) / 2;
            let model = if d.model.is_empty() {
                "ATA"
            } else {
                d.model.as_str()
            };
            let name = format!("{} {}: {}", self.t("Диск", "Disk"), i, model);
            // a small drive picture
            let ic = Rect::new(cols[0], row.y + 13, 22, 18);
            c.fill_round(ic, 3, rgb(0x8a, 0x94, 0xa6));
            c.fill_rect(
                ic.x + 3,
                ic.bottom() - 6,
                ic.w - 6,
                2,
                rgb(0xd8, 0xde, 0xe8),
            );
            c.fill_round(
                Rect::new(ic.right() - 6, ic.y + 4, 3, 3),
                1,
                rgb(0x5c, 0xe0, 0x7c),
            );
            c.draw_text(cols[0] + 34, y, &name, theme::text());
            c.draw_text(cols[1], y, &size_text(d.bytes, self.russian), theme::text());
            let state = self.disk_state(d);
            let color = if d.big_enough() {
                theme::text()
            } else {
                theme::error()
            };
            c.draw_text(cols[2], y, &state, color);
            self.draw_focus(c, Ctl::Disk(i), 6);
        }
        let Some(d) = self.selected_disk() else {
            return;
        };
        if !d.big_enough() {
            return;
        }
        let mut y = self.rect(Ctl::Disk(self.disks.len() - 1)).bottom() + 24;
        if self.can_keep() {
            self.draw_radio(
                c,
                Ctl::Keep(true),
                self.keep,
                self.t(
                    "Сохранить файлы и установить поверх",
                    "Keep the files and install over them",
                ),
                None,
            );
            self.draw_radio(
                c,
                Ctl::Keep(false),
                !self.keep,
                self.t(
                    "Стереть диск и установить начисто",
                    "Erase the disk and do a clean install",
                ),
                None,
            );
            y = self.rect(Ctl::Keep(false)).bottom() + 16;
        }
        let (text, color) = if self.keep {
            (
                self.t(
                    "Документы и пользователи на диске сохранятся, система будет заменена.",
                    "Documents and accounts on the disk stay, the system is replaced.",
                ),
                theme::text_dim(),
            )
        } else {
            (
                self.t(
                    "Все файлы на этом диске будут удалены.",
                    "Everything on this disk will be deleted.",
                ),
                theme::warning(),
            )
        };
        c.draw_text(r.x + 16, y, text, color);
    }

    fn draw_field(&self, c: &mut Canvas, i: usize, label: &str) {
        let r = self.rect(Ctl::Field(i));
        c.draw_text(r.x, r.y - UI.line_height - 6, label, theme::text());
        let focused = self.focus == Ctl::Field(i);
        c.fill_round(r, 4, theme::light());
        c.outline_round(r, 4, theme::stroke());
        let f = &self.fields[i];
        let y = r.y + (r.h - UI.line_height) / 2;
        let x0 = r.x + 10;
        let caret_x = if i == 0 {
            let s = f.string();
            c.draw_text(x0, y, &s, theme::text());
            let w: i32 = f.text[..f.cursor]
                .iter()
                .map(|&ch| UI.advance16(ch) as i32)
                .sum();
            x0 + (w + 8) / 16
        } else {
            for k in 0..f.text.len() as i32 {
                let dot = Rect::new(x0 + k * 11, r.y + r.h / 2 - 4, 8, 8);
                c.fill_round(dot, 4, theme::text());
            }
            x0 + f.cursor as i32 * 11 - 2
        };
        if focused {
            c.fill_rect(r.x + 1, r.bottom() - 2, r.w - 2, 2, theme::accent());
            if self.caret_on {
                c.fill_rect(caret_x, y, 1, UI.line_height, theme::text());
            }
        }
    }

    fn draw_account(&self, c: &mut Canvas) {
        self.heading(
            c,
            self.t(
                "Кто будет пользоваться этим компьютером?",
                "Who's going to use this computer?",
            ),
        );
        let r = self.content();
        let top = self.body_top();
        c.draw_text(
            r.x,
            top,
            self.t(
                "Имя: латинские буквы, цифры, - и _. Пароль можно не задавать.",
                "Name: Latin letters, digits, - and _. The password may be left empty.",
            ),
            theme::text_dim(),
        );
        self.draw_field(c, 0, self.t("Имя пользователя", "User name"));
        self.draw_field(c, 1, self.t("Пароль", "Password"));
        self.draw_field(c, 2, self.t("Повторите пароль", "Confirm the password"));
        if let Some((ru, en)) = self.problem {
            let y = self.rect(Ctl::Field(2)).bottom() + 16;
            c.draw_text(r.x, y, self.t(ru, en), theme::error());
        }
        // the user picture next to the fields
        let a = Rect::new(r.x + FIELD_W + 110, top + 60, AVATAR, AVATAR);
        c.blit_alpha(a.x, a.y, a.w, a.h, &self.avatar);
        let name = self.fields[0].string();
        if !name.is_empty() {
            c.text_centered_in(
                &UI_BOLD,
                Rect::new(a.x - 60, a.bottom() + 12, a.w + 120, 24),
                &name,
                theme::text(),
            );
        }
    }

    fn draw_ready(&self, c: &mut Canvas) {
        self.heading(c, self.t("Всё готово к установке", "Ready to install"));
        let r = self.content();
        let mut y = self.body_top();
        c.draw_text(
            r.x,
            y,
            self.t(
                "EverOS будет установлена так:",
                "EverOS will be installed like this:",
            ),
            theme::text_dim(),
        );
        y += 36;
        let Some(d) = self.selected_disk() else {
            return;
        };
        let disk = format!(
            "{} {}, {} ({})",
            self.t("Диск", "Disk"),
            self.disk,
            if d.model.is_empty() {
                "ATA"
            } else {
                d.model.as_str()
            },
            size_text(d.bytes, self.russian)
        );
        let files = if self.keep {
            self.t("сохранятся", "are kept")
        } else {
            self.t("будут удалены", "are deleted")
        };
        let password = if self.fields[1].text.is_empty() {
            self.t("без пароля", "no password")
        } else {
            self.t("с паролем", "with a password")
        };
        let rows = [
            (self.t("Куда", "Where"), disk),
            (
                self.t("Файлы на диске", "Files on the disk"),
                String::from(files),
            ),
            (self.t("Раздел", "Partition"), String::from("FAT32")),
            (self.t("Загрузчик", "Boot loader"), String::from("GRUB 2")),
            (
                self.t("Учётная запись", "Account"),
                format!("{} ({})", self.fields[0].string(), password),
            ),
        ];
        for (label, value) in rows {
            c.fill_round(Rect::new(r.x + 2, y + 6, 6, 6), 3, theme::accent());
            c.draw_text(r.x + 20, y, label, theme::text_dim());
            c.draw_text(r.x + 220, y, &value, theme::text());
            y += 34;
        }
        y += 12;
        c.draw_text(
            r.x,
            y,
            self.t(
                "После установки компьютер перезагрузится и EverOS запустится с диска.",
                "When it's done, the computer restarts and EverOS starts from the disk.",
            ),
            theme::text_dim(),
        );
    }

    fn step_label(&self, s: Step) -> &'static str {
        match s {
            Step::Prepare => self.t("Подготовка диска", "Preparing the disk"),
            Step::Copy => self.t("Копирование файлов EverOS", "Copying EverOS files"),
            Step::Bootloader => self.t("Установка загрузчика", "Installing the boot loader"),
            Step::Account => self.t("Создание учётной записи", "Creating your account"),
            Step::Finish | Step::Done => self.t("Завершение", "Finishing up"),
        }
    }

    fn draw_installing(&self, c: &mut Canvas) {
        self.heading(c, self.t("Установка EverOS", "Installing EverOS"));
        let r = self.content();
        let top = self.body_top();
        c.draw_text(
            r.x,
            top,
            self.t(
                "Это займёт немного времени. Не выключайте компьютер.",
                "This won't take long. Keep the computer on.",
            ),
            theme::text_dim(),
        );
        let Some(install) = &self.install else {
            return;
        };
        let ms = interrupts::ticks() * 1000 / interrupts::TIMER_HZ;
        let mut y = top + 48;
        for s in setup::STEPS {
            let (cx, cy) = (r.x + 12, y + UI.line_height / 2);
            let label = self.step_label(s);
            if s < install.step {
                c.fill_round(Rect::new(cx - 10, cy - 10, 20, 20), 10, theme::accent());
                self.draw_check(c, cx - 5, cy - 5, 10, theme::on_accent());
                c.draw_text(r.x + 36, y, label, theme::text());
            } else if s == install.step {
                draw_spinner(c, cx, cy, 9, ms, theme::accent());
                let text = if s == Step::Copy {
                    format!("{} ({}%)", label, install.step_permille() / 10)
                } else {
                    String::from(label)
                };
                c.draw_text_in(&UI_BOLD, r.x + 36, y, &text, theme::text());
            } else {
                c.fill_round(Rect::new(cx - 4, cy - 4, 8, 8), 4, theme::stroke());
                c.draw_text(r.x + 36, y, label, theme::text_dim());
            }
            y += 40;
        }
        // the whole installation as one bar
        let index = setup::STEPS
            .iter()
            .position(|&s| s == install.step)
            .unwrap_or(4) as u32;
        let permille = (index * 1000 + install.step_permille()) / setup::STEPS.len() as u32;
        let bar = Rect::new(r.x, r.bottom() - 24, r.w, 6);
        c.fill_round(bar, 3, theme::track());
        let w = (bar.w as u32 * permille / 1000) as i32;
        if w > 0 {
            c.fill_round(Rect::new(bar.x, bar.y, w.max(6), bar.h), 3, theme::accent());
        }
        c.draw_text(
            r.x,
            bar.y - UI.line_height - 10,
            &format!("{}%", permille / 10),
            theme::text_dim(),
        );
    }

    fn draw_done(&self, c: &mut Canvas) {
        let r = self.content();
        let s = 88;
        let circle = Rect::new(r.x + (r.w - s) / 2, r.y + 30, s, s);
        c.fill_round(circle, s / 2, theme::accent());
        self.draw_check(c, circle.x + 24, circle.y + 26, 40, theme::on_accent());
        let y = circle.bottom() + 28;
        c.text_centered_in(
            &HEADING,
            Rect::new(r.x, y, r.w, 40),
            self.t("EverOS установлена", "EverOS is installed"),
            theme::text(),
        );
        let name = self.fields[0].string();
        let sign_in = if self.russian {
            format!("После перезагрузки войдите как {}.", name)
        } else {
            format!("After the restart, sign in as {}.", name)
        };
        c.text_centered(Rect::new(r.x, y + 56, r.w, 24), &sign_in, theme::text());
        let restart = if self.russian {
            format!("Перезагрузка через {} с", self.countdown)
        } else {
            format!("Restarting in {} s", self.countdown)
        };
        c.text_centered(Rect::new(r.x, y + 86, r.w, 24), &restart, theme::text_dim());
        c.text_centered(
            Rect::new(r.x, y + 130, r.w, 24),
            self.t(
                "Если снова откроется установка, извлеките ISO или выберите загрузку с диска.",
                "If Setup opens again, remove the ISO or boot from the hard disk.",
            ),
            theme::text_dim(),
        );
    }

    fn draw_failed(&self, c: &mut Canvas) {
        self.heading(
            c,
            self.t(
                "Не удалось установить EverOS",
                "EverOS couldn't be installed",
            ),
        );
        let r = self.content();
        let top = self.body_top();
        c.draw_text(r.x, top, self.failure, theme::error());
        c.draw_text(
            r.x,
            top + 36,
            self.t(
                "Нажмите «Назад», чтобы выбрать диск ещё раз.",
                "Press Back to choose the disk again.",
            ),
            theme::text_dim(),
        );
    }

    /// The whole screen needs drawing.
    pub fn show(&mut self) {
        serial::write_str("setup: page language\n");
        self.dirty.add(self.screen());
    }
}
