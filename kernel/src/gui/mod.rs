//! The graphical desktop in the style of Windows 11: a background with a
//! bloom, icons, windows with rounded corners and soft shadows, a
//! centred taskbar with a start menu and a clock, and the mouse pointer.
//!
//! Everything is drawn into a back buffer in memory and then copied to
//! the screen, so nothing flickers. Only the areas that changed (the
//! "dirty" rectangles) are redrawn and copied.
//!
//! Before the desktop comes the sign-in screen (login.rs). Windows zoom
//! and fade when they open and close and fly to the taskbar when
//! minimised, the start menu slides up, and highlights fade in and out.
//! Animations follow the timer, and each frame redraws only what moves.

mod about;
mod anim;
mod browser;
mod calc;
mod canvas;
mod demo;
mod explorer;
mod filedialog;
#[rustfmt::skip]
mod font_data;
mod icons;
mod login;
mod notepad;
mod paint;
mod settings;
mod start;
mod terminal;
mod text;
mod theme;
mod tray;
#[rustfmt::skip]
pub mod webfont;
mod widgets;

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use anim::{lerp, Fader, Tween, ONE};
use canvas::{fast_mix, mix, rgb, Canvas, Dirty, Rect};
use icons::Icons;
use login::Login;
use start::StartMenu;
use tray::{Panel, Tray};

use crate::framebuffer::Framebuffer;
use crate::interrupts::{self, KEYBOARD_BYTES, MOUSE_BYTES};
use crate::keyboard::{Key, Keyboard, Layout};
use crate::multiboot::BootInfo;
use crate::sync::{ByteQueue, IrqMutex, StaticBuffer};
use crate::{console::CONSOLE, fs, port, ps2, rtc, serial, users, vmmouse, StackString};

const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static BACK_BUFFER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The desktop background, drawn once at start.
static WALLPAPER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// Window contents. Each app draws into its own part only when its content
/// changes, so moving a window just copies pixels.
static SURFACES: StaticBuffer<{ 6 * 1024 * 1024 }> = StaticBuffer::new();
/// The blurred wallpaper behind the sign-in panel.
static BACKDROP: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The screen being faded away when signing in or locking.
static SNAPSHOT: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// Where a zooming window or the sliding start menu is drawn before it
/// is scaled and blended onto the screen.
static SCRATCH: StaticBuffer<{ MAX_W * 1000 }> = StaticBuffer::new();

/// Whether the desktop is running (the shell asks before opening apps).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Apps to open or close, sent from the shell.
static REQUESTS: ByteQueue = ByteQueue::new();
const CLOSE: u8 = 0x80;
const LOCK: u8 = 0x40;

const TASKBAR_H: i32 = 48;
const TITLE_H: i32 = 32;
const BORDER: i32 = 1;
const WINDOW_RADIUS: i32 = 8;
/// How far window shadows reach.
const SPREAD: i32 = 16;
const SHADOW_DROP: i32 = 4;
const DOUBLE_CLICK_TICKS: u64 = interrupts::TIMER_HZ / 2;
const BLINK_TICKS: u64 = interrupts::TIMER_HZ / 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum App {
    Terminal,
    Explorer,
    Notepad,
    Paint,
    Calculator,
    Demo,
    Browser,
    Settings,
    About,
}

const APPS: [App; 9] = [
    App::Terminal,
    App::Explorer,
    App::Notepad,
    App::Paint,
    App::Calculator,
    App::Demo,
    App::Browser,
    App::Settings,
    App::About,
];

impl App {
    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            App::Explorer => "File Explorer",
            App::Notepad => "Notepad",
            App::Paint => "Paint",
            App::Calculator => "Calculator",
            App::Demo => "Graphics",
            App::Browser => "Browser",
            App::Settings => "Settings",
            App::About => "About EverOS",
        }
    }

    fn client_size(self) -> (i32, i32) {
        match self {
            App::Terminal => (terminal::CLIENT_W, terminal::CLIENT_H),
            App::Explorer => (explorer::CLIENT_W, explorer::CLIENT_H),
            App::Notepad => (notepad::CLIENT_W, notepad::CLIENT_H),
            App::Paint => (paint::CLIENT_W, paint::CLIENT_H),
            App::Calculator => (calc::CLIENT_W, calc::CLIENT_H),
            App::Demo => (demo::CLIENT_W, demo::CLIENT_H),
            App::Browser => (browser::CLIENT_W, browser::CLIENT_H),
            App::Settings => (settings::CLIENT_W, settings::CLIENT_H),
            App::About => (about::CLIENT_W, about::CLIENT_H),
        }
    }

    fn default_position(self) -> (i32, i32) {
        match self {
            App::Terminal => (240, 70),
            App::Explorer => (330, 110),
            App::Notepad => (520, 170),
            App::Paint => (520, 150),
            App::Calculator => (1440, 90),
            App::Demo => (760, 330),
            App::Browser => (200, 40),
            App::Settings => (420, 140),
            App::About => (680, 280),
        }
    }
}

/// Ask the desktop to open an app. Returns false in text mode.
pub fn request_open(app: App) -> bool {
    REQUESTS.push(app.index() as u8);
    ACTIVE.load(Ordering::Relaxed)
}

/// Ask the browser to go to an address (the shell's `browser` command).
pub fn request_address(address: &str) {
    browser::request_address(address);
}

/// A file for Notepad and a folder for File Explorer, from the shell.
static FILE_REQUEST: IrqMutex<Option<String>> = IrqMutex::new(None);
static FOLDER_REQUEST: IrqMutex<Option<String>> = IrqMutex::new(None);

/// Ask Notepad to open a file (the shell's `notepad` command).
pub fn request_file(path: &str) {
    *FILE_REQUEST.lock() = Some(String::from(path));
}

/// Ask File Explorer to show a folder (the shell's `explorer` command).
pub fn request_folder(path: &str) {
    *FOLDER_REQUEST.lock() = Some(String::from(path));
}

/// Ask the desktop to show the lock screen.
pub fn request_lock() -> bool {
    REQUESTS.push(LOCK);
    ACTIVE.load(Ordering::Relaxed)
}

/// Ask the desktop to close an app's window.
pub fn request_close(app: App) -> bool {
    REQUESTS.push(CLOSE | app.index() as u8);
    ACTIVE.load(Ordering::Relaxed)
}

/// A mouse event for an app, in its client coordinates.
#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub kind: MouseKind,
}

#[derive(Clone, Copy)]
pub enum MouseKind {
    Down {
        right: bool,
    },
    /// Moved with a button held.
    Move,
    Up,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    /// Opening or closing: grow or shrink a little and fade.
    Zoom,
    /// Flying to or from the taskbar button.
    Minimize,
}

#[derive(Clone, Copy)]
struct WindowAnim {
    motion: Motion,
    /// 0 is gone, ONE is fully there.
    tween: Tween,
}

#[derive(Clone, Copy)]
struct Window {
    /// Outer frame, including the title bar and border.
    rect: Rect,
    open: bool,
    minimized: bool,
    anim: Option<WindowAnim>,
}

impl Window {
    fn visible(&self) -> bool {
        self.open && !self.minimized
    }

    /// Whether it is on the screen, also while it animates away.
    fn drawn(&self) -> bool {
        self.visible() || self.anim.is_some()
    }

    fn client(&self) -> Rect {
        Rect::new(
            self.rect.x + BORDER,
            self.rect.y + TITLE_H,
            self.rect.w - 2 * BORDER,
            self.rect.h - TITLE_H - BORDER,
        )
    }

    fn title_bar(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLE_H)
    }

    fn close_button(&self) -> Rect {
        Rect::new(self.rect.right() - 46, self.rect.y, 46, TITLE_H)
    }

    fn minimize_button(&self) -> Rect {
        self.close_button().offset(-46, 0)
    }

    /// Everything the window draws on, shadow included.
    fn bounds(&self) -> Rect {
        shadow_bounds(self.rect)
    }
}

/// A frame and its shadow.
fn shadow_bounds(r: Rect) -> Rect {
    Rect::new(
        r.x - SPREAD,
        r.y - SPREAD,
        r.w + 2 * SPREAD,
        r.h + 2 * SPREAD + SHADOW_DROP,
    )
}

/// What the screen shows.
#[derive(Clone, Copy)]
enum Phase {
    /// The lock screen or the sign-in panel.
    Login,
    /// The sign-in screen (in SNAPSHOT) fading into the desktop.
    Unlocking(Tween),
    /// The desktop (in SNAPSHOT) fading into the lock screen.
    Locking(Tween),
    Desktop,
}

