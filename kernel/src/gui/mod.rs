//! The graphical desktop in the style of Windows 11: a background with a
//! bloom, icons, windows with rounded corners and soft shadows, a
//! centred taskbar with a start menu and a clock, and the mouse pointer.
//!
//! Everything is drawn into a back buffer in memory and then copied to
//! the screen, so nothing flickers. Only the area that changed (the
//! "dirty" rectangle) is redrawn and copied.

mod browser;
mod calc;
mod canvas;
mod demo;
#[rustfmt::skip]
mod font_data;
mod icons;
mod paint;
mod start;
mod terminal;
mod text;
mod theme;
#[rustfmt::skip]
mod web_font_data;

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use canvas::{mix, rgb, Canvas, Rect};
use icons::{draw_icon, Icons};
use start::StartMenu;

use crate::framebuffer::Framebuffer;
use crate::interrupts::{self, KEYBOARD_BYTES, MOUSE_BYTES};
use crate::keyboard::{Key, Keyboard, Layout};
use crate::multiboot::BootInfo;
use crate::sync::{ByteQueue, StaticBuffer};
use crate::{console::CONSOLE, port, ps2, rtc, serial, vmmouse, StackString};

const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static BACK_BUFFER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The desktop background, drawn once at start.
static WALLPAPER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// Window contents. Each app draws into its own part only when its content
/// changes, so moving a window just copies pixels.
static SURFACES: StaticBuffer<{ 6 * 1024 * 1024 }> = StaticBuffer::new();

/// Whether the desktop is running (the shell asks before opening apps).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Apps to open or close, sent from the shell.
static REQUESTS: ByteQueue = ByteQueue::new();
const CLOSE: u8 = 0x80;

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
    Paint,
    Calculator,
    Demo,
    Browser,
}

const APPS: [App; 5] = [
    App::Terminal,
    App::Paint,
    App::Calculator,
    App::Demo,
    App::Browser,
];

