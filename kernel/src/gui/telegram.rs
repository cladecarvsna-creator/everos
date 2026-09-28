//! Telegram: a window in the style of Telegram Desktop. First it asks for
//! the api_id and api_hash (once), then the phone number, the code and
//! the two-step verification password; then it shows the chat list on
//! the left and the open chat on the right, with a box to write in.
//!
//! Messages show photos, files to save, links (addresses open in the
//! browser, @usernames and t.me links open the chat) and colour emoji.
//! The search box also finds people and channels by name or username;
//! a channel can be read before joining it. A right click on a message
//! copies it, a click on the chat's name shows its username and link.
//!
//! The client itself (crate::tg) runs in a fiber; this file only draws
//! what it shares and passes on what the user does.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::emoji;
use super::text::{Font, HEADING, TITLE, UI, UI_BOLD};
use super::widgets::{self, FieldEvent, TextField};
use super::{theme, App, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::keyboard::Key;
use crate::tg::{self, ChatKind, Cmd, Download, History, Message, Peer, Shared, Stage};

pub const CLIENT_W: i32 = 1100;
pub const CLIENT_H: i32 = 720;

const LIST_W: i32 = 340;
const TOP_H: i32 = 56;
const ROW_H: i32 = 68;
const HEADER_H: i32 = 30;
const INPUT_H: i32 = 56;
const AVATAR: i32 = 50;
const LINE_H: i32 = 20;
const BUBBLE_MAX: i32 = 470;
const PAD_X: i32 = 12;
const PAD_Y: i32 = 7;
/// The biggest a photo is shown in a message.
const PHOTO_W: i32 = 400;
const PHOTO_H: i32 = 340;
const FILE_H: i32 = 50;
const PICK_COLS: i32 = 15;
const PICK_CELL: i32 = 30;
const TOAST_MS: i64 = 2000;
/// Wait this long after typing in the search box before asking Telegram.
const SEARCH_WAIT_MS: i64 = 450;

/// Telegram's colours for avatars and names in groups.
const PALETTE: [Color; 7] = [
    rgb(0xe1, 0x70, 0x76),
    rgb(0xfa, 0xa7, 0x74),
    rgb(0xa6, 0x95, 0xe7),
    rgb(0x7b, 0xc8, 0x62),
    rgb(0x6e, 0xc9, 0xcb),
    rgb(0x65, 0xaa, 0xdd),
    rgb(0xee, 0x7a, 0xae),
];
const BLUE: Color = rgb(0x41, 0x9f, 0xd9);

fn palette(id: i64) -> Color {
    PALETTE[(id.unsigned_abs() % 7) as usize]
}

fn now_ms() -> i64 {
    tg::mtproto::now_ms()
}

// ---- colours for the light and the dark look --------------------------------------------

fn pick(light: Color, dark: Color) -> Color {
    if theme::dark() {
        dark
    } else {
        light
    }
}

fn panel() -> Color {
    pick(rgb(0xff, 0xff, 0xff), rgb(0x17, 0x21, 0x2b))
}

fn text() -> Color {
    pick(rgb(0x00, 0x00, 0x00), rgb(0xf5, 0xf5, 0xf5))
}

fn dim() -> Color {
    pick(rgb(0x70, 0x79, 0x81), rgb(0x70, 0x84, 0x99))
}

fn line() -> Color {
    pick(rgb(0xe7, 0xe7, 0xe7), rgb(0x0e, 0x16, 0x21))
}

fn hover() -> Color {
    pick(rgb(0xf1, 0xf1, 0xf1), rgb(0x20, 0x2b, 0x36))
}

fn selected() -> Color {
    pick(BLUE, rgb(0x2b, 0x52, 0x78))
}

fn bubble_in() -> Color {
    pick(rgb(0xff, 0xff, 0xff), rgb(0x18, 0x25, 0x33))
}

fn bubble_out() -> Color {
    pick(rgb(0xef, 0xfd, 0xde), rgb(0x2b, 0x52, 0x78))
}

fn time_in() -> Color {
    pick(rgb(0xa0, 0xac, 0xb6), rgb(0x6d, 0x7f, 0x8f))
}

fn time_out() -> Color {
    pick(rgb(0x6c, 0xb3, 0x5f), rgb(0x7d, 0xa8, 0xd3))
}

fn link_color(out: bool) -> Color {
    if out {
        pick(rgb(0x3a, 0x8e, 0x3c), rgb(0xa8, 0xd2, 0xff))
    } else {
        pick(rgb(0x16, 0x8a, 0xcd), rgb(0x6a, 0xb3, 0xf3))
    }
}

fn wall_top() -> Color {
    pick(rgb(0xd6, 0xe2, 0xb8), rgb(0x0e, 0x16, 0x21))
}

fn wall_bottom() -> Color {
    pick(rgb(0x9c, 0xc4, 0x98), rgb(0x0e, 0x16, 0x21))
}

fn pill() -> Color {
    pick(rgb(0x6f, 0x8f, 0x72), rgb(0x1e, 0x2c, 0x3a))
}

// ---- text -----------------------------------------------------------------------------

/// Text as it is drawn: typographic quotes become plain ones, emoji
/// become pictures, signs the fonts don't have become a dot. Also where
/// each character of `s` went (see `emoji::convert`).
fn shown(s: &str) -> (String, Vec<usize>) {
    let plain: String = s
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' if UI.glyph(c).is_none() => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{2033}' if UI.glyph(c).is_none() => '"',
            '\u{2010}'..='\u{2012}' | '\u{2212}' if UI.glyph(c).is_none() => '-',
            '\t' => ' ',
            c => c,
        })
        .collect();
    emoji::convert(&plain, |c| {
        UI.glyph(c).is_some() && UI_BOLD.glyph(c).is_some()
    })
}

fn clean(s: &str) -> String {
    shown(s).0
}

fn width(f: &Font, s: &str) -> i32 {
    emoji::width(f, s)
}

/// Draw shown text, emoji included; returns its width.
fn put(c: &mut Canvas, f: &Font, x: i32, y: i32, s: &str, color: Color) -> i32 {
    emoji::draw(c, f, x, y, s, color)
}

/// Split shown text into lines no wider than `max`, each with the
/// number of its first character in `s`.
fn wrap(f: &Font, s: &str, max: i32) -> Vec<(String, usize)> {
    let mut lines = Vec::new();
    let mut at = 0;
    for para in s.split('\n') {
        let mut line = String::new();
        let mut start = at;
        let mut pos = at;
        let mut w_line = 0;
        for word in para.split_inclusive(' ') {
            let n = word.chars().count();
            let w = width(f, word);
            if w_line + w <= max || line.is_empty() && w <= max {
                line.push_str(word);
                w_line += w;
                pos += n;
                continue;
            }
            if !line.is_empty() {
                lines.push((core::mem::take(&mut line), start));
                start = pos;
                w_line = 0;
            }
            if w <= max {
                line.push_str(word);
                w_line = w;
                pos += n;
                continue;
            }
            // a word longer than a line: break it anywhere
            for c in word.chars() {
                let cw = emoji::char_width(f, c);
                if w_line + cw > max && !line.is_empty() {
                    lines.push((core::mem::take(&mut line), start));
                    start = pos;
                    w_line = 0;
                }
                line.push(c);
                w_line += cw;
                pos += 1;
            }
        }
        lines.push((line, start));
        at += para.chars().count() + 1;
    }
    lines
}

/// Cut shown text to fit `max` pixels, with "..." at the end.
fn fit(f: &Font, s: &str, max: i32) -> String {
    if width(f, s) <= max {
        return String::from(s);
    }
    let dots = f.width("\u{2026}");
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = emoji::char_width(f, c);
        if w + cw + dots > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('\u{2026}');
    out
}