/// What is under the mouse and lights up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    Minimize(App),
    Close(App),
    /// A taskbar button: 0 is Start, then the apps.
    Slot(usize),
    /// A tray button: the layout, quick settings or the clock.
    Tray(usize),
    Quick(tray::Target),
}

pub struct Desktop<'a> {
    fb: Framebuffer,
    back: &'static mut [u32],
    wallpaper: &'static mut [u32],
    width: i32,
    height: i32,
    boot: &'a BootInfo,
    icons: &'static Icons,
    pointer_image: Pointer,

    windows: [Window; APPS.len()],
    /// Open windows from bottom to top.
    order: [App; APPS.len()],
    order_len: usize,
    focused: Option<App>,

    mouse_x: i32,
    mouse_y: i32,
    left: bool,
    right: bool,
    /// Window being moved, and where in its title bar it was grabbed.
    drag: Option<(App, i32, i32)>,
    /// App that got the button press and gets the moves until release.
    capture: Option<App>,
    hover: Fader<Hover>,
    start: StartMenu,
    /// How far the start menu is open, for its slide.
    menu: Tween,
    menu_moving: bool,
    tray: Tray,
    /// The open tray flyout, and the one last shown for its slide.
    panel: Option<Panel>,
    panel_shown: Panel,
    panel_anim: Tween,
    panel_moving: bool,
    /// The quick settings slider being dragged.
    slider: Option<tray::Slider>,
    /// Asks the main loop to switch the keyboard layout.
    toggle_layout: bool,
    /// Copy the whole back buffer to the screen next frame (brightness).
    present_all: bool,
    phase: Phase,
    login: Login,
    /// Who the open windows belong to.
    session_user: Option<usize>,
    /// Mouse buttons as the PS/2 mouse and the vmmouse last reported them.
    ps2_buttons: (bool, bool),
    vm_buttons: (bool, bool),
    /// Last vmmouse position, to tell moves from button-only events.
    vm_position: (u32, u32),
    selected_icon: Option<usize>,
    last_click: (u64, usize),

    layout: Layout,
    clock: StackString<16>,
    date: StackString<16>,
    cursor_on: bool,
    dirty: Dirty,
    snapshot: &'static mut [u32],
    scratch: &'static mut [u32],
    surfaces: [&'static mut [u32]; APPS.len()],
    /// Apps whose surface must be drawn again.
    stale: [bool; APPS.len()],

    terminal: terminal::Terminal,
    paint: paint::Paint,
    calc: calc::Calc,
    browser: Box<browser::Browser>,
    notepad: Box<notepad::Notepad>,
    explorer: Box<explorer::Explorer>,
    settings: settings::Settings,
    about: about::About,
    /// The window the mouse was last over, for hover highlights.
    hover_app: Option<App>,
}

