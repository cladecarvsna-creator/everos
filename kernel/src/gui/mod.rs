//! The graphical desktop in the style of Windows 11: a background with a
//! bloom, icons, windows with rounded corners and soft shadows, a
//! centred taskbar with a start menu and a clock, and the mouse pointer.
//!
//! Everything is drawn into a back buffer in memory and then copied to
//! the screen, so nothing flickers. Only the area that changed (the
//! "dirty" rectangle) is redrawn and copied.

mod calc;
mod canvas;
mod demo;
mod paint;
mod terminal;
mod theme;

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use canvas::{mix, rgb, Canvas, Rect};

use crate::framebuffer::Framebuffer;
use crate::interrupts::{self, KEYBOARD_BYTES, MOUSE_BYTES};
use crate::keyboard::{Key, Keyboard, Layout};
use crate::multiboot::BootInfo;
use crate::sync::{ByteQueue, StaticBuffer};
use crate::{console::CONSOLE, port, ps2, rtc, serial, StackString};

const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static BACK_BUFFER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The desktop background, drawn once at start.
static WALLPAPER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();

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
}

const APPS: [App; 4] = [App::Terminal, App::Paint, App::Calculator, App::Demo];

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
        }
    }

    fn client_size(self) -> (i32, i32) {
        match self {
            App::Terminal => (terminal::CLIENT_W, terminal::CLIENT_H),
            App::Paint => (paint::CLIENT_W, paint::CLIENT_H),
            App::Calculator => (calc::CLIENT_W, calc::CLIENT_H),
            App::Demo => (demo::CLIENT_W, demo::CLIENT_H),
        }
    }

    fn default_position(self) -> (i32, i32) {
        match self {
            App::Terminal => (150, 40),
            App::Paint => (250, 100),
            App::Calculator => (760, 60),
            App::Demo => (380, 170),
        }
    }
}

/// Ask the desktop to open an app. Returns false in text mode.
pub fn request_open(app: App) -> bool {
    REQUESTS.push(app.index() as u8);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Open(App),
    Restart,
    ShutDown,
}

const MENU: [MenuItem; 6] = [
    MenuItem::Open(App::Terminal),
    MenuItem::Open(App::Paint),
    MenuItem::Open(App::Calculator),
    MenuItem::Open(App::Demo),
    MenuItem::Restart,
    MenuItem::ShutDown,
];
const MENU_W: i32 = 520;
const MENU_H: i32 = 250;
const MENU_FOOTER: i32 = 64;

/// What is under the mouse and lights up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    None,
    Minimize(App),
    Close(App),
    /// A taskbar button: 0 is Start, then the apps.
    Slot(usize),
    Menu(usize),
}

/// App icons, shrunk from the 48 pixel drawings, with alpha.
struct Icons {
    medium: [[u32; 24 * 24]; APPS.len()],
    small: [[u32; 16 * 16]; APPS.len()],
}

impl Icons {
    fn new() -> Self {
        let mut icons = Icons {
            medium: [[0; 24 * 24]; APPS.len()],
            small: [[0; 16 * 16]; APPS.len()],
        };
        for app in APPS {
            // pixels left at the marker value are transparent
            let mut big = [TRANSPARENT; 48 * 48];
            draw_icon(&mut Canvas::new(&mut big, 48, 48), app, 0, 0);
            shrink(&big, 2, &mut icons.medium[app.index()]);
            shrink(&big, 3, &mut icons.small[app.index()]);
        }
        icons
    }

    fn draw_medium(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        c.blit_alpha(x, y, 24, 24, &self.medium[app.index()]);
    }

    fn draw_small(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        c.blit_alpha(x, y, 16, 16, &self.small[app.index()]);
    }
}

const TRANSPARENT: u32 = 0xffc8_c8c8;

/// Scale a 48x48 picture down by `k`, averaging the opaque pixels of each
/// block and turning how many there were into alpha.
fn shrink(big: &[u32], k: usize, out: &mut [u32]) {
    let n = 48 / k;
    for oy in 0..n {
        for ox in 0..n {
            let (mut r, mut g, mut b, mut count) = (0, 0, 0, 0);
            for y in oy * k..oy * k + k {
                for x in ox * k..ox * k + k {
                    let p = big[y * 48 + x];
                    if p != TRANSPARENT {
                        r += (p >> 16) & 0xff;
                        g += (p >> 8) & 0xff;
                        b += p & 0xff;
                        count += 1;
                    }
                }
            }
            out[oy * n + ox] = if count == 0 {
                0
            } else {
                let alpha = count * 255 / (k * k) as u32;
                alpha << 24 | (r / count) << 16 | (g / count) << 8 | (b / count)
            };
        }
    }
}