fn initials(title: &str) -> String {
    let mut out = String::new();
    for word in title.split_whitespace().take(2) {
        if let Some(c) = word.chars().find(|c| c.is_alphanumeric()) {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// "1.4 MB", "820 KB".
fn size_text(bytes: i64) -> String {
    if bytes >= 1 << 20 {
        let tenths = bytes * 10 / (1 << 20);
        alloc::format!("{}.{} MB", tenths / 10, tenths % 10)
    } else if bytes >= 1024 {
        alloc::format!("{} KB", bytes / 1024)
    } else {
        alloc::format!("{} B", bytes)
    }
}

/// "12 345".
fn count_text(n: i64) -> String {
    let digits = alloc::format!("{}", n);
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

// ---- time -------------------------------------------------------------------------------

/// (year, month, day, hour, minute, weekday 0 = Monday) of a local time.
fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = crate::rtc::civil_from_days(days);
    let weekday = (days + 3).rem_euclid(7);
    (y, m, d, secs / 3600, secs / 60 % 60, weekday)
}

fn clock(t: i64) -> String {
    let (_, _, _, h, m, _) = civil(t);
    alloc::format!("{:02}:{:02}", h, m)
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
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

/// A time for the chat list: the clock today, the weekday this week,
/// the date before.
fn short_date(t: i64, now: i64) -> String {
    if t == 0 {
        return String::new();
    }
    let day = t.div_euclid(86400);
    let today = now.div_euclid(86400);
    let (y, m, d, _, _, wd) = civil(t);
    if day == today {
        clock(t)
    } else if today - day < 7 && day <= today {
        String::from(WEEKDAYS[wd as usize])
    } else {
        alloc::format!("{:02}.{:02}.{:02}", d, m, y % 100)
    }
}

fn long_date(t: i64, now: i64) -> String {
    let (y, m, d, _, _, _) = civil(t);
    let (ny, _, _, _, _, _) = civil(now);
    let month = MONTHS[(m as usize).clamp(1, 12) - 1];
    if y == ny {
        alloc::format!("{} {}", d, month)
    } else {
        alloc::format!("{} {} {}", d, month, y)
    }
}

// ---- the window ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    ApiId,
    ApiHash,
    Search,
    Input,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Next,
    Send,
    Menu,
    Back,
    Emoji,
    Join,
}

/// A line of the chat list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A chat of the list, by index in `Shared::chats`.
    Chat(usize),
    /// "Global search" above the chats found.
    Header,
    /// A chat found by search, by index in `Shared::found`.
    Found(usize),
}

/// Something on screen that does something when clicked.
#[derive(Clone, PartialEq)]
enum Hit {
    Link(String),
    /// A photo: its id and its message.
    Photo(i64, i64),
    /// A file card: its message and the file's id.
    File(i64, i64),
    /// A message, for the right click menu.
    Message(i64),
    Header,
    Copy(String),
    Join,
    Leave,
    CloseInfo,
    Emoji(&'static str),
}

/// What a context menu item does.
#[derive(Clone, PartialEq)]
enum Act {
    Copy(String),
    Save(i64),
    Follow(String),
}

struct ContextMenu {
    x: i32,
    y: i32,
    items: Vec<(String, Act)>,
    hover: Option<usize>,
}

impl ContextMenu {
    fn labels(&self) -> Vec<widgets::Item<'_>> {
        self.items
            .iter()
            .map(|(l, _)| (l.as_str(), "", true))
            .collect()
    }

    fn rect(&self) -> Rect {
        let r = widgets::menu_rect(self.x, self.y, &self.labels());
        // keep it inside the window
        Rect::new(
            r.x.min(CLIENT_W - r.w - 4),
            r.y.min(CLIENT_H - r.h - 4),
            r.w,
            r.h,
        )
    }
}

/// A message laid out for drawing.
struct Laid {
    /// Offset of its top from the top of everything, and its height.
    y: i32,
    h: i32,
    kind: LaidKind,
}

enum LaidKind {
    Date(String),
    Service(String),
    Bubble {
        index: usize,
        /// Lines of shown text, with the number of their first character.
        lines: Vec<(String, usize)>,
        /// Links in the shown text: first and end character, target.
        links: Vec<(usize, usize, String)>,
        w: i32,
        /// The name above the text, in groups.
        name: Option<String>,
        /// The time sits on its own line under the text.
        time_below: bool,
        /// The size a photo is shown at.
        photo: Option<(i32, i32)>,
        /// A file card.
        file: bool,
    },
}

pub struct Telegram {
    shared: Rc<RefCell<Shared>>,
    fiber: Option<Fiber>,
    /// Stopped clients still finishing.
    draining: Vec<Fiber>,
    drawn: u64,
    api_id: TextField,
    api_hash: TextField,
    /// The phone number, the code or the password.
    form: TextField,
    form_stage: Stage,
    search: TextField,
    input: TextField,
    focus: Focus,
    open: Option<Peer>,
    /// How far the chat list is scrolled, in pixels.
    list_top: i32,
    /// How far the chat is scrolled up from its newest message.
    scroll: i32,
    /// The height of the laid out chat last time, to keep the view still
    /// when messages arrive.
    content_h: i32,
    newest: i64,
    /// The row under the mouse; `Header` also stands for "somewhere on
    /// the list".
    hover_row: Option<Row>,
    hover_button: Option<Button>,
    pressed: Option<Button>,
    menu: Option<Option<usize>>,
    /// What can be clicked in the chat, found while drawing it.
    hits: Vec<(Rect, Hit)>,
    hover_hit: Option<Hit>,
    /// The chat's details are shown.
    info: bool,
    /// The emoji picker is open.
    picker: bool,
    /// A photo shown big: its id and message.
    viewer: Option<(i64, i64)>,
    context: Option<ContextMenu>,
    /// A short note at the bottom ("Copied"), until a time.
    toast: Option<(String, i64)>,
    /// When the search box last changed, and what was searched.
    search_edit: i64,
    searched: String,
}

impl Telegram {
    pub fn new() -> Self {
        Self {
            shared: Rc::new(RefCell::new(Shared::new())),
            fiber: None,
            draining: Vec::new(),
            drawn: 0,
            api_id: TextField::default(),
            api_hash: TextField::default(),
            form: TextField::default(),
            form_stage: Stage::Starting,
            search: TextField::default(),
            input: TextField::default(),
            focus: Focus::ApiId,
            open: None,
            list_top: 0,
            scroll: 0,
            content_h: 0,
            newest: 0,
            hover_row: None,
            hover_button: None,
            pressed: None,
            menu: None,
            hits: Vec::new(),
            hover_hit: None,
            info: false,
            picker: false,
            viewer: None,
            context: None,
            toast: None,
            search_edit: 0,
            searched: String::new(),
        }
    }

    /// The window opened: start the client.
    pub fn start(&mut self) {
        if self.fiber.is_none() {
            crate::net::init();
            self.shared = Rc::new(RefCell::new(Shared::new()));
            self.open = None;
            self.searched.clear();
            let shared = self.shared.clone();
            self.fiber = Some(Fiber::new(move || tg::client::run(shared)));
            crate::serial::write_str("\ntelegram: started\n");
        }
    }

    /// The window closed: disconnect.
    pub fn stop(&mut self) {
        if let Some(mut f) = self.fiber.take() {
            f.cancel();
            self.draining.push(f);
        }
        self.viewer = None;
        self.context = None;
        self.info = false;
        self.picker = false;
    }

    /// Let the client run a little. True when the window must be drawn
    /// again.
    pub fn tick(&mut self) -> bool {
        self.draining.retain_mut(|f| !f.resume());
        if let Some(f) = &mut self.fiber {
            if f.resume() {
                self.fiber = None;
            }
        }
        let mut redraw = false;
        let goto = self.shared.borrow_mut().goto.take();
        if let Some(peer) = goto {
            self.open_chat(peer);
            redraw = true;
        }
        let now = now_ms();
        if self.toast.as_ref().is_some_and(|t| now > t.1) {
            self.toast = None;
            redraw = true;
        }
        // search Telegram once the typing stops
        let q = self.search.string();
        if q != self.searched && now - self.search_edit > SEARCH_WAIT_MS {
            self.searched = q.clone();
            if self.stage() == Stage::Ready {
                self.command(Cmd::Search(q));
            }
        }
        let version = self.shared.borrow().version;
        redraw || version != self.drawn
    }

    /// Whether the client waits for an answer the user is waiting for.
    pub fn busy(&self) -> bool {
        !self.draining.is_empty() || self.shared.borrow().busy
    }

    fn command(&mut self, cmd: Cmd) {
        let mut s = self.shared.borrow_mut();
        s.commands.push_back(cmd);
        s.changed();
    }

    fn stage(&self) -> Stage {
        self.shared.borrow().stage.clone()
    }

    fn say(&mut self, note: &str) {
        self.toast = Some((String::from(note), now_ms() + TOAST_MS));
    }

    // ---- places -----------------------------------------------------------------------

    fn card() -> Rect {
        Rect::new((CLIENT_W - 420) / 2, 70, 420, 520)
    }

    fn field_rect(i: i32) -> Rect {
        let c = Self::card();
        Rect::new(c.x + 40, c.y + 290 + i * 70, c.w - 80, 36)
    }

    fn next_rect(&self) -> Rect {
        let c = Self::card();
        let fields = if self.stage() == Stage::Config { 2 } else { 1 };
        Rect::new(c.x + 40, c.y + 290 + fields * 70 + 4, c.w - 80, 42)
    }

    fn back_rect(&self) -> Rect {
        let n = self.next_rect();
        Rect::new(n.x, n.bottom() + 12, n.w, 30)
    }

    fn search_rect() -> Rect {
        Rect::new(58, 11, LIST_W - 72, 34)
    }

    fn menu_button() -> Rect {
        Rect::new(10, 10, 38, 36)
    }

    fn menu_items() -> [widgets::Item<'static>; 3] {
        [
            ("Reload chats", "", true),
            ("", "", false),
            ("Log out", "", true),
        ]
    }

    fn menu_rect() -> Rect {
        widgets::menu_rect(10, 50, &Self::menu_items())
    }

    fn chat_area() -> Rect {
        Rect::new(
            LIST_W + 1,
            TOP_H,
            CLIENT_W - LIST_W - 1,
            CLIENT_H - TOP_H - INPUT_H,
        )
    }

    fn input_rect() -> Rect {
        Rect::new(
            LIST_W + 16,
            CLIENT_H - INPUT_H + 10,
            CLIENT_W - LIST_W - 124,
            36,
        )
    }

    fn emoji_rect() -> Rect {
        Rect::new(CLIENT_W - 96, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn send_rect() -> Rect {
        Rect::new(CLIENT_W - 52, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn join_rect() -> Rect {
        Rect::new(
            LIST_W + 1,
            CLIENT_H - INPUT_H + 1,
            CLIENT_W - LIST_W - 1,
            INPUT_H - 1,
        )
    }

    fn picker_rect() -> Rect {
        let rows = (emoji::PICKER.len() as i32 + PICK_COLS - 1) / PICK_COLS;
        let w = PICK_COLS * PICK_CELL + 16;
        let h = rows * PICK_CELL + 16;
        Rect::new(CLIENT_W - 12 - w, CLIENT_H - INPUT_H - 6 - h, w, h)
    }

    fn info_rect() -> Rect {
        let a = Self::chat_area();
        Rect::new(a.right() - 316, a.y + 10, 300, 330)
    }

    /// The open chat as it can be used: can we write, are we in it.
    fn open_state(&self) -> Option<(bool, bool)> {
        let peer = self.open?;
        let s = self.shared.borrow();
        let c = s.chat(peer)?;
        Some((c.member, c.can_post))
    }

    /// The rows of the chat list, top down: (top, height, row).
    fn rows(&self) -> Vec<(i32, i32, Row)> {
        let s = self.shared.borrow();
        let q = self.search.string().to_lowercase();
        let q = q.trim();
        let bare = q.trim_start_matches('@');
        let mut out = Vec::new();
        let mut y = 0;
        for (i, c) in s.chats.iter().enumerate() {
            let hit = q.is_empty()
                || c.title.to_lowercase().contains(q)
                || (!bare.is_empty() && c.username.to_lowercase().contains(bare));
            if hit {
                out.push((y, ROW_H, Row::Chat(i)));
                y += ROW_H;
            }
        }
        if !q.is_empty() && !s.found.is_empty() {
            out.push((y, HEADER_H, Row::Header));
            y += HEADER_H;
            for i in 0..s.found.len() {
                out.push((y, ROW_H, Row::Found(i)));
                y += ROW_H;
            }
        }
        out
    }

    /// The top of the list, under the note about the connection.
    fn list_y(&self) -> i32 {
        let s = self.shared.borrow();
        if !s.online || s.error.is_some() {
            TOP_H + 30
        } else {
            TOP_H
        }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<Row> {
        let top = self.list_y();
        if x >= LIST_W || y < top {
            return None;
        }
        let y = y - top + self.list_top;
        self.rows()
            .into_iter()
            .find(|(ry, h, _)| y >= *ry && y < ry + h)
            .map(|r| r.2)
            .filter(|r| *r != Row::Header)
    }

    fn row_peer(&self, row: Row) -> Option<Peer> {
        let s = self.shared.borrow();
        match row {
            Row::Chat(i) => s.chats.get(i).map(|c| c.peer),
            Row::Found(i) => s.found.get(i).map(|c| c.peer),
            Row::Header => None,
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // the last drawn is on top
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    // ---- input ------------------------------------------------------------------------

    pub fn on_key(&mut self, key: Key) -> bool {
        let stage = self.stage();
        match stage {
            Stage::Starting => false,
            Stage::Config => {
                if matches!(key, Key::Char('\t')) {
                    self.focus = if self.focus == Focus::ApiId {
                        Focus::ApiHash
                    } else {
                        Focus::ApiId
                    };
                    return true;
                }
                let field = if self.focus == Focus::ApiHash {
                    &mut self.api_hash
                } else {
                    &mut self.api_id
                };
                match field.on_key(key) {
                    FieldEvent::Enter => {
                        if self.focus == Focus::ApiId {
                            self.focus = Focus::ApiHash;
                        } else {
                            self.submit();
                        }
                        true
                    }
                    FieldEvent::None => false,
                    _ => true,
                }
            }
            Stage::Phone | Stage::Code(_) | Stage::Password(_) => match self.form.on_key(key) {
                FieldEvent::Enter => {
                    self.submit();
                    true
                }
                FieldEvent::Escape if !matches!(stage, Stage::Phone) => {
                    self.command(Cmd::Phone(String::new()));
                    true
                }
                FieldEvent::None => false,
                _ => true,
            },
            Stage::Ready => self.ready_key(key),
        }
    }

    fn ready_key(&mut self, key: Key) -> bool {
        if self.menu.is_some() || self.context.is_some() {
            self.menu = None;
            self.context = None;
            return true;
        }
        if self.viewer.is_some() {
            if matches!(key, Key::Escape | Key::Char(' ') | Key::Char('\n')) {
                self.viewer = None;
            }
            return true;
        }
        match key {
            Key::Escape if self.picker => {
                self.picker = false;
                return true;
            }
            Key::Escape if self.info => {
                self.info = false;
                return true;
            }
            Key::Escape if self.focus == Focus::Search && !self.search.text.is_empty() => {
                self.search.set("");
                self.list_top = 0;
                return true;
            }
            Key::Escape if self.open.is_some() => {
                self.close_chat();
                return true;
            }
            Key::PageUp => return self.on_wheel(-8),
            Key::PageDown => return self.on_wheel(8),
            _ => {}
        }
        if self.focus == Focus::Search {
            let event = self.search.on_key(key);
            if event == FieldEvent::Changed {
                self.search_edit = now_ms();
                self.list_top = 0;
            }
            if event == FieldEvent::Enter {
                self.search_enter();
            }
            return event != FieldEvent::None;
        }
        if self.open.is_none() || self.open_state().is_some_and(|(m, p)| !m || !p) {
            return false;
        }
        self.focus = Focus::Input;
        match self.input.on_key(key) {
            FieldEvent::Enter => {
                self.send();
                true
            }
            FieldEvent::None => false,
            _ => true,
        }
    }

    /// Enter in the search box: a @username is looked up, otherwise the
    /// first chat found opens.
    fn search_enter(&mut self) {
        let q = String::from(self.search.string().trim());
        if q.starts_with('@') && q.len() > 1 {
            self.command(Cmd::Resolve(String::from(&q[1..])));
            return;
        }
        let first = self
            .rows()
            .into_iter()
            .find(|r| r.2 != Row::Header)
            .and_then(|r| self.row_peer(r.2));
        match first {
            Some(peer) => self.open_chat(peer),
            None if !q.is_empty() && !q.contains(' ') => self.command(Cmd::Resolve(q)),
            None => {}
        }
    }

    fn submit(&mut self) {
        let cmd = match self.stage() {
            Stage::Config => Cmd::Config {
                api_id: self.api_id.string(),
                api_hash: self.api_hash.string(),
            },
            Stage::Phone => Cmd::Phone(self.form.string()),
            Stage::Code(_) => Cmd::Code(self.form.string()),
            Stage::Password(_) => Cmd::Password(self.form.string()),
            _ => return,
        };
        let mut s = self.shared.borrow_mut();
        s.busy = true;
        s.error = None;
        s.commands.push_back(cmd);
        s.changed();
    }

    fn send(&mut self) {
        let text = self.input.string();
        let Some(peer) = self.open else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        self.input.set("");
        self.scroll = 0;
        self.picker = false;
        self.command(Cmd::Send(peer, String::from(text.trim())));
    }

    fn open_chat(&mut self, peer: Peer) {
        if self.open != Some(peer) {
            self.input.set("");
            self.info = false;
        }
        self.open = Some(peer);
        self.shared.borrow_mut().open = Some(peer);
        self.scroll = 0;
        self.content_h = 0;
        self.newest = 0;
        self.focus = Focus::Input;
        self.command(Cmd::Open(peer));
    }

    fn close_chat(&mut self) {
        self.open = None;
        self.info = false;
        self.picker = false;
        self.shared.borrow_mut().open = None;
    }

    /// A link was clicked: usernames and t.me links open the chat here,
    /// other addresses open in the browser.
    fn follow(&mut self, target: &str) {
        if let Some(name) = target.strip_prefix('@') {
            self.command(Cmd::Resolve(String::from(name)));
            return;
        }
        let lower = target.to_ascii_lowercase();
        let bare = lower
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        for host in ["t.me/", "telegram.me/", "telegram.dog/"] {
            if let Some(path) = bare.strip_prefix(host) {
                let skip = target.len() - path.len();
                let first = target[skip..].split(['/', '?', '#']).next().unwrap_or("");
                let special = first.is_empty()
                    || first.starts_with('+')
                    || matches!(
                        first,
                        "joinchat" | "addstickers" | "addemoji" | "share" | "proxy" | "socks"
                    );
                if !special {
                    self.command(Cmd::Resolve(String::from(first)));
                    return;
                }
            }
        }
        let address = if lower.starts_with("http://") || lower.starts_with("https://") {
            String::from(target)
        } else {
            alloc::format!("http://{}", target)
        };
        super::request_address(&address);
        super::request_open(App::Browser);
        self.say("Opening in the browser");
    }

    /// A file card was clicked: save it, or show it once saved.
    fn file_clicked(&mut self, msg: i64, file: i64) {
        let Some(peer) = self.open else {
            return;
        };
        let state = self.shared.borrow().downloads.get(&file).cloned();
        match state {
            Some(Download::Going(_)) => {}
            Some(Download::Done(path)) => {
                let lower = path.to_ascii_lowercase();
                if [".txt", ".md", ".log", ".csv", ".ini", ".conf", ".json"]
                    .iter()
                    .any(|e| lower.ends_with(e))
                {
                    super::request_file(&path);
                    super::request_open(App::Notepad);
                } else {
                    super::request_folder(&crate::fs::parent(&path));
                    super::request_open(App::Explorer);
                }
            }
            _ => self.command(Cmd::Download(peer, msg)),
        }
    }

    fn copy(&mut self, text: &str, note: &str) {
        widgets::copy(text);
        self.say(note);
    }

    /// The right click menu for a message.
    fn message_menu(&mut self, id: i64, x: i32, y: i32) {
        let Some(peer) = self.open else {
            return;
        };
        let mut items: Vec<(String, Act)> = Vec::new();
        {
            let s = self.shared.borrow();
            let Some(m) = s
                .history
                .get(&peer)
                .and_then(|h| h.messages.iter().find(|m| m.id == id))
            else {
                return;
            };
            if !m.text.is_empty() {
                items.push((String::from("Copy text"), Act::Copy(m.text.clone())));
            }
            for l in m.links.iter().take(2) {
                let shortened: String = l.target.chars().take(24).collect();
                items.push((
                    alloc::format!("Open {}", shortened),
                    Act::Follow(l.target.clone()),
                ));
                let copy = if l.target.starts_with('@') {
                    "Copy username"
                } else {
                    "Copy link address"
                };
                items.push((String::from(copy), Act::Copy(l.target.clone())));
            }
            let username = s.chat(peer).map(|c| c.username.clone()).unwrap_or_default();
            if !username.is_empty() && matches!(peer, Peer::Channel(_)) && m.id != 0 {
                items.push((
                    String::from("Copy message link"),
                    Act::Copy(alloc::format!("https://t.me/{}/{}", username, m.id)),
                ));
            }
            if m.file.is_some() || m.photo.is_some() {
                items.push((String::from("Save to Downloads"), Act::Save(m.id)));
            }
        }
        if !items.is_empty() {
            self.context = Some(ContextMenu {
                x,
                y,
                items,
                hover: None,
            });
        }
    }

    fn act(&mut self, act: Act) {
        match act {
            Act::Copy(text) => self.copy(&text, "Copied to the clipboard"),
            Act::Follow(target) => self.follow(&target),
            Act::Save(id) => {
                if let Some(peer) = self.open {
                    self.command(Cmd::Download(peer, id));
                    self.say("Saving to Downloads");
                }
            }
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let (x, y) = (ev.x, ev.y);
        let right = match ev.kind {
            MouseKind::Down { right } => right,
            MouseKind::Up => {
                let pressed = self.pressed.take();
                if pressed.is_some() && pressed == self.button_at(x, y) {
                    match pressed {
                        Some(Button::Next) => self.submit(),
                        Some(Button::Send) => self.send(),
                        Some(Button::Back) => self.command(Cmd::Phone(String::new())),
                        Some(Button::Emoji) => self.picker = !self.picker,
                        Some(Button::Join) => {
                            if let Some(peer) = self.open {
                                self.command(Cmd::Join(peer));
                            }
                        }
                        _ => {}
                    }
                }
                return pressed.is_some();
            }
            _ => return false,
        };
        if let Some(menu) = self.context.take() {
            let labels = menu.labels();
            let hit = widgets::menu_item_at(menu.rect(), &labels, x, y);
            if let Some(i) = hit {
                let act = menu.items[i].1.clone();
                self.act(act);
            }
            return true;
        }
        if let Some(hover) = self.menu {
            self.menu = None;
            let items = Self::menu_items();
            match widgets::menu_item_at(Self::menu_rect(), &items, x, y).or(hover) {
                Some(0) => self.command(Cmd::Reload),
                Some(2) => {
                    self.close_chat();
                    self.command(Cmd::LogOut);
                }
                _ => {}
            }
            return true;
        }
        if let Some((_, msg)) = self.viewer {
            // "Save" at the top, anywhere else closes
            if right || Self::viewer_save().contains(x, y) {
                self.act(Act::Save(msg));
            }
            self.viewer = None;
            return true;
        }
        let stage = self.stage();
        match stage {
            Stage::Config => {
                for (i, focus) in [(0, Focus::ApiId), (1, Focus::ApiHash)] {
                    let r = Self::field_rect(i);
                    if r.contains(x, y) {
                        self.focus = focus;
                        let field = if i == 0 {
                            &mut self.api_id
                        } else {
                            &mut self.api_hash
                        };
                        field.click(r, x);
                        return true;
                    }
                }
            }
            Stage::Phone | Stage::Code(_) | Stage::Password(_) => {
                let r = Self::field_rect(0);
                if r.contains(x, y) {
                    self.form.click(r, x);
                    return true;
                }
            }
            Stage::Ready => {
                if self.ready_click(x, y, right) {
                    return true;
                }
            }
            Stage::Starting => {}
        }
        if right {
            return false;
        }
        if let Some(b) = self.button_at(x, y) {
            self.pressed = Some(b);
            return true;
        }
        true
    }

    fn ready_click(&mut self, x: i32, y: i32, right: bool) -> bool {
        if right {
            if let Some(Hit::Message(id)) = self
                .hits
                .iter()
                .rev()
                .find(|(r, h)| r.contains(x, y) && matches!(h, Hit::Message(_)))
                .map(|(_, h)| h.clone())
            {
                self.message_menu(id, x, y);
                return true;
            }
            return false;
        }
        if self.picker {
            if let Some(Hit::Emoji(e)) = self.hit_at(x, y) {
                self.focus = Focus::Input;
                for c in e.chars() {
                    self.input.on_key(Key::Char(c));
                }
                return true;
            }
            if !Self::picker_rect().contains(x, y) && !Self::emoji_rect().contains(x, y) {
                self.picker = false;
            }
        }
        if Self::menu_button().contains(x, y) {
            self.menu = Some(None);
            return true;
        }
        if Self::search_rect().contains(x, y) {
            self.focus = Focus::Search;
            self.search.click(Self::search_rect(), x);
            return true;
        }
        if let Some(row) = self.row_at(x, y) {
            if let Some(peer) = self.row_peer(row) {
                self.open_chat(peer);
            }
            return true;
        }
        if let Some(hit) = self.hit_at(x, y) {
            match hit {
                Hit::Link(target) => self.follow(&target),
                Hit::Photo(photo, msg) => {
                    let loaded = self
                        .shared
                        .borrow()
                        .photos
                        .get(&photo)
                        .is_some_and(|p| p.is_some());
                    if loaded {
                        self.viewer = Some((photo, msg));
                    }
                }
                Hit::File(msg, file) => self.file_clicked(msg, file),
                Hit::Header => self.info = !self.info,
                Hit::CloseInfo => self.info = false,
                Hit::Copy(text) => self.copy(&text, "Copied to the clipboard"),
                Hit::Join => {
                    if let Some(peer) = self.open {
                        self.command(Cmd::Join(peer));
                    }
                }
                Hit::Leave => {
                    if let Some(peer) = self.open {
                        self.command(Cmd::Leave(peer));
                        self.info = false;
                    }
                }
                Hit::Message(_) | Hit::Emoji(_) => {
                    if self.open.is_some() {
                        self.focus = Focus::Input;
                    }
                    return false;
                }
            }
            return true;
        }
        if Self::input_rect().contains(x, y) && self.open.is_some() {
            self.focus = Focus::Input;
            self.input.click(Self::input_rect(), x);
            return true;
        }
        if self.info && !Self::info_rect().contains(x, y) && x > LIST_W {
            self.info = false;
            return true;
        }
        if self.open.is_some() && x > LIST_W {
            self.focus = Focus::Input;
        }
        false
    }

    fn button_at(&self, x: i32, y: i32) -> Option<Button> {
        match self.stage() {
            Stage::Config | Stage::Phone | Stage::Code(_) | Stage::Password(_) => {
                if self.next_rect().contains(x, y) {
                    return Some(Button::Next);
                }
                let back = !matches!(self.stage(), Stage::Config | Stage::Phone);
                if back && self.back_rect().contains(x, y) {
                    return Some(Button::Back);
                }
                None
            }
            Stage::Ready => {
                if Self::menu_button().contains(x, y) {
                    return Some(Button::Menu);
                }
                let (member, can_post) = self.open_state()?;
                if !member {
                    return Self::join_rect().contains(x, y).then_some(Button::Join);
                }
                if !can_post {
                    None
                } else if Self::send_rect().contains(x, y) {
                    Some(Button::Send)
                } else if Self::emoji_rect().contains(x, y) {
                    Some(Button::Emoji)
                } else {
                    None
                }
            }
            Stage::Starting => None,
        }
    }

    pub fn on_wheel(&mut self, delta: i32) -> bool {
        if self.stage() != Stage::Ready || self.viewer.is_some() {
            return false;
        }
        if self.hover_row.is_some() || self.open.is_none() {
            let rows = self.rows();
            let total = rows.last().map_or(0, |r| r.0 + r.1);
            let view = CLIENT_H - self.list_y();
            let top = (self.list_top + delta.signum() * ROW_H * 2).clamp(0, (total - view).max(0));
            let changed = top != self.list_top;
            self.list_top = top;
            return changed;
        }
        // up (negative) goes back in time
        self.scroll = (self.scroll - delta * 3 * LINE_H).max(0);
        true
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let ready = self.stage() == Stage::Ready;
        let row = if ready { self.row_at(x, y) } else { None };
        let button = self.button_at(x, y);
        let on_list = x < LIST_W && y > TOP_H;
        let mut changed =
            row != self.hover_row && !(row.is_none() && on_list) || button != self.hover_button;
        self.hover_row = row.or(if on_list { Some(Row::Header) } else { None });
        self.hover_button = button;
        let hit = if ready {
            self.hit_at(x, y).filter(|h| !matches!(h, Hit::Message(_)))
        } else {
            None
        };
        if hit != self.hover_hit {
            self.hover_hit = hit;
            changed = true;
        }
        if let Some(menu) = self.menu {
            let items = Self::menu_items();
            let h = widgets::menu_item_at(Self::menu_rect(), &items, x, y);
            if h != menu {
                self.menu = Some(h);
                changed = true;
            }
        }
        if let Some(ctx) = &mut self.context {
            let h = widgets::menu_item_at(ctx.rect(), &ctx.labels(), x, y);
            if h != ctx.hover {
                ctx.hover = h;
                changed = true;
            }
        }
        changed
    }

    // ---- drawing ------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, focused: bool, caret: bool) {
        self.drawn = self.shared.borrow().version;
        let stage = self.stage();
        if stage != self.form_stage {
            // a new step: an empty box, ready to type in
            if !matches!(
                (&stage, &self.form_stage),
                (Stage::Code(_), Stage::Code(_)) | (Stage::Password(_), Stage::Password(_))
            ) {
                self.form.set("");
            }
            if stage == Stage::Config {
                self.focus = Focus::ApiId;
            }
            if stage == Stage::Ready && self.focus != Focus::Input {
                self.focus = Focus::Search;
            }
            self.form_stage = stage.clone();
        }
        self.hits.clear();
        match stage {
            Stage::Ready => self.draw_main(c, focused && caret),
            _ => self.draw_form(c, &stage, focused && caret),
        }
    }

    fn draw_form(&mut self, c: &mut Canvas, stage: &Stage, caret: bool) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, panel());
        let card = Self::card();
        let cx = CLIENT_W / 2;
        draw_logo(c, cx, card.y + 70, 56);
        let (title, lines): (&str, Vec<String>) = match stage {
            Stage::Starting => ("Telegram", alloc::vec![String::from("Connecting...")]),
            Stage::Config => (
                "Your Telegram app",
                alloc::vec![
                    String::from("EverOS needs an api_id and api_hash to talk to"),
                    String::from("Telegram. Get yours at my.telegram.org: API"),
                    String::from("development tools. They stay on this computer."),
                ],
            ),
            Stage::Phone => (
                "Your phone number",
                alloc::vec![
                    String::from("Enter your phone number with the country code."),
                    String::from("Telegram will send you a login code."),
                ],
            ),
            Stage::Code(hint) => (
                "Enter the code",
                wrap(&UI, hint, card.w - 60)
                    .into_iter()
                    .map(|l| l.0)
                    .collect(),
            ),
            Stage::Password(hint) => {
                let mut l = alloc::vec![String::from("Your account is protected with a password.")];
                if !hint.is_empty() {
                    l.push(alloc::format!("Hint: {}", clean(hint)));
                }
                ("Two-step verification", l)
            }
            Stage::Ready => return,
        };
        let tw = HEADING.width(title);
        c.draw_text_in(&HEADING, cx - tw / 2, card.y + 140, title, text());
        let mut y = card.y + 186;
        for l in &lines {
            let w = width(&UI, l);
            put(c, &UI, cx - w / 2, y, l, dim());
            y += LINE_H;
        }
        let (error, busy) = {
            let s = self.shared.borrow();
            (s.error.clone(), s.busy)
        };
        if *stage == Stage::Starting {
            if let Some(e) = error {
                let mut y = card.y + 290;
                for (l, _) in wrap(&UI, &e, card.w) {
                    let w = UI.width(&l);
                    c.draw_text(cx - w / 2, y, &l, theme::error());
                    y += LINE_H;
                }
            }
            return;
        }
        let labels: &[(&str, bool)] = match stage {
            Stage::Config => &[("api_id", false), ("api_hash", false)],
            Stage::Phone => &[("Phone number", false)],
            Stage::Code(_) => &[("Code", false)],
            _ => &[("Password", true)],
        };
        for (i, (label, secret)) in labels.iter().enumerate() {
            let r = Self::field_rect(i as i32);
            c.draw_text(r.x, r.y - 22, label, dim());
            let field = match (stage, i) {
                (Stage::Config, 0) => &mut self.api_id,
                (Stage::Config, _) => &mut self.api_hash,
                _ => &mut self.form,
            };
            let focused = match stage {
                Stage::Config => (i == 0) == (self.focus == Focus::ApiId),
                _ => true,
            };
            if *secret {
                // dots instead of the letters
                let mut shown = field.clone();
                shown.text = alloc::vec!['\u{2022}'; field.text.len()];
                shown.draw(c, r, focused, caret && focused);
            } else {
                field.draw(c, r, focused, caret && focused);
                if field.text.is_empty() && !focused {
                    c.draw_text(r.x + 8, r.y + 9, label, dim());
                }
            }
        }
        let next = self.next_rect();
        let label = if busy { "Please wait..." } else { "Next" };
        let face = if self.hover_button == Some(Button::Next) {
            mix(BLUE, rgb(0, 0, 0), 20)
        } else {
            BLUE
        };
        c.fill_round(next, 8, face);
        c.text_centered_in(&UI_BOLD, next, label, rgb(0xff, 0xff, 0xff));
        if !matches!(stage, Stage::Config | Stage::Phone) {
            let back = self.back_rect();
            c.text_centered(back, "Use a different number", BLUE);
        }
        if let Some(e) = error {
            let mut y = self.back_rect().bottom() + 8;
            for (l, _) in wrap(&UI, &e, card.w) {
                let w = UI.width(&l);
                c.draw_text(cx - w / 2, y, &l, theme::error());
                y += LINE_H;
            }
        }
    }

    fn draw_main(&mut self, c: &mut Canvas, caret: bool) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, panel());
        self.draw_list(c, caret);
        c.fill_rect(LIST_W, 0, 1, CLIENT_H, line());
        match self.open {
            Some(peer) => self.draw_chat(c, peer, caret),
            None => {
                let area = Rect::new(LIST_W + 1, 0, CLIENT_W - LIST_W - 1, CLIENT_H);
                c.vertical_gradient(area, wall_top(), wall_bottom());
                draw_pill(
                    c,
                    area.x + area.w / 2,
                    area.y + area.h / 2,
                    "Select a chat to start messaging",
                );
            }
        }
        if self.picker && self.open.is_some() {
            self.draw_picker(c);
        }
        if let Some((photo, _)) = self.viewer {
            self.draw_viewer(c, photo);
        }
        if let Some((note, _)) = &self.toast {
            let a = Self::chat_area();
            let (cx, cy) = if self.open.is_some() {
                (a.x + a.w / 2, a.bottom() - 30)
            } else {
                (CLIENT_W / 2, CLIENT_H - 40)
            };
            draw_toast(c, cx, cy, note);
        }
        if let Some(hover) = self.menu {
            let items = Self::menu_items();
            widgets::draw_menu(c, Self::menu_rect(), &items, hover);
        }
        if let Some(ctx) = &self.context {
            widgets::draw_menu(c, ctx.rect(), &ctx.labels(), ctx.hover);
        }
    }

    fn draw_list(&mut self, c: &mut Canvas, caret: bool) {
        // the menu button: three lines
        let m = Self::menu_button();
        if self.hover_button == Some(Button::Menu) {
            c.fill_round(m, 18, hover());
        }
        for i in 0..3 {
            c.fill_round(Rect::new(m.x + 10, m.y + 11 + i * 6, 18, 2), 1, dim());
        }
        let search_focused = self.focus == Focus::Search;
        let sr = Self::search_rect();
        c.fill_round(sr, 17, pick(rgb(0xf1, 0xf1, 0xf1), rgb(0x24, 0x2f, 0x3d)));
        if search_focused {
            c.outline_round(sr, 17, BLUE);
        }
        if self.search.text.is_empty() {
            c.draw_text(sr.x + 14, sr.y + 9, "Search or @username", dim());
            if search_focused && caret {
                c.fill_rect(sr.x + 14, sr.y + 8, 1, 18, text());
            }
        } else {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(sr.inset(4));
            let s = clean(&self.search.string());
            let w = put(&mut sub, &UI, sr.x + 14, sr.y + 9, &s, text());
            if search_focused && caret {
                let before: String = self.search.text[..self.search.cursor].iter().collect();
                let cw = width(&UI, &clean(&before)).min(w);
                sub.fill_rect(sr.x + 15 + cw, sr.y + 8, 1, 18, text());
            }
        }

        let (now, tz, online, error) = {
            let s = self.shared.borrow();
            (now_ms() / 1000, s.tz, s.online, s.error.clone())
        };
        let now = now + tz;
        let rows = self.rows();
        let top = self.list_y();
        let status = if !online {
            Some(String::from("Connecting..."))
        } else {
            error
        };
        if let Some(e) = status {
            let r = Rect::new(0, TOP_H, LIST_W, 30);
            c.fill(r, pick(rgb(0xff, 0xf4, 0xe0), rgb(0x2a, 0x2a, 0x1c)));
            c.draw_text(12, TOP_H + 7, &fit(&UI, &clean(&e), LIST_W - 24), dim());
        }
        let s = self.shared.borrow();
        let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
        sub.clip_to(Rect::new(0, top, LIST_W, CLIENT_H - top));
        let c = &mut sub;
        for &(ry, rh, row) in &rows {
            let y = top + ry - self.list_top;
            if y + rh < top {
                continue;
            }
            if y >= CLIENT_H {
                break;
            }
            let chat = match row {
                Row::Header => {
                    c.fill(Rect::new(0, y, LIST_W, rh), hover());
                    c.draw_text_in(&UI_BOLD, 14, y + 6, "Global search", dim());
                    continue;
                }
                Row::Chat(i) => &s.chats[i],
                Row::Found(i) => &s.found[i],
            };
            let is_open = self.open == Some(chat.peer);
            let r = Rect::new(0, y, LIST_W, ROW_H);
            if is_open {
                c.fill(r, selected());
            } else if self.hover_row == Some(row) {
                c.fill(r, hover());
            }
            let (fg, sub_fg) = if is_open {
                (rgb(0xff, 0xff, 0xff), rgb(0xe8, 0xf2, 0xfa))
            } else {
                (text(), dim())
            };
            draw_avatar(
                c,
                10 + AVATAR / 2,
                y + ROW_H / 2,
                AVATAR / 2,
                &chat.title,
                peer_id(chat.peer),
                chat.kind,
            );
            let tx = 10 + AVATAR + 12;
            let date = if matches!(row, Row::Found(_)) {
                String::new()
            } else {
                short_date(chat.date + tz, now)
            };
            let dw = UI.width(&date);
            c.draw_text(LIST_W - 12 - dw, y + 13, &date, sub_fg);
            let mut name_x = tx;
            if chat.kind == ChatKind::Channel || chat.kind == ChatKind::Group {
                draw_group_mark(c, tx, y + 14, fg, chat.kind == ChatKind::Channel);
                name_x += 20;
            }
            let title = fit(&UI_BOLD, &clean(&chat.title), LIST_W - 12 - dw - 8 - name_x);
            put(c, &UI_BOLD, name_x, y + 12, &title, fg);
            if let Row::Found(_) = row {
                // who it is instead of the last message
                let mut line = String::new();
                if !chat.username.is_empty() {
                    line.push('@');
                    line.push_str(&chat.username);
                }
                let what = members_text(chat);
                if !what.is_empty() {
                    if !line.is_empty() {
                        line.push_str(", ");
                    }
                    line.push_str(&what);
                }
                let line = fit(&UI, &clean(&line), LIST_W - 12 - tx);
                put(c, &UI, tx, y + 38, &line, sub_fg);
                continue;
            }
            // the last message and the unread count
            let mut right = LIST_W - 12;
            if chat.unread > 0 {
                let n = if chat.unread > 999 {
                    String::from("999+")
                } else {
                    alloc::format!("{}", chat.unread)
                };
                let w = (UI_BOLD.width(&n) + 14).max(22);
                let badge = Rect::new(right - w, y + 36, w, 22);
                let color = if is_open {
                    rgb(0xff, 0xff, 0xff)
                } else {
                    pick(rgb(0x4f, 0xae, 0x4e), rgb(0x3e, 0x88, 0xc7))
                };
                c.fill_round(badge, 11, color);
                c.text_centered_in(
                    &UI_BOLD,
                    badge,
                    &n,
                    if is_open { BLUE } else { rgb(0xff, 0xff, 0xff) },
                );
                right -= w + 6;
            }
            let mut px = tx;
            if chat.last_out && chat.kind != ChatKind::Saved {
                let you = "You: ";
                px += c.draw_text(px, y + 38, you, if is_open { fg } else { BLUE });
            }
            let last = fit(&UI, &clean(&chat.last), right - px);
            put(c, &UI, px, y + 38, &last, sub_fg);
        }
        if rows.is_empty() {
            let searching = !self.search.text.is_empty();
            let msg = if s.chats.is_empty() && !searching {
                "Loading chats..."
            } else if searching && self.searched != self.search.string() {
                "Searching..."
            } else {
                "No chats found"
            };
            let w = UI.width(msg);
            c.draw_text((LIST_W - w) / 2, top + 40, msg, dim());
        }
    }

    fn draw_chat(&mut self, c: &mut Canvas, peer: Peer, caret: bool) {
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let s = self.shared.borrow();
        let chat = s.chat(peer).cloned();
        let (title, kind) = chat
            .as_ref()
            .map(|c| (c.title.clone(), c.kind))
            .unwrap_or((String::new(), ChatKind::Private));
        let read_out = chat.as_ref().map_or(0, |c| c.read_out);
        let member = chat.as_ref().is_none_or(|c| c.member);
        let can_post = chat.as_ref().is_none_or(|c| c.can_post);
        let tz = s.tz;

        // the header: a click shows the details
        let head = Rect::new(LIST_W + 1, 0, CLIENT_W - LIST_W - 1, TOP_H);
        c.fill(head, panel());
        if self.hover_hit == Some(Hit::Header) {
            c.fill(head, hover());
        }
        hits.push((head, Hit::Header));
        put(
            c,
            &UI_BOLD,
            head.x + 20,
            10,
            &fit(&UI_BOLD, &clean(&title), head.w - 40),
            text(),
        );
        let mut subtitle = String::from(match kind {
            ChatKind::Saved => "your cloud storage",
            ChatKind::Bot => "bot",
            ChatKind::Group => "group",
            ChatKind::Channel => "channel",
            ChatKind::Private => "private chat",
        });
        if let Some(ch) = &chat {
            let members = members_text(ch);
            if !members.is_empty() {
                subtitle = members;
            }
            if !ch.username.is_empty() {
                subtitle.push_str("  \u{2022}  @");
                subtitle.push_str(&ch.username);
            }
        }
        c.draw_text(head.x + 20, 30, &fit(&UI, &subtitle, head.w - 40), dim());
        c.fill_rect(head.x, TOP_H - 1, head.w, 1, line());

        // the messages
        let area = Self::chat_area();
        c.vertical_gradient(area, wall_top(), wall_bottom());
        let empty = History::default();
        let h = s.history.get(&peer).unwrap_or(&empty);
        let group = matches!(kind, ChatKind::Group);
        let max_w = BUBBLE_MAX.min(area.w * 7 / 10);
        let laid = layout(&h.messages, group, max_w, tz);
        let total = laid.last().map_or(0, |l| l.y + l.h) + 12;
        // keep the view still when new messages come while scrolled up
        let newest = h.messages.last().map_or(0, |m| m.id.max(m.date));
        if self.content_h != 0 && self.scroll > 0 && newest != self.newest && total > self.content_h
        {
            self.scroll += total - self.content_h;
        }
        self.content_h = total;
        self.newest = newest;
        let view = area.h;
        self.scroll = self.scroll.min((total - view).max(0));
        let wants_older = !h.complete && !h.loading && self.scroll + view + 200 > total;
        // everything is laid out top down; the bottom of it sits at the
        // bottom of the area, moved down by the scroll
        let base = area.bottom() - total + self.scroll;
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(area);
            let c = &mut sub;
            if h.loading && h.messages.is_empty() {
                draw_pill(c, area.x + area.w / 2, area.y + area.h / 2, "Loading...");
            } else if h.messages.is_empty() && h.complete {
                draw_pill(
                    c,
                    area.x + area.w / 2,
                    area.y + area.h / 2,
                    "No messages here yet",
                );
            }
            for l in &laid {
                let y = base + l.y;
                if y + l.h < area.y || y > area.bottom() {
                    continue;
                }
                match &l.kind {
                    LaidKind::Date(d) => draw_pill(c, area.x + area.w / 2, y + 14, d),
                    LaidKind::Service(t) => draw_pill(c, area.x + area.w / 2, y + 14, t),
                    LaidKind::Bubble { index, w, .. } => {
                        let m = &h.messages[*index];
                        let x = if m.out {
                            area.right() - 16 - w
                        } else {
                            area.x + 16
                        };
                        let look = Look {
                            read_out,
                            tz,
                            s: &s,
                            hover: self.hover_hit.as_ref(),
                        };
                        draw_bubble(c, m, x, y, l, &look, &mut hits);
                    }
                }
            }
            if h.loading && !h.messages.is_empty() {
                draw_pill(c, area.x + area.w / 2, area.y + 20, "Loading...");
            }
        }
        // only what is in the area can be clicked
        hits.retain(|(r, h)| matches!(h, Hit::Header) || r.intersect(&area) == *r);
        if self.info {
            if let Some(ch) = &chat {
                draw_info(c, ch, &mut hits, self.hover_hit.as_ref());
            }
        }
        drop(s);
        if wants_older {
            self.command(Cmd::Older(peer));
        }

        // the box to write in, or a button to join
        let bar = Self::join_rect();
        c.fill(Rect::new(bar.x, bar.y - 1, bar.w, bar.h + 1), panel());
        c.fill_rect(bar.x, bar.y - 1, bar.w, 1, line());
        if !member {
            if self.hover_button == Some(Button::Join) {
                c.fill(bar, hover());
            }
            let label = if kind == ChatKind::Channel {
                "JOIN CHANNEL"
            } else {
                "JOIN GROUP"
            };
            c.text_centered_in(&UI_BOLD, bar, label, BLUE);
        } else if !can_post {
            c.text_centered(bar, "Only admins can post in this channel", dim());
        } else {
            self.draw_input(c, caret);
        }
        self.hits.extend(hits);
    }

    fn draw_input(&mut self, c: &mut Canvas, caret: bool) {
        let ir = Self::input_rect();
        let focused = self.focus == Focus::Input;
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(ir);
            if self.input.text.is_empty() {
                sub.draw_text(ir.x + 4, ir.y + 9, "Write a message...", dim());
                if focused && caret {
                    sub.fill_rect(ir.x + 4, ir.y + 8, 1, 18, text());
                }
            } else {
                // the end of long text stays in view
                let shown = clean(&self.input.string());
                let w = width(&UI, &shown);
                let x = ir.x + 4 - (w - (ir.w - 12)).max(0);
                put(&mut sub, &UI, x, ir.y + 9, &shown, text());
                if focused && caret {
                    let before: String = self.input.text[..self.input.cursor].iter().collect();
                    let cw = width(&UI, &clean(&before));
                    sub.fill_rect(x + cw, ir.y + 8, 1, 18, text());
                }
            }
        }
        // the emoji button: a smiling face
        let er = Self::emoji_rect();
        if self.hover_button == Some(Button::Emoji) || self.picker {
            c.fill_round(er, 20, hover());
        }
        draw_smile(
            c,
            er.x + 20,
            er.y + 20,
            if self.picker { BLUE } else { dim() },
        );
        let sr = Self::send_rect();
        let active = !self.input.text.is_empty();
        let color = if active { BLUE } else { dim() };
        if self.hover_button == Some(Button::Send) {
            c.fill_round(sr, 20, hover());
        }
        draw_plane(c, sr.x + 8, sr.y + 10, 24, color);
    }

    fn draw_picker(&mut self, c: &mut Canvas) {
        let r = Self::picker_rect();
        c.shadow(r, 10, 10, 3, 80);
        c.fill_round(r, 10, panel());
        for (i, e) in emoji::PICKER.iter().enumerate() {
            let (col, row) = (i as i32 % PICK_COLS, i as i32 / PICK_COLS);
            let cell = Rect::new(
                r.x + 8 + col * PICK_CELL,
                r.y + 8 + row * PICK_CELL,
                PICK_CELL,
                PICK_CELL,
            );
            if self.hover_hit == Some(Hit::Emoji(e)) {
                c.fill_round(cell, 6, hover());
            }
            let shown = clean(e);
            let w = width(&UI, &shown);
            put(
                c,
                &UI,
                cell.x + (PICK_CELL - w) / 2,
                cell.y + 5,
                &shown,
                text(),
            );
            self.hits.push((cell, Hit::Emoji(e)));
        }
    }

    fn viewer_save() -> Rect {
        Rect::new(CLIENT_W - 120, 12, 100, 32)
    }

    fn draw_viewer(&mut self, c: &mut Canvas, photo: i64) {
        c.fill_round_alpha(Rect::new(0, 0, CLIENT_W, CLIENT_H), 0, rgb(0, 0, 0), 220);
        let s = self.shared.borrow();
        if let Some(Some(p)) = s.photos.get(&photo) {
            let (mw, mh) = (CLIENT_W - 80, CLIENT_H - 100);
            // as big as fits, but at most twice its size
            let scale_w = (mw * 100 / p.w).min(mh * 100 / p.h).min(200);
            let (w, h) = (p.w * scale_w / 100, p.h * scale_w / 100);
            let dst = Rect::new((CLIENT_W - w) / 2, (CLIENT_H - h) / 2 + 10, w, h);
            if w >= p.w {
                c.blit_scaled(dst, &p.pixels, p.w, p.h, 256);
            } else {
                c.blit_smooth(dst, &p.pixels, p.w, p.h);
            }
        }
        let save = Self::viewer_save();
        c.fill_round(save, 16, rgb(0x33, 0x33, 0x33));
        c.text_centered(save, "Save", rgb(0xff, 0xff, 0xff));
        c.draw_text(20, 20, "Click anywhere to close", rgb(0xcc, 0xcc, 0xcc));
    }
}

fn peer_id(p: Peer) -> i64 {
    match p {
        Peer::User(id) | Peer::Chat(id) | Peer::Channel(id) => id,
    }
}

/// "1 234 subscribers", "5 members"; empty when not known.
fn members_text(c: &tg::Chat) -> String {
    if c.members <= 0 {
        return String::new();
    }
    let what = match (c.kind, c.members) {
        (ChatKind::Channel, 1) => "subscriber",
        (ChatKind::Channel, _) => "subscribers",
        (_, 1) => "member",
        _ => "members",
    };
    alloc::format!("{} {}", count_text(c.members), what)
}

/// What drawing a message needs to know besides the message.
struct Look<'a> {
    read_out: i64,
    tz: i64,
    s: &'a Shared,
    hover: Option<&'a Hit>,
}

/// How big a photo is shown.
fn photo_size(p: &tg::Photo, max_w: i32) -> (i32, i32) {
    let mut w = p.w.min(PHOTO_W).min(max_w - 8);
    let mut h = w * p.h / p.w.max(1);
    if h > PHOTO_H {
        h = PHOTO_H;
        w = (h * p.w / p.h.max(1)).max(120);
    }
    (w.max(120), h.max(60))
}

/// Lay out the messages top down, with date lines between days.
fn layout(messages: &[Message], group: bool, max_w: i32, tz: i64) -> Vec<Laid> {
    let mut out = Vec::new();
    let mut y = 8;
    let mut last_day = i64::MIN;
    let mut last_from = i64::MIN;
    let now = now_ms() / 1000 + tz;
    for (i, m) in messages.iter().enumerate() {
        let day = (m.date + tz).div_euclid(86400);
        if day != last_day {
            last_day = day;
            last_from = i64::MIN;
            out.push(Laid {
                y,
                h: 28,
                kind: LaidKind::Date(long_date(m.date + tz, now)),
            });
            y += 36;
        }
        if m.service {
            out.push(Laid {
                y,
                h: 28,
                kind: LaidKind::Service(fit(&UI, &clean(&m.text), max_w)),
            });
            y += 36;
            last_from = i64::MIN;
            continue;
        }
        let photo = m.photo.as_ref().map(|p| photo_size(p, max_w));
        let file = m.file.is_some();
        // a label for what is attached and not shown otherwise
        let mut label = String::new();
        if let (Some(media), None, false) = (&m.media, &photo, file) {
            label.push('[');
            label.push_str(media);
            label.push(']');
            if !m.text.is_empty() {
                label.push('\n');
            }
        }
        let (body, map) = shown(&m.text);
        let (label_shown, _) = shown(&label);
        let text = alloc::format!("{}{}", label_shown, body);
        let offset = label_shown.chars().count();
        let links: Vec<(usize, usize, String)> = m
            .links
            .iter()
            .map(|l| {
                let at = |i: usize| map[i.min(map.len() - 1)] + offset;
                (at(l.start), at(l.end), l.target.clone())
            })
            .collect();
        let wrap_w = match photo {
            Some((pw, _)) => pw + 8 - 2 * PAD_X,
            None => max_w - 2 * PAD_X,
        };
        let lines = if text.is_empty() {
            Vec::new()
        } else {
            wrap(&UI, &text, wrap_w)
        };
        let name = (group && !m.out && m.from_id != last_from).then(|| clean(&m.from));
        let time_w = time_width(m);
        let widest = lines.iter().map(|l| width(&UI, &l.0)).max().unwrap_or(0);
        let last_w = lines.last().map_or(0, |l| width(&UI, &l.0));
        // a photo alone has its time on the picture
        let time_below =
            !lines.is_empty() && last_w + 12 + time_w > wrap_w || file && lines.is_empty();
        let mut w = widest.max(if time_below || lines.is_empty() {
            time_w
        } else {
            last_w + 12 + time_w
        });
        if let Some(n) = &name {
            w = w.max(width(&UI_BOLD, n).min(max_w - 2 * PAD_X));
        }
        if file {
            w = w.max(250);
        }
        let mut w = w + 2 * PAD_X;
        let mut h = lines.len() as i32 * LINE_H + 2 * PAD_Y;
        if time_below {
            h += LINE_H - 4;
        }
        if name.is_some() {
            h += LINE_H;
        }
        if let Some((pw, ph)) = photo {
            w = pw + 8;
            h += ph + 4;
            if lines.is_empty() {
                // no text: the picture fills the bubble
                h = ph + 8 + if name.is_some() { LINE_H + 4 } else { 0 };
            }
        }
        if file {
            h += FILE_H + 4;
        }
        // messages in a row from the same person sit closer
        let gap = if m.from_id == last_from { 4 } else { 10 };
        last_from = m.from_id;
        out.push(Laid {
            y,
            h,
            kind: LaidKind::Bubble {
                index: i,
                lines,
                links,
                w,
                name,
                time_below,
                photo,
                file,
            },
        });
        y += h + gap;
    }
    out
}

fn time_width(m: &Message) -> i32 {
    let mut w = UI.width("00:00");
    if m.edited {
        w += UI.width("edited ");
    }
    if m.out {
        w += 20;
    }
    w
}

fn draw_bubble(
    c: &mut Canvas,
    m: &Message,
    x: i32,
    y: i32,
    l: &Laid,
    look: &Look,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let LaidKind::Bubble {
        lines,
        links,
        w,
        name,
        time_below,
        photo,
        file,
        ..
    } = &l.kind
    else {
        return;
    };
    let (w, h) = (*w, l.h);
    let r = Rect::new(x, y, w, h);
    let face = if m.out { bubble_out() } else { bubble_in() };
    c.shadow(r, 10, 2, 1, 30);
    c.fill_round(r, 10, face);
    hits.push((r, Hit::Message(m.id)));
    let mut ty = y + PAD_Y;
    if let Some(n) = name {
        let n = fit(&UI_BOLD, n, w - 2 * PAD_X);
        put(c, &UI_BOLD, x + PAD_X, ty, &n, palette(m.from_id));
        ty += LINE_H;
    }
    if let (Some((pw, ph)), Some(p)) = (photo, &m.photo) {
        let pr = Rect::new(x + 4, if name.is_some() { ty } else { y + 4 }, *pw, *ph);
        match look.s.photos.get(&p.loc.id) {
            Some(Some(pic)) => {
                if pic.w > pr.w {
                    c.blit_smooth(pr, &pic.pixels, pic.w, pic.h);
                } else {
                    c.blit_scaled(pr, &pic.pixels, pic.w, pic.h, 256);
                }
            }
            Some(None) => {
                c.fill_round(pr, 8, pick(rgb(0xdd, 0xdd, 0xdd), rgb(0x22, 0x2e, 0x3a)));
                c.text_centered(pr, "The photo could not be loaded", dim());
            }
            None => {
                c.fill_round(pr, 8, pick(rgb(0xe4, 0xe9, 0xe4), rgb(0x22, 0x2e, 0x3a)));
                c.text_centered(pr, "Loading photo...", dim());
            }
        }
        hits.push((pr, Hit::Photo(p.loc.id, m.id)));
        ty = pr.bottom() + 4;
        if lines.is_empty() {
            // the time on the picture, in a dark pill
            let mut time = clock(m.date + look.tz);
            if m.edited {
                time = alloc::format!("edited {}", time);
            }
            let tw = UI.width(&time) + 12;
            let t = Rect::new(pr.right() - tw - 6, pr.bottom() - 26, tw, 20);
            c.fill_round_alpha(t, 10, rgb(0, 0, 0), 110);
            c.text_centered(t, &time, rgb(0xff, 0xff, 0xff));
            return;
        }
    }
    if let (true, Some(d)) = (*file, &m.file) {
        let card = Rect::new(x + PAD_X, ty, w - 2 * PAD_X, FILE_H);
        draw_file(c, card, d, m.out, look);
        hits.push((card, Hit::File(m.id, d.loc.id)));
        ty += FILE_H + 4;
    }
    let body = if m.failed { theme::error() } else { text() };
    let labelled = m.media.is_some() && photo.is_none() && !*file;
    for (i, (l, start)) in lines.iter().enumerate() {
        // the [Photo] line of a message with something attached
        let color = if i == 0 && labelled && l.starts_with('[') {
            if m.out {
                time_out()
            } else {
                BLUE
            }
        } else {
            body
        };
        draw_line(
            c,
            x + PAD_X,
            ty,
            l,
            *start,
            links,
            color,
            m.out,
            look.hover,
            hits,
        );
        ty += LINE_H;
    }
    // the time, and ticks for our messages
    let tcolor = if m.out { time_out() } else { time_in() };
    let mut time = String::new();
    if m.edited {
        time.push_str("edited ");
    }
    time.push_str(&clock(m.date + look.tz));
    let tw = UI.width(&time) + if m.out { 20 } else { 0 };
    let tx = x + w - PAD_X - tw;
    let tyy = if *time_below { ty - 2 } else { ty - LINE_H };
    c.draw_text(tx, tyy, &time, tcolor);
    if m.out {
        let cx = x + w - PAD_X - 16;
        let cy = tyy + 5;
        if m.failed {
            c.draw_text_in(&UI_BOLD, cx + 4, tyy, "!", theme::error());
        } else if m.id == 0 {
            // a small clock: still sending
            c.outline_round(Rect::new(cx + 2, cy, 11, 11), 5, tcolor);
            c.fill_rect(cx + 7, cy + 2, 1, 4, tcolor);
            c.fill_rect(cx + 7, cy + 5, 3, 1, tcolor);
        } else {
            draw_tick(c, cx, cy + 1, tcolor);
            if m.id <= look.read_out {
                draw_tick(c, cx + 5, cy + 1, tcolor);
            }
        }
    }
}

/// One line of a message, with its links in colour (underlined under
/// the mouse), each of them clickable.
#[allow(clippy::too_many_arguments)]
fn draw_line(
    c: &mut Canvas,
    x: i32,
    y: i32,
    line: &str,
    start: usize,
    links: &[(usize, usize, String)],
    color: Color,
    out: bool,
    hover: Option<&Hit>,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let link_at = |i: usize| links.iter().find(|l| i >= l.0 && i < l.1);
    let mut pen = x;
    let mut run = String::new();
    let mut run_link: Option<&(usize, usize, String)> = None;
    let chars: Vec<char> = line.chars().collect();
    let mut flush =
        |c: &mut Canvas, pen: &mut i32, run: &mut String, link: Option<&(usize, usize, String)>| {
            if run.is_empty() {
                return;
            }
            let col = if link.is_some() {
                link_color(out)
            } else {
                color
            };
            let w = put(c, &UI, *pen, y, run, col);
            if let Some(l) = link {
                let hit = Hit::Link(l.2.clone());
                if hover == Some(&hit) {
                    c.fill_rect(*pen, y + 17, w, 1, col);
                }
                hits.push((Rect::new(*pen, y, w, LINE_H), hit));
            }
            *pen += w;
            run.clear();
        };
    for (k, ch) in chars.iter().enumerate() {
        let l = link_at(start + k);
        if l != run_link {
            flush(c, &mut pen, &mut run, run_link);
            run_link = l;
        }
        run.push(*ch);
    }
    flush(c, &mut pen, &mut run, run_link);
}

/// A file in a message: a round button with an arrow (or a check once
/// saved), the name and the size or how far saving is.
fn draw_file(c: &mut Canvas, r: Rect, d: &tg::Document, out: bool, look: &Look) {
    let accent = if out { time_out() } else { BLUE };
    let state = look.s.downloads.get(&d.loc.id);
    let circle = Rect::new(r.x, r.y + 3, 44, 44);
    c.fill_round(circle, 22, accent);
    let white = rgb(0xff, 0xff, 0xff);
    let (cx, cy) = (circle.x + 22, circle.y + 22);
    match state {
        Some(Download::Done(_)) => {
            // a check mark
            for d in 0..3 {
                c.line(cx - 9, cy + d - 1, cx - 3, cy + 5 + d, white);
                c.line(cx - 3, cy + 5 + d, cx + 9, cy - 7 + d, white);
            }
        }
        Some(Download::Going(p)) => {
            // how much, as a bar across the circle
            let bar = Rect::new(cx - 12, cy - 2, 24, 4);
            c.fill_round(bar, 2, mix(accent, white, 50));
            c.fill_round(
                Rect::new(bar.x, bar.y, (24 * *p as i32 / 1000).max(2), 4),
                2,
                white,
            );
        }
        _ => {
            // an arrow down
            c.fill_rect(cx - 1, cy - 10, 3, 14, white);
            c.fill_polygon(&[(cx - 8, cy + 1), (cx + 9, cy + 1), (cx, cy + 10)], white);
        }
    }
    let tx = r.x + 56;
    let name = fit(&UI_BOLD, &clean(&d.name), r.right() - tx);
    put(c, &UI_BOLD, tx, r.y + 6, &name, text());
    let status = match state {
        Some(Download::Going(p)) => alloc::format!(
            "{} of {}",
            size_text(d.size * *p as i64 / 1000),
            size_text(d.size)
        ),
        Some(Download::Done(_)) => {
            alloc::format!(
                "{}  \u{2022}  Saved to Downloads, click to open",
                size_text(d.size)
            )
        }
        Some(Download::Failed(why)) => why.clone(),
        None => alloc::format!("{}  \u{2022}  Click to save", size_text(d.size)),
    };
    let color = if matches!(state, Some(Download::Failed(_))) {
        theme::error()
    } else if out {
        time_out()
    } else {
        dim()
    };
    c.draw_text(tx, r.y + 26, &fit(&UI, &status, r.right() - tx), color);
}

/// The chat's details: name, username and link to copy, join or leave.
fn draw_info(c: &mut Canvas, chat: &tg::Chat, hits: &mut Vec<(Rect, Hit)>, hover: Option<&Hit>) {
    let r = Telegram::info_rect();
    c.shadow(r, 12, 12, 4, 90);
    c.fill_round(r, 12, panel());
    hits.push((r, Hit::Message(0)));
    let close = Rect::new(r.right() - 36, r.y + 8, 28, 28);
    if hover == Some(&Hit::CloseInfo) {
        c.fill_round(close, 14, hover_color());
    }
    draw_cross(c, close, dim());
    hits.push((close, Hit::CloseInfo));
    let cx = r.x + r.w / 2;
    draw_avatar(
        c,
        cx,
        r.y + 60,
        40,
        &chat.title,
        peer_id(chat.peer),
        chat.kind,
    );
    let title = fit(&UI_BOLD, &clean(&chat.title), r.w - 30);
    let tw = width(&UI_BOLD, &title);
    put(c, &UI_BOLD, cx - tw / 2, r.y + 110, &title, text());
    let mut what = members_text(chat);
    if what.is_empty() {
        what = String::from(match chat.kind {
            ChatKind::Channel => "channel",
            ChatKind::Group => "group",
            ChatKind::Bot => "bot",
            ChatKind::Saved => "your cloud storage",
            ChatKind::Private => "user",
        });
    }
    let ww = UI.width(&what);
    c.draw_text(cx - ww / 2, r.y + 132, &what, dim());
    let mut y = r.y + 166;
    if chat.username.is_empty() {
        c.draw_text(r.x + 20, y, "No username", dim());
    } else {
        let name = alloc::format!("@{}", chat.username);
        let link = alloc::format!("https://t.me/{}", chat.username);
        for (value, caption, copy) in [
            (name.clone(), "Username, click to copy", name.clone()),
            (
                alloc::format!("t.me/{}", chat.username),
                "Link, click to copy",
                link,
            ),
        ] {
            let row = Rect::new(r.x + 8, y - 4, r.w - 16, 44);
            let hit = Hit::Copy(copy);
            if hover == Some(&hit) {
                c.fill_round(row, 8, hover_color());
            }
            c.draw_text(r.x + 20, y, &fit(&UI, &value, r.w - 40), BLUE);
            c.draw_text(r.x + 20, y + 19, caption, dim());
            hits.push((row, hit));
            y += 48;
        }
    }
    if let Peer::Channel(_) = chat.peer {
        let b = Rect::new(r.x + 20, r.bottom() - 52, r.w - 40, 36);
        let (label, hit, color) = if chat.member {
            ("Leave", Hit::Leave, theme::error())
        } else {
            ("Join", Hit::Join, BLUE)
        };
        if hover == Some(&hit) {
            c.fill_round(b, 8, hover_color());
        }
        c.outline_round(b, 8, color);
        c.text_centered_in(&UI_BOLD, b, label, color);
        hits.push((b, hit));
    }
}

fn hover_color() -> Color {
    hover()
}

fn draw_tick(c: &mut Canvas, x: i32, y: i32, color: Color) {
    for d in 0..2 {
        c.line(x, y + 5 + d, x + 3, y + 8 + d, color);
        c.line(x + 3, y + 8 + d, x + 10, y + d, color);
    }
}

fn draw_pill(c: &mut Canvas, cx: i32, cy: i32, text: &str) {
    let text = clean(text);
    let w = width(&UI, &text) + 20;
    let r = Rect::new(cx - w / 2, cy - 12, w, 24);
    c.fill_round(r, 12, pill());
    put(
        c,
        &UI,
        r.x + 10,
        r.y + (24 - UI.line_height) / 2,
        &text,
        rgb(0xff, 0xff, 0xff),
    );
}

/// A note over the chat that goes away by itself: white on dark, so it
/// reads over photos and light bubbles alike.
fn draw_toast(c: &mut Canvas, cx: i32, cy: i32, text: &str) {
    let text = clean(text);
    let w = width(&UI, &text) + 28;
    let r = Rect::new(cx - w / 2, cy - 15, w, 30);
    c.fill_round_alpha(r, 15, rgb(0, 0, 0), 200);
    put(
        c,
        &UI,
        r.x + 14,
        r.y + (30 - UI.line_height) / 2,
        &text,
        rgb(0xff, 0xff, 0xff),
    );
}

/// A close mark (the font has no ✕), two pixels thick.
fn draw_cross(c: &mut Canvas, r: Rect, color: Color) {
    let (cx, cy, h) = (r.x + r.w / 2, r.y + r.h / 2, 5);
    for d in 0..2 {
        c.line(cx - h + d, cy - h, cx + h + d, cy + h, color);
        c.line(cx + h + d, cy - h, cx - h + d, cy + h, color);
    }
}

fn draw_avatar(
    c: &mut Canvas,
    cx: i32,
    cy: i32,
    radius: i32,
    title: &str,
    id: i64,
    kind: ChatKind,
) {
    let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
    if kind == ChatKind::Saved {
        c.fill_round(r, radius, BLUE);
        // a bookmark
        let (w, h) = (radius * 7 / 10, radius);
        let (x, y) = (cx - w / 2, cy - h / 2);
        c.fill_polygon(
            &[
                (x, y),
                (x + w, y),
                (x + w, y + h),
                (x + w / 2, y + h * 7 / 10),
                (x, y + h),
            ],
            rgb(0xff, 0xff, 0xff),
        );
        return;
    }
    c.fill_round(r, radius, palette(id));
    let letters = initials(&clean(title));
    c.text_centered_in(&TITLE, r, &letters, rgb(0xff, 0xff, 0xff));
}

/// Two heads for groups, a loudspeaker for channels, before the name.
fn draw_group_mark(c: &mut Canvas, x: i32, y: i32, color: Color, channel: bool) {
    if channel {
        c.fill_polygon(
            &[
                (x, y + 5),
                (x + 5, y + 5),
                (x + 12, y),
                (x + 12, y + 14),
                (x + 5, y + 9),
                (x, y + 9),
            ],
            color,
        );
    } else {
        c.fill_round(Rect::new(x + 1, y + 1, 6, 6), 3, color);
        c.fill_round(Rect::new(x, y + 8, 8, 6), 3, color);
        c.fill_round(Rect::new(x + 8, y + 1, 6, 6), 3, color);
        c.fill_round(Rect::new(x + 7, y + 8, 8, 6), 3, color);
    }
}

/// A round smiling face for the emoji button.
fn draw_smile(c: &mut Canvas, cx: i32, cy: i32, color: Color) {
    let face = Rect::new(cx - 11, cy - 11, 22, 22);
    c.outline_round(face, 11, color);
    c.outline_round(face.inset(1), 10, color);
    c.fill_round(Rect::new(cx - 6, cy - 5, 3, 4), 1, color);
    c.fill_round(Rect::new(cx + 3, cy - 5, 3, 4), 1, color);
    for d in 0..2 {
        c.line(cx - 6, cy + 3 + d, cx - 2, cy + 6 + d, color);
        c.line(cx - 2, cy + 6 + d, cx + 2, cy + 6 + d, color);
        c.line(cx + 2, cy + 6 + d, cx + 6, cy + 3 + d, color);
    }
}

/// The paper plane, `s` pixels wide, at (x, y).
fn draw_plane(c: &mut Canvas, x: i32, y: i32, s: i32, color: Color) {
    let p = |fx: i32, fy: i32| (x + fx * s / 100, y + fy * s / 100);
    c.fill_polygon(
        &[
            p(0, 40),
            p(100, 0),
            p(80, 90),
            p(45, 62),
            p(35, 85),
            p(30, 55),
        ],
        color,
    );
    c.fill_polygon(
        &[p(30, 55), p(90, 8), p(45, 62)],
        mix(color, rgb(0, 0, 0), 40),
    );
}

/// The Telegram logo: a white plane on a blue circle.
fn draw_logo(c: &mut Canvas, cx: i32, cy: i32, radius: i32) {
    let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
    c.fill_round(r, radius, rgb(0x2a, 0xa3, 0xd8));
    let s = radius;
    let (x, y) = (cx - s / 2 - s / 12, cy - s / 3);
    let p = |fx: i32, fy: i32| (x + fx * s / 100, y + fy * s / 100);
    let white = rgb(0xff, 0xff, 0xff);
    c.fill_polygon(
        &[
            p(0, 44),
            p(92, 8),
            p(76, 88),
            p(48, 66),
            p(36, 84),
            p(33, 58),
        ],
        white,
    );
    c.fill_polygon(&[p(33, 58), p(82, 18), p(48, 66)], rgb(0xc8, 0xda, 0xea));
}