impl<'a> Desktop<'a> {
    pub fn new(fb: Framebuffer, boot: &'a BootInfo) -> Self {
        let width = fb.width.min(MAX_W) as i32;
        let height = fb.height.min(MAX_H) as i32;
        let mut windows = [Window {
            rect: Rect::default(),
            open: false,
            minimized: false,
            anim: None,
        }; APPS.len()];
        for app in APPS {
            let (w, h) = app.client_size();
            let (x, y) = app.default_position();
            // keep windows on small screens
            let x = x.min(width - w - 2 * BORDER).max(0);
            let y = y.min(height - TASKBAR_H - h - TITLE_H - BORDER).max(0);
            windows[app.index()].rect = Rect::new(x, y, w + 2 * BORDER, h + TITLE_H + BORDER);
        }
        let mut pool = SURFACES.take();
        let surfaces = core::array::from_fn(|i| {
            let (w, h) = APPS[i].client_size();
            let (mine, rest) = core::mem::take(&mut pool).split_at_mut((w * h) as usize);
            pool = rest;
            mine
        });
        let wallpaper = WALLPAPER.take();
        draw_wallpaper(&mut Canvas::new(wallpaper, width as usize, height as usize));
        let login = Login::new(wallpaper, BACKDROP.take(), width, height);
        Self {
            fb,
            back: BACK_BUFFER.take(),
            wallpaper,
            width,
            height,
            boot,
            icons: icons::get(),
            pointer_image: Pointer::new(),
            windows,
            order: APPS,
            order_len: 0,
            focused: None,
            mouse_x: width / 2,
            mouse_y: height / 2,
            left: false,
            right: false,
            drag: None,
            capture: None,
            hover: Fader::new(anim::ms(120)),
            start: StartMenu::new(),
            menu: Tween::new(0, 0, 1),
            menu_moving: false,
            tray: Tray::new(),
            panel: None,
            panel_shown: Panel::Quick,
            panel_anim: Tween::new(0, 0, 1),
            panel_moving: false,
            slider: None,
            toggle_layout: false,
            present_all: false,
            phase: Phase::Login,
            login,
            session_user: None,
            ps2_buttons: (false, false),
            vm_buttons: (false, false),
            vm_position: (0, 0),
            selected_icon: None,
            last_click: (0, usize::MAX),
            layout: Layout::Us,
            clock: StackString::new(),
            date: StackString::new(),
            cursor_on: true,
            dirty: Dirty::default(),
            snapshot: SNAPSHOT.take(),
            scratch: SCRATCH.take(),
            surfaces,
            stale: [true; APPS.len()],
            terminal: terminal::Terminal::new(),
            paint: paint::Paint::new(),
            calc: calc::Calc::new(),
            browser: Box::new(browser::Browser::new()),
            notepad: Box::new(notepad::Notepad::new()),
            explorer: Box::new(explorer::Explorer::new()),
            settings: settings::Settings::new(),
            about: about::About::new(),
            hover_app: None,
        }
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn damage(&mut self, r: Rect) {
        self.dirty.add(r.intersect(&self.screen()));
    }

    fn damage_window(&mut self, app: App) {
        let w = self.windows[app.index()];
        if w.visible() {
            self.damage(w.bounds());
        }
    }

    // ---- animations -------------------------------------------------------

    /// Start a window moving towards `to` (0 gone, ONE there). An
    /// animation already running the same way turns around smoothly.
    fn animate(&mut self, app: App, motion: Motion, to: i32) {
        let duration = match (motion, to) {
            (Motion::Zoom, ONE) => anim::ms(200),
            (Motion::Zoom, _) => anim::ms(150),
            (Motion::Minimize, _) => anim::ms(260),
        };
        let w = &mut self.windows[app.index()];
        match &mut w.anim {
            Some(a) if a.motion == motion => a.tween.retarget(to, duration),
            _ => {
                let tween = Tween::new(ONE - to, to, duration);
                w.anim = Some(WindowAnim { motion, tween });
            }
        }
        self.damage(self.anim_envelope(app));
    }

    /// Where a window minimises to: a small frame over its taskbar button.
    fn minimized_rect(&self, app: App) -> Rect {
        let r = self.windows[app.index()].rect;
        let slot = self.slot_rect(app.index() + 1);
        let (w, h) = (r.w / 6, r.h / 6);
        Rect::new(
            slot.x + slot.w / 2 - w / 2,
            self.height - TASKBAR_H - h / 2,
            w,
            h,
        )
    }

    /// The frame of an animating window now, and how opaque it is.
    fn anim_frame(&self, app: App) -> Option<(Rect, i32)> {
        let w = &self.windows[app.index()];
        let a = w.anim?;
        let p = a.tween.value();
        let r = w.rect;
        Some(match a.motion {
            Motion::Zoom => {
                let scale = lerp(ONE * 92 / 100, ONE, p);
                let (nw, nh) = (r.w * scale / ONE, r.h * scale / ONE);
                let frame = Rect::new(r.x + (r.w - nw) / 2, r.y + (r.h - nh) / 2, nw, nh);
                (frame, p)
            }
            Motion::Minimize => {
                let t = self.minimized_rect(app);
                let frame = Rect::new(
                    lerp(t.x, r.x, p),
                    lerp(t.y, r.y, p),
                    lerp(t.w, r.w, p),
                    lerp(t.h, r.h, p),
                );
                // stay solid for most of the way
                (frame, (p * 2).min(ONE))
            }
        })
    }

    /// Everything an animating window may cover on its way.
    fn anim_envelope(&self, app: App) -> Rect {
        let w = &self.windows[app.index()];
        match w.anim {
            Some(a) if a.motion == Motion::Minimize => {
                w.bounds().union(&shadow_bounds(self.minimized_rect(app)))
            }
            _ => w.bounds(),
        }
    }

    /// Move every animation on by the time that has passed and mark
    /// what it changes.
    fn tick(&mut self) {
        match self.phase {
            Phase::Unlocking(t) | Phase::Locking(t) if t.done() => {
                self.phase = match self.phase {
                    Phase::Locking(_) => Phase::Login,
                    _ => Phase::Desktop,
                };
                self.damage(self.screen());
            }
            _ => {}
        }
        self.login.tick();
        let (rects, n) = self.login.dirty.take();
        if matches!(self.phase, Phase::Login | Phase::Locking(_)) {
            for r in &rects[..n] {
                self.damage(*r);
            }
        }

        for app in APPS {
            let Some(a) = self.windows[app.index()].anim else {
                continue;
            };
            self.damage(self.anim_envelope(app));
            if a.tween.done() {
                let w = &mut self.windows[app.index()];
                w.anim = None;
                if a.motion == Motion::Zoom && !w.open {
                    self.remove_from_order(app);
                }
            }
        }
        // one more frame once it stops, to draw where it ended
        let menu_moving = !self.menu.done();
        let menu_was_moving = core::mem::replace(&mut self.menu_moving, menu_moving);
        if menu_moving || menu_was_moving || self.start.tick() {
            let r = self.menu_rect();
            self.damage(r.union(&r.offset(0, 48)));
        }
        let panel_moving = !self.panel_anim.done();
        let panel_was_moving = core::mem::replace(&mut self.panel_moving, panel_moving);
        if panel_moving || panel_was_moving {
            let r = self.panel_rect(self.panel_shown).inset(-SPREAD);
            self.damage(r.union(&r.offset(0, 48)));
        }
        if self.hover.tick() {
            for h in self.hover.lit().into_iter().flatten() {
                self.damage(self.hover_rect(h));
            }
        }
    }

    // ---- signing in and out -----------------------------------------------

    /// Keep what the screen shows now, without the pointer, to fade from.
    fn take_snapshot(&mut self) {
        let mut back = core::mem::take(&mut self.back);
        let mut scratch = core::mem::take(&mut self.scratch);
        {
            let mut c = Canvas::new(back, self.width as usize, self.height as usize);
            self.draw_scene(&mut c, scratch);
        }
        let n = (self.width * self.height) as usize;
        self.snapshot[..n].copy_from_slice(&back[..n]);
        core::mem::swap(&mut self.back, &mut back);
        core::mem::swap(&mut self.scratch, &mut scratch);
    }

    fn signed_in(&mut self) {
        self.take_snapshot();
        let user = users::current();
        if let Some(name) = users::current_name() {
            fs::ensure_home(name.as_str());
        }
        if self.session_user != user {
            // someone else: start a fresh session
            self.close_all();
            self.session_user = user;
            *self.notepad = notepad::Notepad::new();
            *self.explorer = explorer::Explorer::new();
        }
        self.phase = Phase::Unlocking(Tween::new(0, ONE, anim::ms(450)));
        self.damage(self.screen());
        if self.order_len == 0 {
            self.open(App::Terminal);
        }
    }

    fn lock(&mut self, sign_out: bool) {
        if !matches!(self.phase, Phase::Desktop) {
            return;
        }
        self.start.open = false;
        self.menu = Tween::new(0, 0, 1);
        self.panel = None;
        self.panel_anim = Tween::new(0, 0, 1);
        self.slider = None;
        self.drag = None;
        self.capture = None;
        self.take_snapshot();
        if sign_out {
            users::sign_out();
            self.close_all();
            self.session_user = None;
        }
        self.login.lock();
        self.phase = Phase::Locking(Tween::new(0, ONE, anim::ms(450)));
        self.damage(self.screen());
    }

    /// Close every window at once, without animations.
    fn close_all(&mut self) {
        for w in &mut self.windows {
            w.open = false;
            w.minimized = false;
            w.anim = None;
        }
        self.order_len = 0;
        self.focused = None;
    }

    fn login_outcome(&mut self, outcome: login::Outcome) {
        match outcome {
            login::Outcome::None => {}
            login::Outcome::SignedIn => self.signed_in(),
            login::Outcome::Restart => restart(),
            login::Outcome::ShutDown => shut_down(),
        }
    }

    /// The app's content changed: draw its surface again.
    fn damage_client(&mut self, app: App) {
        self.stale[app.index()] = true;
        let w = self.windows[app.index()];
        if w.visible() {
            self.damage(w.client());
        }
    }

    /// An app changed. Notepad and File Explorer show the file or folder
    /// in the title bar, so their whole window is drawn again.
    fn app_changed(&mut self, app: App) {
        match app {
            App::Notepad | App::Explorer => {
                self.stale[app.index()] = true;
                self.damage_window(app);
            }
            _ => self.damage_client(app),
        }
    }

    /// The text in a window's title bar.
    fn window_title(&self, app: App) -> String {
        match app {
            App::Notepad => self.notepad.title(),
            App::Explorer => self.explorer.title(),
            _ => String::from(app.title()),
        }
    }

    /// Act on what Notepad and File Explorer ask for, and on files and
    /// folders the shell asked for.
    fn poll_apps(&mut self) {
        if core::mem::take(&mut self.notepad.want_close) {
            self.close(App::Notepad);
        }
        if self.windows[App::Explorer.index()].open && self.explorer.check_changes() {
            self.app_changed(App::Explorer);
        }
        if let Some(path) = self.explorer.open_request.take() {
            self.open_file(&path);
        }
        let file = FILE_REQUEST.lock().take();
        if let Some(path) = file {
            self.open_file(&path);
        }
        let folder = FOLDER_REQUEST.lock().take();
        if let Some(path) = folder {
            self.explorer.show(&path);
            self.open(App::Explorer);
            self.app_changed(App::Explorer);
        }
    }

    /// Open a file in Notepad.
    fn open_file(&mut self, path: &str) {
        self.open(App::Notepad);
        self.notepad.open_file(path);
        self.app_changed(App::Notepad);
    }

    // ---- window management ----------------------------------------------

    fn open(&mut self, app: App) {
        let w = &mut self.windows[app.index()];
        if !w.open {
            w.open = true;
            w.minimized = false;
            // for the boot test, which only sees the serial port
            serial::write_str("\ndesktop: opened ");
            serial::write_str(app.title());
            serial::write_str("\n");
            self.start.note_opened(app);
            self.animate(app, Motion::Zoom, ONE);
        } else if w.minimized {
            w.minimized = false;
            self.animate(app, Motion::Minimize, ONE);
        }
        if app == App::Browser {
            self.browser.start();
        }
        if app == App::Explorer {
            self.explorer.start();
            self.stale[app.index()] = true;
        }
        self.focus(app);
        self.damage_taskbar();
    }

    fn close(&mut self, app: App) {
        if !self.windows[app.index()].open {
            return;
        }
        // Notepad first asks about unsaved changes
        if app == App::Notepad && !self.notepad.try_close() {
            self.focus(app);
            self.app_changed(app);
            return;
        }
        self.damage_window(app);
        // it stays in the stacking order until it has faded out
        self.windows[app.index()].open = false;
        self.animate(app, Motion::Zoom, 0);
        if self.focused == Some(app) {
            self.focus_top();
        }
        self.damage_taskbar();
    }

    fn minimize(&mut self, app: App) {
        self.damage_window(app);
        self.windows[app.index()].minimized = true;
        self.animate(app, Motion::Minimize, 0);
        if self.focused == Some(app) {
            self.focus_top();
        }
        self.damage_taskbar();
    }

    fn remove_from_order(&mut self, app: App) {
        if let Some(i) = self.order[..self.order_len].iter().position(|&a| a == app) {
            self.order.copy_within(i + 1..self.order_len, i);
            self.order_len -= 1;
        }
    }

    /// Raise a window to the top and give it the keyboard.
    fn focus(&mut self, app: App) {
        if self.focused != Some(app) {
            if let Some(old) = self.focused {
                self.damage_window(old);
            }
            self.damage_taskbar();
        }
        self.remove_from_order(app);
        self.order[self.order_len] = app;
        self.order_len += 1;
        self.focused = Some(app);
        self.cursor_on = true;
        self.damage_window(app);
    }

    fn focus_top(&mut self) {
        self.focused = None;
        let top = self.order[..self.order_len]
            .iter()
            .rev()
            .find(|&&a| self.windows[a.index()].visible())
            .copied();
        if let Some(app) = top {
            self.focus(app);
        }
    }

    /// The topmost visible window under a point.
    fn window_at(&self, x: i32, y: i32) -> Option<App> {
        self.order[..self.order_len]
            .iter()
            .rev()
            .find(|a| {
                let w = &self.windows[a.index()];
                w.visible() && w.rect.contains(x, y)
            })
            .copied()
    }

    fn move_window(&mut self, app: App, x: i32, y: i32) {
        let w = self.windows[app.index()];
        // keep part of the title bar on the screen
        let x = x.clamp(80 - w.rect.w, self.width - 80);
        let y = y.clamp(0, self.height - TASKBAR_H - TITLE_H);
        if (x, y) != (w.rect.x, w.rect.y) {
            self.damage(w.bounds());
            self.windows[app.index()].rect.x = x;
            self.windows[app.index()].rect.y = y;
            self.damage_window(app);
        }
    }

    // ---- input -------------------------------------------------------------

    fn on_key(&mut self, key: Key) {
        match self.phase {
            Phase::Login => {
                let outcome = self.login.on_key(key);
                self.login_outcome(outcome);
                return;
            }
            Phase::Locking(_) => return,
            // typing can start while the desktop fades in
            Phase::Unlocking(_) | Phase::Desktop => {}
        }
        if let Key::LayoutChanged = key {
            self.damage_taskbar();
            self.damage_panel();
            self.damage_client(App::Settings);
            return;
        }
        if self.panel.is_some() {
            if let Key::Escape = key {
                self.close_panel();
                return;
            }
        }
        if self.start.open {
            match self.start.on_key(key) {
                start::Action::None => {}
                action => self.menu_action(action),
            }
            return;
        }
        if let Key::Super = key {
            self.open_menu();
            return;
        }
        let Some(app) = self.focused else {
            return;
        };
        let changed = match app {
            App::Terminal => {
                self.terminal.on_key(key, self.boot);
                self.cursor_on = true;
                false // the console reports its own changes
            }
            App::Calculator => self.calc.on_key(key),
            App::Browser => self.browser.on_key(key),
            App::Notepad => self.notepad.on_key(key),
            App::Explorer => self.explorer.on_key(key),
            App::Paint | App::Demo | App::Settings | App::About => false,
        };
        if changed {
            self.cursor_on = true;
            self.app_changed(app);
        }
    }

    /// A PS/2 mouse packet: relative movement.
    fn on_ps2(&mut self, packet: ps2::MousePacket) {
        self.ps2_buttons = (packet.left, packet.right);
        self.pointer(self.mouse_x + packet.dx, self.mouse_y + packet.dy);
    }

    /// A vmmouse event: an absolute position.
    fn on_vmmouse(&mut self, ev: vmmouse::Event) {
        self.vm_buttons = (
            ev.buttons & vmmouse::LEFT != 0,
            ev.buttons & vmmouse::RIGHT != 0,
        );
        // button-only events repeat the last position; skip those, so the
        // PS/2 mouse (as QEMU's monitor drives it) is not thrown back
        let (mut x, mut y) = (self.mouse_x, self.mouse_y);
        if (ev.x, ev.y) != self.vm_position {
            self.vm_position = (ev.x, ev.y);
            x = (ev.x as u64 * self.width as u64 / 65536) as i32;
            y = (ev.y as u64 * self.height as u64 / 65536) as i32;
        }
        self.pointer(x, y);
        if ev.wheel != 0 && matches!(self.phase, Phase::Desktop) {
            let app = self.window_at(self.mouse_x, self.mouse_y);
            let changed = match app {
                Some(App::Browser) => self.browser.on_wheel(ev.wheel),
                Some(App::Notepad) => self.notepad.on_wheel(ev.wheel),
                Some(App::Explorer) => self.explorer.on_wheel(ev.wheel),
                _ => false,
            };
            if let Some(app) = app.filter(|_| changed) {
                self.app_changed(app);
            }
        }
    }

    /// Move the pointer to a position and handle button changes.
    fn pointer(&mut self, x: i32, y: i32) {
        let (old_x, old_y) = (self.mouse_x, self.mouse_y);
        self.mouse_x = x.clamp(0, self.width - 1);
        self.mouse_y = y.clamp(0, self.height - 1);
        let moved = (old_x, old_y) != (self.mouse_x, self.mouse_y);
        if moved {
            self.damage(pointer_rect(old_x, old_y));
            self.damage(pointer_rect(self.mouse_x, self.mouse_y));
        }

        let (was_left, was_right) = (self.left, self.right);
        self.left = self.ps2_buttons.0 || self.vm_buttons.0;
        self.right = self.ps2_buttons.1 || self.vm_buttons.1;

        match self.phase {
            Phase::Desktop => {}
            Phase::Login => {
                self.login.on_move(self.mouse_x, self.mouse_y);
                if self.left && !was_left {
                    let outcome = self.login.on_click(self.mouse_x, self.mouse_y);
                    self.login_outcome(outcome);
                }
                return;
            }
            Phase::Unlocking(_) | Phase::Locking(_) => return,
        }

        if self.left && !was_left {
            self.press(false);
        } else if self.right && !was_right {
            self.press(true);
        } else if moved && (self.left || self.right) {
            self.held_move();
        } else if moved {
            self.hover_apps();
        }
        if (was_left && !self.left) || (was_right && !self.right) {
            self.release();
        }
        self.update_hover();
    }

    /// Tell the app under the mouse where it is, so it can light up what
    /// is under it (the browser shows where a link goes).
    fn hover_apps(&mut self) {
        let app = self.window_at(self.mouse_x, self.mouse_y);
        // the window the mouse left forgets its highlight
        if let Some(old) = self.hover_app.filter(|&a| Some(a) != app) {
            if self.app_hover(old, -1000, -1000) {
                self.app_changed(old);
            }
        }
        self.hover_app = app;
        if let Some(app) = app {
            let client = self.windows[app.index()].client();
            if self.app_hover(app, self.mouse_x - client.x, self.mouse_y - client.y) {
                self.app_changed(app);
            }
        }
    }

    fn app_hover(&mut self, app: App, x: i32, y: i32) -> bool {
        match app {
            App::Browser => self.browser.on_hover(x, y),
            App::Notepad => self.notepad.on_hover(x, y),
            App::Explorer => self.explorer.on_hover(x, y),
            _ => false,
        }
    }

    /// Light up whatever is under the mouse now.
    fn update_hover(&mut self) {
        let hover = if self.drag.is_some() {
            None
        } else {
            self.hover_at(self.mouse_x, self.mouse_y)
        };
        if self.hover.set(hover) {
            for h in self.hover.lit().into_iter().flatten() {
                self.damage(self.hover_rect(h));
            }
        }
        if self.start.open
            && self
                .start
                .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y)
        {
            self.damage(self.menu_rect());
        }
    }