pub struct Desktop<'a> {
    fb: Framebuffer,
    back: &'static mut [u32],
    wallpaper: &'static mut [u32],
    width: i32,
    height: i32,
    boot: &'a BootInfo,
    icons: Icons,

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
    menu_open: bool,
    selected_icon: Option<usize>,
    last_click: (u64, usize),

    layout: Layout,
    clock: StackString<16>,
    cursor_on: bool,
    dirty: Rect,

    terminal: terminal::Terminal,
    paint: paint::Paint,
    calc: calc::Calc,
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
            menu_open: false,
            selected_icon: None,
            last_click: (0, usize::MAX),
            layout: Layout::Us,
            clock: StackString::new(),
            cursor_on: true,
            dirty: Rect::default(),
            terminal: terminal::Terminal::new(),
            paint: paint::Paint::new(),
            calc: calc::Calc::new(),
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

    fn damage_client(&mut self, app: App) {
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
        }
        w.minimized = false;
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
        if self.menu_open {
            if let Key::Escape = key {
                self.close_menu();
                return;
            }
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
            App::Paint | App::Demo => false,
        };
        if changed {
            self.damage_client(app);
        }
    }

    fn on_mouse(&mut self, packet: ps2::MousePacket) {
        let (old_x, old_y) = (self.mouse_x, self.mouse_y);
        self.mouse_x = (self.mouse_x + packet.dx).clamp(0, self.width - 1);
        self.mouse_y = (self.mouse_y + packet.dy).clamp(0, self.height - 1);
        let moved = (old_x, old_y) != (self.mouse_x, self.mouse_y);
        if moved {
            self.damage(pointer_rect(old_x, old_y));
            self.damage(pointer_rect(self.mouse_x, self.mouse_y));
        }

        let (was_left, was_right) = (self.left, self.right);
        self.left = packet.left;
        self.right = packet.right;

        if self.left && !was_left {
            self.press(false);
        } else if self.right && !was_right {
            self.press(true);
        } else if moved && (self.left || self.right) {
            self.held_move();
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
    }

    fn hover_at(&self, x: i32, y: i32) -> Hover {
        if self.menu_open {
            if let Some(i) = self.menu_item_at(x, y) {
                return Hover::Menu(i);
            }
            if self.menu_rect().contains(x, y) {
                return Hover::None;
            }
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
            Hover::Menu(i) => self.menu_item_rect(i),
        }
    }

    fn press(&mut self, right: bool) {
        let (x, y) = (self.mouse_x, self.mouse_y);
        if self.menu_open {
            if let Some(i) = self.menu_item_at(x, y) {
                self.close_menu();
                if !right {
                    self.activate(MENU[i]);
                }
                return;
            }
            if self.menu_rect().contains(x, y) {
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
            self.menu_open = true;
            self.damage(self.menu_rect());
            self.damage_taskbar();
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

    fn close_menu(&mut self) {
        if self.menu_open {
            self.menu_open = false;
            self.damage(self.menu_rect());
            self.damage_taskbar();
        }
    }

    fn activate(&mut self, item: MenuItem) {
        match item {
            MenuItem::Open(app) => self.open(app),
            MenuItem::Restart => restart(),
            MenuItem::ShutDown => shut_down(),
        }
    }

    /// The start menu, with room for its shadow.
    fn menu_rect(&self) -> Rect {
        self.menu_panel().inset(-SPREAD)
    }

    fn menu_panel(&self) -> Rect {
        Rect::new(
            (self.width - MENU_W) / 2,
            self.height - TASKBAR_H - 12 - MENU_H,
            MENU_W,
            MENU_H,
        )
    }

    fn menu_item_rect(&self, i: usize) -> Rect {
        let p = self.menu_panel();
        match MENU[i] {
            MenuItem::Open(_) => Rect::new(p.x + 28 + i as i32 * 96, p.y + 64, 88, 92),
            MenuItem::Restart => Rect::new(p.right() - 240, p.bottom() - 50, 108, 36),
            MenuItem::ShutDown => Rect::new(p.right() - 124, p.bottom() - 50, 108, 36),
        }
    }

    fn menu_item_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..MENU.len()).find(|&i| self.menu_item_rect(i).contains(x, y))
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
            if self.menu_open {
                self.draw_menu(&mut c);
            }
            draw_pointer(&mut c, self.mouse_x, self.mouse_y);
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
            let mut sub = win.sub(client);
            match app {
                App::Terminal => self.terminal.draw(&mut sub, focused && self.cursor_on),
                App::Paint => self.paint.draw(&mut sub),
                App::Calculator => self.calc.draw(&mut sub),
                App::Demo => demo::draw(&mut sub),
            }
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
                None => self.menu_open,
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

    fn draw_menu(&self, c: &mut Canvas) {
        let p = self.menu_panel();
        c.shadow(p, WINDOW_RADIUS, SPREAD, SHADOW_DROP, 110);
        {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, WINDOW_RADIUS);
            m.fill_round_alpha(p, 0, rgb(0xf6, 0xf7, 0xfb), 250);
            // "Pinned" in bold, by drawing it twice
            m.draw_text(p.x + 36, p.y + 26, "Pinned", theme::TEXT);
            m.draw_text(p.x + 37, p.y + 26, "Pinned", theme::TEXT);

            for (i, item) in MENU.into_iter().enumerate() {
                let r = self.menu_item_rect(i);
                let hover = self.hover == Hover::Menu(i);
                match item {
                    MenuItem::Open(app) => {
                        if hover {
                            m.fill_round_alpha(r, 6, 0xffffff, 230);
                            m.outline_round(r, 6, theme::STROKE);
                        }
                        draw_icon(&mut m, app, r.x + 20, r.y + 10);
                        let label = Rect::new(r.x, r.y + 64, r.w, 16);
                        m.text_centered(label, app.title(), theme::TEXT);
                    }
                    MenuItem::Restart | MenuItem::ShutDown => {}
                }
            }

            let footer = Rect::new(p.x, p.bottom() - MENU_FOOTER, p.w, MENU_FOOTER);
            m.fill(footer, rgb(0xec, 0xee, 0xf4));
            m.fill_rect(p.x, footer.y, p.w, 1, theme::STROKE);
            let avatar = Rect::new(p.x + 28, footer.y + 16, 32, 32);
            m.fill_round(avatar, 16, theme::ACCENT);
            m.text_centered(avatar, "E", 0xffffff);
            m.draw_text(p.x + 72, footer.y + 24, "EverOS", theme::TEXT);

            for (i, item) in MENU.into_iter().enumerate() {
                let r = self.menu_item_rect(i);
                let label = match item {
                    MenuItem::Restart => "Restart",
                    MenuItem::ShutDown => "Shut down",
                    MenuItem::Open(_) => continue,
                };
                if self.hover == Hover::Menu(i) {
                    m.fill_round(r, 5, 0xffffff);
                    m.outline_round(r, 5, theme::STROKE);
                }
                // a power symbol: a ring with a gap and a bar
                let (cx, cy) = (r.x + 18, r.y + 18);
                let ring = Rect::new(cx - 7, cy - 7, 14, 14);
                m.outline_round(ring, 7, theme::TEXT);
                m.outline_round(ring.inset(1), 6, theme::TEXT);
                if let MenuItem::ShutDown = item {
                    let bg = if self.hover == Hover::Menu(i) {
                        0xffffff
                    } else {
                        rgb(0xec, 0xee, 0xf4)
                    };
                    m.fill_rect(cx - 3, cy - 8, 6, 6, bg);
                    m.fill_rect(cx - 1, cy - 9, 2, 8, theme::TEXT);
                } else {
                    m.fill_round(Rect::new(cx + 3, cy - 9, 5, 5), 2, theme::TEXT);
                }
                m.draw_text(r.x + 32, r.y + 10, label, theme::TEXT);
            }
        }
        c.outline_round(p, WINDOW_RADIUS, rgb(0xc8, 0xca, 0xd2));
    }
}

// ---- layout helpers ---------------------------------------------------------

fn icon_rect(i: usize) -> Rect {
    Rect::new(16, 16 + i as i32 * 92, 88, 80)
}

// ---- pictures ---------------------------------------------------------------

/// Mouse pointer: 'X' is the outline, '.' the fill, ' ' transparent.
const POINTER: [&[u8]; 19] = [
    b"X           ",
    b"XX          ",
    b"X.X         ",
    b"X..X        ",
    b"X...X       ",
    b"X....X      ",
    b"X.....X     ",
    b"X......X    ",
    b"X.......X   ",
    b"X........X  ",
    b"X.........X ",
    b"X......XXXXX",
    b"X...X..X    ",
    b"X..XX..X    ",
    b"X.X  X..X   ",
    b"XX   X..X   ",
    b"X     X..X  ",
    b"      X..X  ",
    b"       XX   ",
];

fn pointer_rect(x: i32, y: i32) -> Rect {
    Rect::new(x, y, 12, 19)
}

fn draw_pointer(c: &mut Canvas, x: i32, y: i32) {
    c.sprite(x, y, &POINTER, &[(b'X', 0x000000), (b'.', 0xffffff)]);
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

/// A 48x48 app icon.
fn draw_icon(c: &mut Canvas, app: App, x: i32, y: i32) {
    let tile = Rect::new(x + 2, y + 2, 44, 44);
    match app {
        App::Terminal => {
            c.fill_round(tile, 8, rgb(0x2b, 0x2d, 0x36));
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.fill_rect(tile.x, tile.y, tile.w, 10, rgb(0x4a, 0x4e, 0x5c));
            }
            c.outline_round(tile, 8, rgb(0x16, 0x18, 0x1e));
            c.draw_text(x + 10, y + 20, ">_", rgb(0xe8, 0xe8, 0xf0));
        }
        App::Paint => {
            c.fill_round(tile, 8, rgb(0xfa, 0xfa, 0xfc));
            c.outline_round(tile, 8, rgb(0xb8, 0xbc, 0xc8));
            c.fill_round(Rect::new(x + 9, y + 9, 13, 13), 6, rgb(0xe8, 0x3c, 0x3c));
            c.fill_round(Rect::new(x + 24, y + 10, 13, 13), 6, rgb(0x2c, 0xb8, 0x5c));
            c.fill_round(Rect::new(x + 14, y + 23, 13, 13), 6, rgb(0x1c, 0x8c, 0xf0));
            for i in 0..3 {
                c.line(
                    x + 28 + i,
                    y + 42,
                    x + 41 + i,
                    y + 29,
                    rgb(0xa8, 0x6a, 0x2c),
                );
            }
            c.fill_round(Rect::new(x + 38, y + 25, 6, 6), 2, rgb(0x40, 0x40, 0x48));
        }
        App::Calculator => {
            c.fill_round(tile, 8, rgb(0x3a, 0x3e, 0x4c));
            c.outline_round(tile, 8, rgb(0x20, 0x22, 0x2c));
            c.fill_round(Rect::new(x + 9, y + 8, 30, 9), 2, rgb(0xd8, 0xe4, 0xf4));
            for row in 0..3 {
                for col in 0..3 {
                    let color = if (row, col) == (2, 2) {
                        rgb(0x3a, 0x9c, 0xff)
                    } else {
                        rgb(0xe8, 0xe8, 0xf0)
                    };
                    let r = Rect::new(x + 9 + col * 11, y + 20 + row * 8, 8, 6);
                    c.fill_round(r, 2, color);
                }
            }
        }
        App::Demo => {
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0x16, 0x20, 0x5c), rgb(0x6a, 0x1c, 0x5c));
            }
            c.fill_round(Rect::new(x + 15, y + 15, 18, 18), 9, rgb(0xff, 0xc0, 0x40));
            c.fill_round(Rect::new(x + 9, y + 10, 8, 8), 4, rgb(0x60, 0xc0, 0xff));
            c.fill_round(Rect::new(x + 33, y + 31, 6, 6), 3, rgb(0xff, 0x70, 0x70));
            c.outline_round(tile, 8, rgb(0x10, 0x10, 0x30));
        }
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
                desk.on_mouse(packet);
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

        if CONSOLE.lock().take_changed() {
            desk.damage_client(App::Terminal);
        }
        let now = interrupts::ticks();
        if now >= next_blink {
            next_blink = now + BLINK_TICKS;
            desk.cursor_on = !desk.cursor_on;
            if desk.focused == Some(App::Terminal) {
                let client = desk.windows[App::Terminal.index()].client();
                let cursor = desk.terminal.cursor_rect().offset(client.x, client.y);
                desk.damage(cursor);
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