impl App {
    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            App::Paint => "Paint",
            App::Calculator => "Calculator",
            App::Demo => "Graphics",
            App::Browser => "Browser",
        }
    }

    fn client_size(self) -> (i32, i32) {
        match self {
            App::Terminal => (terminal::CLIENT_W, terminal::CLIENT_H),
            App::Paint => (paint::CLIENT_W, paint::CLIENT_H),
            App::Calculator => (calc::CLIENT_W, calc::CLIENT_H),
            App::Demo => (demo::CLIENT_W, demo::CLIENT_H),
            App::Browser => (browser::CLIENT_W, browser::CLIENT_H),
        }
    }

    fn default_position(self) -> (i32, i32) {
        match self {
            App::Terminal => (240, 70),
            App::Paint => (520, 150),
            App::Calculator => (1440, 90),
            App::Demo => (760, 330),
            App::Browser => (200, 40),
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

#[derive(Clone, Copy)]
struct Window {
    /// Outer frame, including the title bar and border.
    rect: Rect,
    open: bool,
    minimized: bool,
}

impl Window {
    fn visible(&self) -> bool {
        self.open && !self.minimized
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
        Rect::new(
            self.rect.x - SPREAD,
            self.rect.y - SPREAD,
            self.rect.w + 2 * SPREAD,
            self.rect.h + 2 * SPREAD + SHADOW_DROP,
        )
    }
}

/// What is under the mouse and lights up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    None,
    Minimize(App),
    Close(App),
    /// A taskbar button: 0 is Start, then the apps.
    Slot(usize),
}

pub struct Desktop<'a> {
    fb: Framebuffer,
    back: &'static mut [u32],
    wallpaper: &'static mut [u32],
    width: i32,
    height: i32,
    boot: &'a BootInfo,
    icons: Icons,
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
    hover: Hover,
    start: StartMenu,
    /// Mouse buttons as the PS/2 mouse and the vmmouse last reported them.
    ps2_buttons: (bool, bool),
    vm_buttons: (bool, bool),
    /// Last vmmouse position, to tell moves from button-only events.
    vm_position: (u32, u32),
    selected_icon: Option<usize>,
    last_click: (u64, usize),

    layout: Layout,
    clock: StackString<16>,
    cursor_on: bool,
    dirty: Rect,
    surfaces: [&'static mut [u32]; APPS.len()],
    /// Apps whose surface must be drawn again.
    stale: [bool; APPS.len()],

    terminal: terminal::Terminal,
    paint: paint::Paint,
    calc: calc::Calc,
    browser: alloc::boxed::Box<browser::Browser>,
}

impl<'a> Desktop<'a> {
    pub fn new(fb: Framebuffer, boot: &'a BootInfo) -> Self {
        let width = fb.width.min(MAX_W) as i32;
        let height = fb.height.min(MAX_H) as i32;
        let mut windows = [Window {
            rect: Rect::default(),
            open: false,
            minimized: false,
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
        Self {
            fb,
            back: BACK_BUFFER.take(),
            wallpaper,
            width,
            height,
            boot,
            icons: Icons::new(),
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
            hover: Hover::None,
            start: StartMenu::new(),
            ps2_buttons: (false, false),
            vm_buttons: (false, false),
            vm_position: (0, 0),
            selected_icon: None,
            last_click: (0, usize::MAX),
            layout: Layout::Us,
            clock: StackString::new(),
            cursor_on: true,
            dirty: Rect::default(),
            surfaces,
            stale: [true; APPS.len()],
            terminal: terminal::Terminal::new(),
            paint: paint::Paint::new(),
            calc: calc::Calc::new(),
            browser: alloc::boxed::Box::new(browser::Browser::new()),
        }
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn damage(&mut self, r: Rect) {
        self.dirty = self.dirty.union(&r);
    }

    fn damage_window(&mut self, app: App) {
        let w = self.windows[app.index()];
        if w.visible() {
            self.damage(w.bounds());
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

    // ---- window management ----------------------------------------------

    fn open(&mut self, app: App) {
        let w = &mut self.windows[app.index()];
        if !w.open {
            w.open = true;
            self.order[self.order_len] = app;
            self.order_len += 1;
            // for the boot test, which only sees the serial port
            serial::write_str("\ndesktop: opened ");
            serial::write_str(app.title());
            serial::write_str("\n");
            self.start.note_opened(app);
        }
        w.minimized = false;
        if app == App::Browser {
            self.browser.start();
        }
        self.focus(app);
        self.damage_taskbar();
    }

    fn close(&mut self, app: App) {
        if !self.windows[app.index()].open {
            return;
        }
        self.damage_window(app);
        self.windows[app.index()].open = false;
        self.remove_from_order(app);
        if self.focused == Some(app) {
            self.focus_top();
        }
        self.damage_taskbar();
    }

    fn minimize(&mut self, app: App) {
        self.damage_window(app);
        self.windows[app.index()].minimized = true;
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
        if let Key::LayoutChanged = key {
            self.damage_taskbar();
            return;
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
            App::Paint | App::Demo => false,
        };
        if changed {
            self.damage_client(app);
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
        if ev.wheel != 0
            && self.window_at(self.mouse_x, self.mouse_y) == Some(App::Browser)
            && self.browser.on_wheel(ev.wheel)
        {
            self.damage_client(App::Browser);
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

        if self.left && !was_left {
            self.press(false);
        } else if self.right && !was_right {
            self.press(true);
        } else if moved && (self.left || self.right) {
            self.held_move();
        } else if moved && self.window_at(self.mouse_x, self.mouse_y) == Some(App::Browser) {
            // the browser shows where a link goes
            let client = self.windows[App::Browser.index()].client();
            if self
                .browser
                .on_hover(self.mouse_x - client.x, self.mouse_y - client.y)
            {
                self.damage_client(App::Browser);
            }
        }
        if (was_left && !self.left) || (was_right && !self.right) {
            self.release();
        }
        self.update_hover();
    }

    /// Light up whatever is under the mouse now.
    fn update_hover(&mut self) {
        let hover = if self.drag.is_some() {
            Hover::None
        } else {
            self.hover_at(self.mouse_x, self.mouse_y)
        };
        if hover != self.hover {
            let (old, new) = (self.hover_rect(self.hover), self.hover_rect(hover));
            self.hover = hover;
            self.damage(old);
            self.damage(new);
        }
        if self.start.open
            && self
                .start
                .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y)
        {
            self.damage(self.menu_rect());
        }
    }

    fn hover_at(&self, x: i32, y: i32) -> Hover {
        if self.start.open && self.menu_panel().contains(x, y) {
            return Hover::None;
        }
        if y >= self.height - TASKBAR_H {
            return match (0..=APPS.len()).find(|&i| self.slot_rect(i).contains(x, y)) {
                Some(i) => Hover::Slot(i),
                None => Hover::None,
            };
        }
        match self.window_at(x, y) {
            Some(app) => {
                let w = self.windows[app.index()];
                if w.close_button().contains(x, y) {
                    Hover::Close(app)
                } else if w.minimize_button().contains(x, y) {
                    Hover::Minimize(app)
                } else {
                    Hover::None
                }
            }
            None => Hover::None,
        }
    }

    fn hover_rect(&self, hover: Hover) -> Rect {
        match hover {
            Hover::None => Rect::default(),
            Hover::Minimize(app) => self.windows[app.index()].minimize_button(),
            Hover::Close(app) => self.windows[app.index()].close_button(),
            Hover::Slot(i) => self.slot_rect(i),
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
        if let Some((app, dx, dy)) = self.drag {
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
            App::Terminal | App::Demo => false,
        };
        if changed {
            self.damage_client(app);
        }
    }

    fn taskbar_click(&mut self, x: i32, y: i32) {
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

    fn open_menu(&mut self) {
        self.start.show();
        self.start
            .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y);
        self.damage(self.menu_rect());
        self.damage_taskbar();
    }

    fn close_menu(&mut self) {
        if self.start.open {
            self.start.open = false;
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
        let dirty = self.dirty.intersect(&self.screen());
        self.dirty = Rect::default();
        if dirty.is_empty() {
            return;
        }
        // bring stale window contents up to date first
        let surfaces = core::mem::take(&mut self.surfaces);
        for app in APPS {
            if self.stale[app.index()] && self.windows[app.index()].visible() {
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
                }
            }
        }
        self.surfaces = surfaces;

        let back = core::mem::take(&mut self.back);
        {
            let mut c = Canvas::new(back, self.width as usize, self.height as usize);
            c.clip_to(dirty);
            c.blit(
                0,
                0,
                self.width,
                self.height,
                self.wallpaper,
                self.width as usize,
            );
            self.draw_icons(&mut c);
            for i in 0..self.order_len {
                let app = self.order[i];
                if self.windows[app.index()].visible() {
                    self.draw_window(&mut c, app);
                }
            }
            self.draw_taskbar(&mut c);
            if self.start.open {
                let blink = self.cursor_on;
                self.start
                    .draw(&mut c, self.menu_panel(), &self.icons, blink);
            }
            self.pointer_image.draw(&mut c, self.mouse_x, self.mouse_y);
        }
        self.back = back;
        self.present(dirty);
    }

    /// Copy part of the back buffer to the screen.
    fn present(&self, r: Rect) {
        let fb = &self.fb;
        let stride = self.width as usize;
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        for y in r.y as usize..r.bottom() as usize {
            let row = &self.back[y * stride + r.x as usize..y * stride + r.right() as usize];
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch + r.x as usize * 4) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, row.len());
                }
            } else {
                for (i, &p) in row.iter().enumerate() {
                    let color =
                        crate::framebuffer::Rgb::new((p >> 16) as u8, (p >> 8) as u8, p as u8);
                    fb.put_raw(r.x as usize + i, y, fb.encode(color));
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
            draw_icon(c, app, r.x + (r.w - 48) / 2, r.y + 6);
            let label = Rect::new(r.x, r.y + 58, r.w, 16);
            c.text_centered(label.offset(1, 1), app.title(), rgb(0x10, 0x10, 0x20));
            c.text_centered(label, app.title(), 0xffffff);
        }
    }

    fn draw_window(&self, c: &mut Canvas, app: App) {
        let w = self.windows[app.index()];
        if !c.visible(w.bounds()) {
            return;
        }
        let r = w.rect;
        let focused = self.focused == Some(app);
        let strength = if focused { 120 } else { 70 };
        c.shadow(r, WINDOW_RADIUS, SPREAD, SHADOW_DROP, strength);

        {
            let mut win = c.sub(Rect::new(0, 0, c.width, c.height));
            win.clip_round(r, WINDOW_RADIUS);
            let title_face = if focused {
                rgb(0xee, 0xf1, 0xf8)
            } else {
                theme::FACE
            };
            win.fill(w.title_bar(), title_face);
            self.icons.draw_small(&mut win, app, r.x + 12, r.y + 8);
            let text = if focused {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            };
            win.draw_text(r.x + 38, r.y + 8, app.title(), text);

            // caption buttons: flat until the mouse is over them
            let min = w.minimize_button();
            if self.hover == Hover::Minimize(app) {
                win.fill(min, mix(title_face, theme::TEXT, 25));
            }
            let (mx, my) = (min.x + 18, min.y + 16);
            win.fill_rect(mx, my, 10, 1, text);

            let close = w.close_button();
            let close_glyph = if self.hover == Hover::Close(app) {
                win.fill(close, rgb(0xc4, 0x2b, 0x1c));
                0xffffff
            } else {
                text
            };
            let (cx, cy) = (close.x + 18, close.y + 11);
            win.line(cx, cy, cx + 9, cy + 9, close_glyph);
            win.line(cx + 9, cy, cx, cy + 9, close_glyph);

            let client = w.client();
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
        let border = if focused {
            rgb(0x8c, 0x90, 0x9c)
        } else {
            rgb(0xb4, 0xb4, 0xb8)
        };
        c.outline_round(r, WINDOW_RADIUS, border);
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
            } else if self.hover == Hover::Slot(i) {
                c.fill_round_alpha(r, 5, 0xffffff, 140);
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

        // keyboard layout and clock
        let clock = Rect::new(self.width - 84, top + 4, 76, 40);
        c.text_centered(clock, self.clock.as_str(), theme::TEXT);
        let layout = Rect::new(self.width - 124, top + 12, 34, 24);
        c.text_centered(layout, self.layout.name(), theme::TEXT);
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
    desk.open(App::Terminal);
    desk.damage(desk.screen());

    let mut keyboard = Keyboard::new();
    let mut mouse = ps2::MouseDecoder::new();
    let absolute = vmmouse::init();
    serial::write_str(if absolute {
        "desktop: absolute mouse\n"
    } else {
        "desktop: PS/2 mouse\n"
    });
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
            if request & CLOSE != 0 {
                desk.close(app);
            } else {
                desk.open(app);
            }
        }

        if desk.browser.tick() {
            desk.damage_client(App::Browser);
        }
        if CONSOLE.lock().take_changed() {
            desk.damage_client(App::Terminal);
        }
        let now = interrupts::ticks();
        if now >= next_blink {
            next_blink = now + BLINK_TICKS;
            desk.cursor_on = !desk.cursor_on;
            if desk.start.open {
                // the caret in the search box
                desk.damage(desk.menu_rect());
            } else if desk.focused == Some(App::Terminal) {
                desk.damage_client(App::Terminal);
            }
        }
        let second = now / interrupts::TIMER_HZ;
        if second != last_second {
            last_second = second;
            let (h, m, s) = rtc::time();
            desk.clock.clear();
            let _ = write!(desk.clock, "{:02}:{:02}:{:02}", h, m, s);
            desk.damage(Rect::new(
                desk.width - 84,
                desk.height - TASKBAR_H,
                84,
                TASKBAR_H,
            ));
        }

        desk.render();
        interrupts::wait_for_interrupt(|| {
            !KEYBOARD_BYTES.is_empty() || !MOUSE_BYTES.is_empty() || !REQUESTS.is_empty()
        });
    }
}