    fn hover_at(&self, x: i32, y: i32) -> Option<Hover> {
        if self.start.open && self.menu_panel().contains(x, y) {
            return None;
        }
        if let Some(panel) = self.panel {
            let r = self.panel_rect(panel);
            if r.contains(x, y) {
                return match panel {
                    Panel::Quick => self.tray.target_at(r, x, y).map(Hover::Quick),
                    Panel::Calendar => None,
                };
            }
        }
        if y >= self.height - TASKBAR_H {
            if let Some(i) = (0..3).find(|&i| self.tray_rect(i).contains(x, y)) {
                return Some(Hover::Tray(i));
            }
            return (0..=APPS.len())
                .find(|&i| self.slot_rect(i).contains(x, y))
                .map(Hover::Slot);
        }
        let app = self.window_at(x, y)?;
        let w = self.windows[app.index()];
        if w.close_button().contains(x, y) {
            Some(Hover::Close(app))
        } else if w.minimize_button().contains(x, y) {
            Some(Hover::Minimize(app))
        } else {
            None
        }
    }

    fn hover_rect(&self, hover: Hover) -> Rect {
        match hover {
            Hover::Minimize(app) => self.windows[app.index()].minimize_button(),
            Hover::Close(app) => self.windows[app.index()].close_button(),
            Hover::Slot(i) => self.slot_rect(i),
            Hover::Tray(i) => self.tray_rect(i),
            Hover::Quick(_) => self.panel_rect(Panel::Quick),
        }
    }

    fn press(&mut self, right: bool) {
        let (x, y) = (self.mouse_x, self.mouse_y);
        if self.start.open {
            let panel = self.menu_panel();
            if panel.contains(x, y) {
                if !right {
                    let action = self.start.on_click(panel, x, y);
                    self.menu_action(action);
                }
                return;
            }
            let on_start = self.slot_rect(0).contains(x, y);
            self.close_menu();
            if on_start {
                return;
            }
        }
        if let Some(panel) = self.panel {
            let r = self.panel_rect(panel);
            if r.contains(x, y) {
                if !right && panel == Panel::Quick {
                    self.quick_click(r, x, y);
                }
                return;
            }
            // a click on the button that opened it only closes it
            let own = self.tray_rect(if panel == Panel::Quick { 1 } else { 2 });
            self.close_panel();
            if own.contains(x, y) {
                return;
            }
        }
        if y >= self.height - TASKBAR_H {
            if !right {
                self.taskbar_click(x, y);
            }
            return;
        }
        if let Some(app) = self.window_at(x, y) {
            self.focus(app);
            let w = self.windows[app.index()];
            if !right && w.close_button().contains(x, y) {
                self.close(app);
            } else if !right && w.minimize_button().contains(x, y) {
                self.minimize(app);
            } else if !right && w.title_bar().contains(x, y) {
                self.drag = Some((app, x - w.rect.x, y - w.rect.y));
            } else if w.client().contains(x, y) {
                self.capture = Some(app);
                self.send_mouse(app, MouseKind::Down { right });
            }
            return;
        }
        if right {
            return;
        }
        // the desktop itself: icons
        let hit = (0..APPS.len()).find(|&i| icon_rect(i).contains(x, y));
        if hit != self.selected_icon {
            for i in [hit, self.selected_icon].into_iter().flatten() {
                self.damage(icon_rect(i));
            }
            self.selected_icon = hit;
        }
        if let Some(i) = hit {
            let now = interrupts::ticks();
            if self.last_click.1 == i && now - self.last_click.0 <= DOUBLE_CLICK_TICKS {
                self.open(APPS[i]);
                self.last_click = (0, usize::MAX);
            } else {
                self.last_click = (now, i);
            }
        }
    }

    fn held_move(&mut self) {
        if let Some(s) = self.slider {
            self.drag_slider(s);
        } else if let Some((app, dx, dy)) = self.drag {
            self.move_window(app, self.mouse_x - dx, self.mouse_y - dy);
        } else if let Some(app) = self.capture {
            self.send_mouse(app, MouseKind::Move);
        }
    }

    fn release(&mut self) {
        if self.left || self.right {
            return;
        }
        self.drag = None;
        self.slider = None;
        if let Some(app) = self.capture.take() {
            self.send_mouse(app, MouseKind::Up);
        }
    }

    fn send_mouse(&mut self, app: App, kind: MouseKind) {
        let client = self.windows[app.index()].client();
        let ev = MouseEvent {
            x: self.mouse_x - client.x,
            y: self.mouse_y - client.y,
            kind,
        };
        let changed = match app {
            App::Paint => self.paint.on_mouse(ev),
            App::Calculator => self.calc.on_mouse(ev),
            App::Browser => self.browser.on_mouse(ev),
            App::Notepad => self.notepad.on_mouse(ev),
            App::Explorer => self.explorer.on_mouse(ev),
            App::Settings => self.settings.on_mouse(ev),
            App::About => self.about.on_mouse(ev),
            App::Terminal | App::Demo => false,
        };
        if core::mem::take(&mut self.settings.switch_layout) {
            self.toggle_layout = true;
        }
        if changed {
            self.cursor_on = true;
            self.app_changed(app);
        }
    }

    fn taskbar_click(&mut self, x: i32, y: i32) {
        match (0..3).find(|&i| self.tray_rect(i).contains(x, y)) {
            Some(0) => {
                self.toggle_layout = true;
                return;
            }
            Some(1) => return self.open_panel(Panel::Quick),
            Some(_) => return self.open_panel(Panel::Calendar),
            None => {}
        }
        let Some(slot) = (0..=APPS.len()).find(|&i| self.slot_rect(i).contains(x, y)) else {
            return;
        };
        if slot == 0 {
            self.open_menu();
            return;
        }
        let app = APPS[slot - 1];
        let w = self.windows[app.index()];
        if w.visible() && self.focused == Some(app) {
            self.minimize(app);
        } else {
            self.open(app);
        }
    }

    // ---- tray ------------------------------------------------------------------

    /// Tray button `i` from the left: the layout, quick settings, the clock.
    fn tray_rect(&self, i: usize) -> Rect {
        let top = self.height - TASKBAR_H + 4;
        match i {
            0 => Rect::new(self.width - 240, top, 52, 40),
            1 => Rect::new(self.width - 184, top, 72, 40),
            _ => Rect::new(self.width - 108, top, 100, 40),
        }
    }

    /// Where a tray flyout sits: above the taskbar, at the right.
    fn panel_rect(&self, panel: Panel) -> Rect {
        let (w, h) = panel.size();
        Rect::new(self.width - 12 - w, self.height - TASKBAR_H - 12 - h, w, h)
    }

    fn damage_panel(&mut self) {
        if self.panel.is_some() {
            self.damage(self.panel_rect(self.panel_shown));
        }
    }

    fn open_panel(&mut self, panel: Panel) {
        self.close_menu();
        if self.panel.is_some() {
            self.damage(self.panel_rect(self.panel_shown).inset(-SPREAD));
        }
        if self.panel_shown != panel {
            // a different flyout slides in from the start
            self.panel_anim = Tween::new(0, 0, 1);
        }
        self.panel = Some(panel);
        self.panel_shown = panel;
        if panel == Panel::Quick {
            self.tray.update_net();
        }
        self.panel_anim.retarget(ONE, anim::ms(220));
        self.damage(self.panel_rect(panel).inset(-SPREAD));
        self.damage_taskbar();
    }

    fn close_panel(&mut self) {
        if self.panel.take().is_some() {
            self.slider = None;
            self.panel_anim.retarget(0, anim::ms(160));
            self.damage(self.panel_rect(self.panel_shown).inset(-SPREAD));
            self.damage_taskbar();
        }
    }

    fn quick_click(&mut self, r: Rect, x: i32, y: i32) {
        match self.tray.target_at(r, x, y) {
            Some(tray::Target::LayoutTile) => self.toggle_layout = true,
            Some(tray::Target::NetworkTile) => {
                // start the network if nothing has yet
                crate::net::init();
                self.tray.update_net();
                self.damage(r);
            }
            Some(tray::Target::Slider(s)) => {
                self.slider = Some(s);
                self.drag_slider(s);
            }
            None => {}
        }
    }

    fn drag_slider(&mut self, s: tray::Slider) {
        let r = self.panel_rect(Panel::Quick);
        if self.tray.drag(r, s, self.mouse_x) {
            self.damage(r);
            if s == tray::Slider::Brightness {
                self.present_all = true;
            } else {
                self.damage_tray();
            }
        }
    }

    fn damage_tray(&mut self) {
        self.damage(self.tray_rect(0).union(&self.tray_rect(2)));
    }

    fn open_menu(&mut self) {
        self.close_panel();
        self.start.show();
        self.start
            .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y);
        self.menu.retarget(ONE, anim::ms(220));
        self.damage(self.menu_rect());
        self.damage_taskbar();
    }

    fn close_menu(&mut self) {
        if self.start.open {
            self.start.open = false;
            self.menu.retarget(0, anim::ms(160));
            self.damage(self.menu_rect());
            self.damage_taskbar();
        }
    }

    fn menu_action(&mut self, action: start::Action) {
        match action {
            start::Action::None => {}
            start::Action::Redraw => self.damage(self.menu_rect()),
            start::Action::Close => self.close_menu(),
            start::Action::Open(app) => {
                self.close_menu();
                self.open(app);
            }
            start::Action::Restart => restart(),
            start::Action::ShutDown => shut_down(),
            start::Action::Lock => self.lock(false),
            start::Action::SignOut => self.lock(true),
        }
    }

    fn menu_panel(&self) -> Rect {
        StartMenu::panel(self.width, self.height, TASKBAR_H)
    }

    /// The start menu, with room for its shadow.
    fn menu_rect(&self) -> Rect {
        self.menu_panel().inset(-SPREAD)
    }

    /// Taskbar button `i` (0 is Start), centred like Windows 11.
    fn slot_rect(&self, i: usize) -> Rect {
        let n = APPS.len() as i32 + 1;
        let total = n * 44 + (n - 1) * 4;
        let x0 = (self.width - total) / 2;
        Rect::new(x0 + i as i32 * 48, self.height - TASKBAR_H + 4, 44, 40)
    }

    fn damage_taskbar(&mut self) {
        self.damage(Rect::new(0, self.height - TASKBAR_H, self.width, TASKBAR_H));
    }

    // ---- drawing -----------------------------------------------------------

    fn render(&mut self) {
        let fading = match self.phase {
            Phase::Unlocking(t) | Phase::Locking(t) => Some(t),
            _ => None,
        };
        if self.dirty.is_empty() && fading.is_none() && !self.present_all {
            return;
        }
        self.update_surfaces();
        let (rects, n) = self.dirty.take();
        let mut back = core::mem::take(&mut self.back);
        let mut scratch = core::mem::take(&mut self.scratch);
        for r in &rects[..n] {
            let mut c = Canvas::new(back, self.width as usize, self.height as usize);
            c.clip_to(*r);
            self.draw_scene(&mut c, scratch);
            self.pointer_image.draw(&mut c, self.mouse_x, self.mouse_y);
        }
        core::mem::swap(&mut self.back, &mut back);
        core::mem::swap(&mut self.scratch, &mut scratch);
        match fading {
            // the old screen over the new one, fading out
            Some(t) => self.present_fade((ONE - t.value()) as u32),
            None if self.present_all => self.present(self.screen()),
            None => {
                for r in &rects[..n] {
                    self.present(*r);
                }
            }
        }
        self.present_all = false;
    }

    /// Facts about the system for Settings and About.
    fn system_info(&self) -> settings::Info<'_> {
        settings::Info {
            screen: (self.width, self.height),
            memory_mib: self.boot.upper_memory_kib / 1024 + 1,
            bootloader: self.boot.bootloader,
            net: self.tray.net,
            address: self.tray.address.as_str(),
            layout: self.layout,
            clock: self.clock.as_str(),
            date: self.date.as_str(),
            uptime_minutes: interrupts::ticks() / interrupts::TIMER_HZ / 60,
        }
    }

    /// Bring stale window contents up to date.
    fn update_surfaces(&mut self) {
        let surfaces = core::mem::take(&mut self.surfaces);
        for app in APPS {
            if self.stale[app.index()] && self.windows[app.index()].drawn() {
                self.stale[app.index()] = false;
                let (w, h) = app.client_size();
                let mut c = Canvas::new(surfaces[app.index()], w as usize, h as usize);
                let focused = self.focused == Some(app);
                match app {
                    App::Terminal => self.terminal.draw(&mut c, focused && self.cursor_on),
                    App::Paint => self.paint.draw(&mut c),
                    App::Calculator => self.calc.draw(&mut c),
                    App::Demo => demo::draw(&mut c),
                    App::Browser => self.browser.draw(&mut c),
                    App::Notepad => self.notepad.draw(&mut c, focused && self.cursor_on),
                    App::Explorer => self.explorer.draw(&mut c, focused && self.cursor_on),
                    App::Settings => self.settings.draw(&mut c, &self.system_info()),
                    App::About => self.about.draw(&mut c, &self.system_info()),
                }
            }
        }
        self.surfaces = surfaces;
    }

    /// Draw everything but the pointer inside the canvas's clip.
    fn draw_scene(&self, c: &mut Canvas, scratch: &mut [u32]) {
        if matches!(self.phase, Phase::Login | Phase::Locking(_)) {
            self.login.draw(c, self.wallpaper);
            return;
        }
        c.blit(
            0,
            0,
            self.width,
            self.height,
            self.wallpaper,
            self.width as usize,
        );
        self.draw_icons(c);
        for i in 0..self.order_len {
            let app = self.order[i];
            if self.windows[app.index()].drawn() {
                self.draw_window(c, app, scratch);
            }
        }
        self.draw_taskbar(c);
        if self.start.open || self.menu.value() > 0 {
            self.draw_menu(c, scratch);
        }
        if self.panel.is_some() || self.panel_anim.value() > 0 {
            self.draw_panel(c, scratch);
        }
    }

    /// Copy part of the back buffer to the screen.
    fn present(&self, r: Rect) {
        let fb = &self.fb;
        let stride = self.width as usize;
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        let dim = self.dim();
        let mut dimmed = [0u32; MAX_W];
        for y in r.y as usize..r.bottom() as usize {
            let mut row = &self.back[y * stride + r.x as usize..y * stride + r.right() as usize];
            if dim > 0 {
                let out = &mut dimmed[..row.len()];
                fade_row(out, row, &BLACK[..row.len()], dim);
                row = out;
            }
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch + r.x as usize * 4) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, row.len());
                }
            } else {
                for (i, &p) in row.iter().enumerate() {
                    put_pixel(fb, r.x as usize + i, y, p);
                }
            }
        }
    }

    /// How much to darken the picture for the brightness setting, 0 to 256.
    fn dim(&self) -> u32 {
        ((100 - self.tray.brightness.clamp(tray::MIN_BRIGHTNESS, 100)) * 256 / 100) as u32
    }

    /// Show the snapshot blended over the back buffer with `alpha` (0 to
    /// 256), for the fade between the sign-in screen and the desktop.
    fn present_fade(&self, alpha: u32) {
        let fb = &self.fb;
        let (w, h) = (self.width as usize, self.height as usize);
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        let mut row = [0u32; MAX_W];
        let dim = self.dim();
        for y in 0..h {
            let (back, snap) = (&self.back[y * w..][..w], &self.snapshot[y * w..][..w]);
            fade_row(&mut row[..w], back, snap, alpha);
            if dim > 0 {
                let faded = row;
                fade_row(&mut row[..w], &faded[..w], &BLACK[..w], dim);
            }
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, w);
                }
            } else {
                for (x, &p) in row[..w].iter().enumerate() {
                    put_pixel(fb, x, y, p);
                }
            }
        }
    }

    fn draw_icons(&self, c: &mut Canvas) {
        for (i, app) in APPS.into_iter().enumerate() {
            let r = icon_rect(i);
            if !c.visible(r) {
                continue;
            }
            if self.selected_icon == Some(i) {
                c.fill_round_alpha(r, 6, rgb(0xb0, 0xd0, 0xff), 90);
                c.outline_round(r, 6, rgb(0x9c, 0xc4, 0xf4));
            }
            self.icons.draw_large(c, app, r.x + (r.w - 48) / 2, r.y + 6);
            let label = Rect::new(r.x, r.y + 58, r.w, 16);
            c.text_centered(label.offset(1, 1), app.title(), rgb(0x10, 0x10, 0x20));
            c.text_centered(label, app.title(), 0xffffff);
        }
    }

    fn draw_window(&self, c: &mut Canvas, app: App, scratch: &mut [u32]) {
        let w = self.windows[app.index()];
        let focused = self.focused == Some(app);
        let strength = if focused { 120 } else { 70 };
        let border = if focused {
            rgb(0x8c, 0x90, 0x9c)
        } else {
            rgb(0xb4, 0xb4, 0xb8)
        };
        let r = w.rect;
        let size = (r.w * r.h) as usize;
        let Some((frame, alpha)) = self.anim_frame(app).filter(|_| size <= scratch.len()) else {
            if !c.visible(w.bounds()) || !w.visible() {
                return;
            }
            c.shadow(r, WINDOW_RADIUS, SPREAD, SHADOW_DROP, strength);
            {
                let mut win = c.sub(Rect::new(0, 0, c.width, c.height));
                win.clip_round(r, WINDOW_RADIUS);
                self.draw_window_body(&mut win, app, r);
            }
            c.outline_round(r, WINDOW_RADIUS, border);
            return;
        };
        // moving: draw it at full size on the side, then scale and blend
        if frame.w <= 0 || frame.h <= 0 || !c.visible(shadow_bounds(frame)) {
            return;
        }
        c.shadow(
            frame,
            WINDOW_RADIUS,
            SPREAD,
            SHADOW_DROP,
            strength * alpha / ONE,
        );
        {
            let mut side = Canvas::new(scratch, r.w as usize, r.h as usize);
            self.draw_window_body(&mut side, app, Rect::new(0, 0, r.w, r.h));
        }
        {
            let mut win = c.sub(Rect::new(0, 0, c.width, c.height));
            win.clip_round(frame, WINDOW_RADIUS);
            win.blit_scaled(frame, scratch, r.w, r.h, alpha);
        }
        c.outline_round_alpha(frame, WINDOW_RADIUS, border, alpha);
    }

    /// The title bar and contents of a window whose frame is `r` on `win`.
    fn draw_window_body(&self, win: &mut Canvas, app: App, r: Rect) {
        let focused = self.focused == Some(app);
        let title_face = if focused {
            rgb(0xee, 0xf1, 0xf8)
        } else {
            theme::FACE
        };
        let title_bar = Rect::new(r.x, r.y, r.w, TITLE_H);
        win.fill(title_bar, title_face);
        self.icons.draw_small(win, app, r.x + 12, r.y + 8);
        let text = if focused {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        let title = self.window_title(app);
        win.draw_text(r.x + 38, r.y + 8, &title, text);

        // caption buttons: flat until the mouse is over them
        let close = Rect::new(r.right() - 46, r.y, 46, TITLE_H);
        let min = close.offset(-46, 0);
        let lit = self.hover.level(Hover::Minimize(app)) as u32;
        if lit > 0 {
            win.fill(min, mix(title_face, theme::TEXT, 25 * lit / 256));
        }
        let (mx, my) = (min.x + 18, min.y + 16);
        win.fill_rect(mx, my, 10, 1, text);

        let lit = self.hover.level(Hover::Close(app)) as u32;
        if lit > 0 {
            win.fill(
                close,
                mix(title_face, rgb(0xc4, 0x2b, 0x1c), lit * 255 / 256),
            );
        }
        let close_glyph = mix(text, 0xffffff, lit * 255 / 256);
        let (cx, cy) = (close.x + 18, close.y + 11);
        win.line(cx, cy, cx + 9, cy + 9, close_glyph);
        win.line(cx + 9, cy, cx, cy + 9, close_glyph);

        let client = Rect::new(
            r.x + BORDER,
            r.y + TITLE_H,
            r.w - 2 * BORDER,
            r.h - TITLE_H - BORDER,
        );
        let surface = &self.surfaces[app.index()];
        win.blit(
            client.x,
            client.y,
            client.w,
            client.h,
            surface,
            client.w as usize,
        );
    }

    /// The start menu, sliding up and fading in while it opens.
    fn draw_menu(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let blink = self.cursor_on && self.start.open;
        self.draw_sliding(c, scratch, self.menu_panel(), self.menu.value(), |c, p| {
            self.start.draw(c, p, self.icons, blink)
        });
    }

    fn draw_panel(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let panel = self.panel_shown;
        let hover = match self.hover.lit()[0] {
            Some(Hover::Quick(t)) => Some(t),
            _ => None,
        };
        let r = self.panel_rect(panel);
        let shown = self.panel_anim.value() >= ONE;
        if shown {
            // in place: the shadow and frame the sliding version adds
            c.shadow(r, 8, SPREAD, 4, 120);
        }
        self.draw_sliding(c, scratch, r, self.panel_anim.value(), |c, p| {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, 8);
            if panel == Panel::Quick {
                self.tray.draw_quick(&mut m, p, self.layout, hover);
            } else {
                Tray::draw_calendar(&mut m, p);
            }
        });
        if shown {
            c.outline_round(r, 8, rgb(0xc8, 0xca, 0xd2));
        }
    }

    /// A flyout `panel` that is `p` of the way in (ONE is fully shown):
    /// lower down and see-through while it slides up from the taskbar.
    fn draw_sliding(
        &self,
        c: &mut Canvas,
        scratch: &mut [u32],
        panel: Rect,
        p: i32,
        draw: impl Fn(&mut Canvas, Rect),
    ) {
        if p >= ONE {
            draw(c, panel);
            return;
        }
        let r = panel.inset(-SPREAD);
        if !c.visible(r.union(&r.offset(0, 48))) || p <= 0 {
            return;
        }
        let frame = panel.offset(0, (ONE - p) * 48 / ONE);
        // it rises from behind the taskbar
        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
        m.clip_to(Rect::new(0, 0, self.width, self.height - TASKBAR_H));
        m.shadow(frame, 8, SPREAD, 4, 120 * p / ONE);
        {
            let mut side = Canvas::new(scratch, panel.w as usize, panel.h as usize);
            draw(&mut side, Rect::new(0, 0, panel.w, panel.h));
        }
        {
            let mut inner = m.sub(Rect::new(0, 0, self.width, self.height));
            inner.clip_round(frame, 8);
            inner.blit_scaled(frame, scratch, panel.w, panel.h, p);
        }
        m.outline_round_alpha(frame, 8, rgb(0xc8, 0xca, 0xd2), p);
    }

    fn draw_taskbar(&self, c: &mut Canvas) {
        let top = self.height - TASKBAR_H;
        let bar = Rect::new(0, top, self.width, TASKBAR_H);
        if !c.visible(bar) {
            return;
        }
        // see-through, like acrylic
        c.fill_round_alpha(bar, 0, rgb(0xf0, 0xf2, 0xf8), 220);
        c.fill_rect(0, top, self.width, 1, rgb(0xd0, 0xd4, 0xdc));

        for i in 0..=APPS.len() {
            let r = self.slot_rect(i);
            let app = if i == 0 { None } else { Some(APPS[i - 1]) };
            let active = match app {
                None => self.start.open,
                Some(a) => self.focused == Some(a) && self.windows[a.index()].visible(),
            };
            if active {
                c.fill_round_alpha(r, 5, 0xffffff, 200);
                c.outline_round(r, 5, rgb(0xe0, 0xe2, 0xe8));
            } else {
                let lit = self.hover.level(Hover::Slot(i));
                if lit > 0 {
                    c.fill_round_alpha(r, 5, 0xffffff, 140 * lit / ONE);
                }
            }
            match app {
                None => draw_start_logo(c, r.x + 10, r.y + 8),
                Some(a) => {
                    self.icons.draw_medium(c, a, r.x + 10, r.y + 7);
                    let w = self.windows[a.index()];
                    if w.open {
                        // a pill under open apps, longer for the active one
                        let (len, color) = if active {
                            (16, theme::ACCENT)
                        } else {
                            (6, rgb(0x8a, 0x8a, 0x92))
                        };
                        let pill = Rect::new(r.x + (r.w - len) / 2, r.bottom() - 4, len, 3);
                        c.fill_round(pill, 1, color);
                    }
                }
            }
        }

        // the tray: layout, network and volume, clock and date
        for i in 0..3 {
            let r = self.tray_rect(i);
            let open = match i {
                1 => self.panel == Some(Panel::Quick),
                2 => self.panel == Some(Panel::Calendar),
                _ => false,
            };
            let lit = if open {
                ONE
            } else {
                self.hover.level(Hover::Tray(i))
            };
            if lit > 0 {
                c.fill_round_alpha(r, 5, 0xffffff, 170 * lit / ONE);
            }
        }
        let layout = self.tray_rect(0);
        c.text_centered(layout, tray::layout_label(self.layout), theme::TEXT);
        let quick = self.tray_rect(1);
        let bg = rgb(0xf0, 0xf2, 0xf8);
        let y = quick.y + 12;
        tray::network_icon(c, quick.x + 14, y, self.tray.net, theme::TEXT, bg);
        tray::volume_icon(c, quick.x + 42, y, self.tray.volume, theme::TEXT);
        let clock = self.tray_rect(2);
        let line = |i: i32| Rect::new(clock.x, clock.y + 2 + i * 18, clock.w, 18);
        c.text_centered(line(0), self.clock.as_str(), theme::TEXT);
        c.text_centered(line(1), self.date.as_str(), theme::TEXT);
    }
}

// ---- layout helpers ---------------------------------------------------------

fn icon_rect(i: usize) -> Rect {
    Rect::new(16, 16 + i as i32 * 92, 88, 80)
}

// ---- pictures ---------------------------------------------------------------

/// The mouse pointer, drawn at 4x with polygons and shrunk with alpha
/// so its edges are smooth.
struct Pointer {
    pixels: [u32; POINTER_W * POINTER_H],
}

const POINTER_W: usize = 16;
const POINTER_H: usize = 24;

impl Pointer {
    fn new() -> Self {
        // the arrow outline and its white inside, in 1/4 pixels
        const OUTER: [(i32, i32); 7] = [
            (2, 2),
            (2, 82),
            (21, 64),
            (35, 94),
            (48, 88),
            (35, 60),
            (60, 60),
        ];
        const INNER: [(i32, i32); 7] = [
            (8, 16),
            (8, 68),
            (22, 55),
            (37, 86),
            (41, 84),
            (27, 54),
            (45, 54),
        ];
        const S: usize = 4;
        let mut big = [0u32; POINTER_W * S * POINTER_H * S];
        let marker = 0xff00_0000;
        big.fill(marker);
        let mut c = Canvas::new(&mut big, POINTER_W * S, POINTER_H * S);
        c.fill_polygon(&OUTER, 0x000000);
        c.fill_polygon(&INNER, 0xffffff);
        let mut pixels = [0u32; POINTER_W * POINTER_H];
        for (i, out) in pixels.iter_mut().enumerate() {
            let (ox, oy) = (i % POINTER_W, i / POINTER_W);
            let (mut sum, mut count) = (0u32, 0u32);
            for y in oy * S..oy * S + S {
                for x in ox * S..ox * S + S {
                    let p = big[y * POINTER_W * S + x];
                    if p != marker {
                        sum += p & 0xff;
                        count += 1;
                    }
                }
            }
            if let Some(grey) = sum.checked_div(count) {
                *out = (count * 255 / (S * S) as u32) << 24 | grey << 16 | grey << 8 | grey;
            }
        }
        Self { pixels }
    }

    fn draw(&self, c: &mut Canvas, x: i32, y: i32) {
        c.blit_alpha(x, y, POINTER_W as i32, POINTER_H as i32, &self.pixels);
    }
}

fn pointer_rect(x: i32, y: i32) -> Rect {
    Rect::new(x, y, POINTER_W as i32, POINTER_H as i32)
}

/// A row of black, to dim towards.
static BLACK: [u32; MAX_W] = [0; MAX_W];

/// `out = mix(a, b, alpha)` for a row, two pixels per step: each 64-bit
/// word holds the red and blue (or green) channels of both, so one
/// multiply blends four channels.
fn fade_row(out: &mut [u32], a: &[u32], b: &[u32], alpha: u32) {
    const M: u64 = 0x00ff_00ff_00ff_00ff;
    let (t, s) = (alpha.min(256) as u64, 256 - alpha.min(256) as u64);
    let pairs = out.len() / 2;
    for i in 0..pairs {
        let pa = a[2 * i] as u64 | (a[2 * i + 1] as u64) << 32;
        let pb = b[2 * i] as u64 | (b[2 * i + 1] as u64) << 32;
        let rb = (((pa & M) * s + (pb & M) * t) >> 8) & M;
        let g = ((((pa >> 8) & M) * s + ((pb >> 8) & M) * t) >> 8) & M;
        let p = rb | g << 8;
        out[2 * i] = p as u32 & 0xff_ffff;
        out[2 * i + 1] = (p >> 32) as u32 & 0xff_ffff;
    }
    if out.len() % 2 == 1 {
        let i = out.len() - 1;
        out[i] = fast_mix(a[i], b[i], alpha);
    }
}

/// Write one pixel to a framebuffer that is not 32-bit XRGB.
fn put_pixel(fb: &Framebuffer, x: usize, y: usize, p: u32) {
    let color = crate::framebuffer::Rgb::new((p >> 16) as u8, (p >> 8) as u8, p as u8);
    fb.put_raw(x, y, fb.encode(color));
}

/// Deep blue with a soft flower of light in the middle.
fn draw_wallpaper(c: &mut Canvas) {
    let (w, h) = (c.width, c.height);
    c.vertical_gradient(
        Rect::new(0, 0, w, h),
        rgb(0x0a, 0x2c, 0x74),
        rgb(0x1c, 0x5c, 0xc0),
    );
    let (cx, cy) = (w / 2, (h - TASKBAR_H) / 2 + 20);
    // unit vectors for eight directions, times 100
    const DIRS: [(i32, i32); 8] = [
        (100, 0),
        (71, 71),
        (0, 100),
        (-71, 71),
        (-100, 0),
        (-71, -71),
        (0, -100),
        (71, -71),
    ];
    let petal = h / 5;
    for (i, (dx, dy)) in DIRS.into_iter().enumerate() {
        let (px, py) = (cx + dx * petal * 3 / 400, cy + dy * petal * 3 / 400);
        let color = if i % 2 == 0 {
            rgb(0x6a, 0xb4, 0xff)
        } else {
            rgb(0x9a, 0xcc, 0xff)
        };
        let r = Rect::new(px - petal, py - petal, 2 * petal, 2 * petal);
        c.fill_round_alpha(r, petal, color, 46);
    }
    for (radius, alpha) in [(petal * 3 / 4, 50), (petal / 2, 70), (petal / 4, 90)] {
        let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
        c.fill_round_alpha(r, radius, rgb(0xe4, 0xf2, 0xff), alpha);
    }
}

/// The start button: four rounded squares.
fn draw_start_logo(c: &mut Canvas, x: i32, y: i32) {
    for (i, color) in [
        rgb(0x2a, 0x9c, 0xf4),
        rgb(0x18, 0x84, 0xe8),
        rgb(0x10, 0x74, 0xd8),
        rgb(0x0a, 0x60, 0xc4),
    ]
    .into_iter()
    .enumerate()
    {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        c.fill_round(Rect::new(x + col * 12, y + row * 12, 11, 11), 2, color);
    }
}

// ---- power ------------------------------------------------------------------

fn restart() {
    // pulse the CPU reset line through the PS/2 controller
    unsafe { port::outb(0x64, 0xfe) };
}

fn shut_down() {
    // ACPI power off in QEMU (PIIX4 and ICH9) and Bochs
    unsafe {
        port::outw(0x604, 0x2000);
        port::outw(0xb004, 0x2000);
        port::outw(0x4004, 0x3400);
    }
}

// ---- main loop ----------------------------------------------------------------

/// Run the desktop forever.
pub fn run(fb: Framebuffer, boot: &BootInfo) -> ! {
    let mut desk = Desktop::new(fb, boot);
    // the shell now prints into the terminal window
    CONSOLE.lock().detach(terminal::COLS, terminal::ROWS);
    ACTIVE.store(true, Ordering::Relaxed);
    crate::print_banner();
    desk.terminal.start();
    // the lock screen first; the terminal opens after signing in
    desk.login.lock();
    desk.damage(desk.screen());

    let mut keyboard = Keyboard::new();
    let mut mouse = ps2::MouseDecoder::new();
    let absolute = vmmouse::init();
    serial::write_str(if absolute {
        "desktop: absolute mouse\n"
    } else {
        "desktop: PS/2 mouse\n"
    });
    // bring the network up now, so the taskbar can show it
    crate::net::init();
    let mut next_blink = 0;
    let mut last_second = u64::MAX;
    loop {
        while let Some(scancode) = KEYBOARD_BYTES.pop() {
            if let Some(key) = keyboard.feed(scancode) {
                desk.layout = keyboard.layout();
                desk.on_key(key);
            }
        }
        while let Some(byte) = MOUSE_BYTES.pop() {
            if let Some(packet) = mouse.feed(byte) {
                desk.on_ps2(packet);
            }
        }
        if absolute {
            while let Some(ev) = vmmouse::poll() {
                desk.on_vmmouse(ev);
            }
        }
        while let Some(request) = REQUESTS.pop() {
            let app = APPS[(request & !CLOSE) as usize % APPS.len()];
            if request == LOCK {
                desk.lock(false);
            } else if request & CLOSE != 0 {
                desk.close(app);
            } else {
                desk.open(app);
            }
        }

        if core::mem::take(&mut desk.toggle_layout) {
            keyboard.toggle_layout();
            desk.on_key(Key::LayoutChanged);
        }
        desk.layout = keyboard.layout();
        desk.poll_apps();
        crate::net::poll();
        if desk.browser.tick() {
            desk.damage_client(App::Browser);
        }
        if CONSOLE.lock().take_changed() {
            desk.damage_client(App::Terminal);
        }
        desk.login.layout = desk.layout.name();
        desk.tick();
        let on_desktop = matches!(desk.phase, Phase::Desktop);
        let now = interrupts::ticks();
        if now >= next_blink {
            next_blink = now + BLINK_TICKS;
            desk.cursor_on = !desk.cursor_on;
            if !on_desktop {
            } else if desk.start.open {
                // the caret in the search box
                desk.damage(desk.menu_rect());
            } else if let Some(app @ (App::Terminal | App::Notepad | App::Explorer)) = desk.focused
            {
                // the text caret blinks
                desk.stale[app.index()] = true;
                desk.damage_client(app);
            }
        }
        let second = now / interrupts::TIMER_HZ;
        if second != last_second {
            last_second = second;
            let (h, m, _) = rtc::time();
            let (year, month, day) = rtc::date();
            let mut clock = StackString::<16>::new();
            let _ = write!(clock, "{:02}:{:02}", h, m);
            let changed = clock.as_str() != desk.clock.as_str();
            desk.clock = clock;
            desk.date.clear();
            let _ = write!(desk.date, "{:02}.{:02}.{}", day, month, year);
            let net_changed = desk.tray.update_net();
            if on_desktop {
                if changed || net_changed {
                    desk.damage_tray();
                    desk.damage_client(App::Settings);
                }
                if net_changed {
                    desk.damage_panel();
                }
            } else {
                desk.login.update_clock();
            }
        }

        desk.render();
        interrupts::wait_for_interrupt(|| {
            !KEYBOARD_BYTES.is_empty() || !MOUSE_BYTES.is_empty() || !REQUESTS.is_empty()
        });
    }
}
