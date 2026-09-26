//! The graphical desktop: background, icons, windows, a taskbar with a
//! start menu and a clock, and the mouse pointer.
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

use canvas::{mix, rgb, Canvas, Color, Rect};

use crate::framebuffer::Framebuffer;
use crate::interrupts::{self, KEYBOARD_BYTES, MOUSE_BYTES};
use crate::keyboard::{Key, Keyboard, Layout};
use crate::multiboot::BootInfo;
use crate::sync::{ByteQueue, StaticBuffer};
use crate::{console::CONSOLE, font, port, ps2, rtc, serial, StackString};

const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static BACK_BUFFER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();

/// Whether the desktop is running (the shell asks before opening apps).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Apps to open or close, sent from the shell.
static REQUESTS: ByteQueue = ByteQueue::new();
const CLOSE: u8 = 0x80;

const TASKBAR_H: i32 = 36;
const TITLE_H: i32 = 26;
const BORDER: i32 = 3;
const SHADOW: i32 = 6;
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
            App::Paint => (250, 110),
            App::Calculator => (760, 60),
            App::Demo => (380, 180),
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
        Rect::new(self.rect.right() - 26, self.rect.y + 4, 20, 18)
    }

    fn minimize_button(&self) -> Rect {
        self.close_button().offset(-24, 0)
    }

    /// Everything the window draws on, shadow included.
    fn bounds(&self) -> Rect {
        Rect::new(
            self.rect.x,
            self.rect.y,
            self.rect.w + SHADOW,
            self.rect.h + SHADOW,
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
const MENU_W: i32 = 210;
const MENU_ITEM_H: i32 = 30;
const MENU_BANNER: i32 = 28;

pub struct Desktop<'a> {
    fb: Framebuffer,
    back: &'static mut [u32],
    width: i32,
    height: i32,
    boot: &'a BootInfo,

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
    menu_open: bool,
    menu_hover: Option<usize>,
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
            let x = x.min(width - w - 2 * BORDER - SHADOW).max(0);
            let y = y.min(height - TASKBAR_H - h - TITLE_H - BORDER).max(0);
            windows[app.index()].rect = Rect::new(x, y, w + 2 * BORDER, h + TITLE_H + BORDER);
        }
        Self {
            fb,
            back: BACK_BUFFER.take(),
            width,
            height,
            boot,
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
            menu_open: false,
            menu_hover: None,
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
        if moved && self.menu_open {
            let hover = self.menu_item_at(self.mouse_x, self.mouse_y);
            if hover != self.menu_hover {
                self.menu_hover = hover;
                self.damage(self.menu_rect());
            }
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
            let on_start = start_button().contains(x, y - (self.height - TASKBAR_H));
            self.close_menu();
            if on_start {
                return;
            }
        }
        if y >= self.height - TASKBAR_H {
            if !right {
                self.taskbar_click(x, y - (self.height - TASKBAR_H));
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
        if start_button().contains(x, y) {
            self.menu_open = true;
            self.menu_hover = None;
            self.damage(self.menu_rect());
            self.damage_taskbar();
            return;
        }
        let hit = self
            .open_apps()
            .enumerate()
            .find(|&(i, _)| task_button(i).contains(x, y));
        if let Some((_, app)) = hit {
            let w = self.windows[app.index()];
            if w.minimized || self.focused != Some(app) {
                self.open(app);
            } else {
                self.minimize(app);
            }
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

    fn open_apps(&self) -> impl Iterator<Item = App> + '_ {
        APPS.into_iter().filter(|a| self.windows[a.index()].open)
    }

    fn menu_rect(&self) -> Rect {
        let h = MENU_BANNER + MENU.len() as i32 * MENU_ITEM_H + 12;
        Rect::new(2, self.height - TASKBAR_H - h, MENU_W, h + SHADOW)
    }

    fn menu_item_rect(&self, i: usize) -> Rect {
        let menu = self.menu_rect();
        let gap = if i >= 4 { 10 } else { 0 };
        Rect::new(
            menu.x + 4,
            menu.y + MENU_BANNER + 2 + i as i32 * MENU_ITEM_H + gap,
            MENU_W - 8,
            MENU_ITEM_H,
        )
    }

    fn menu_item_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..MENU.len()).find(|&i| self.menu_item_rect(i).contains(x, y))
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
            self.draw_background(&mut c);
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

    fn draw_background(&self, c: &mut Canvas) {
        let area = Rect::new(0, 0, self.width, self.height - TASKBAR_H);
        c.vertical_gradient(area, rgb(0x1c, 0x3a, 0x7a), rgb(0x5a, 0x2a, 0x78));
        // a soft glow and the name in the middle of the desktop
        let (cx, cy) = (self.width / 2, (self.height - TASKBAR_H) / 2);
        for i in 0..6 {
            let r = 150 - i * 20;
            c.fill_circle(
                cx,
                cy,
                r,
                mix(rgb(0x3a, 0x3a, 0x88), rgb(0x8a, 0x6a, 0xc8), i as u32 * 40),
            );
        }
        let title = "EverOS";
        let x = cx - canvas::text_width(title) * 3 / 2;
        draw_scaled(c, x + 3, cy - 21, title, 3, rgb(0x20, 0x18, 0x48));
        draw_scaled(c, x, cy - 24, title, 3, rgb(0xf4, 0xf0, 0xff));
        let hint = "double-click an icon to start an app";
        c.text_centered(
            Rect::new(0, cy + 40, self.width, 16),
            hint,
            rgb(0xd0, 0xc8, 0xf0),
        );
    }

    fn draw_icons(&self, c: &mut Canvas) {
        for (i, app) in APPS.into_iter().enumerate() {
            let r = icon_rect(i);
            if !c.visible(r) {
                continue;
            }
            let selected = self.selected_icon == Some(i);
            if selected {
                c.fill(r, rgb(0x5a, 0x7a, 0xd8));
                c.outline(r, rgb(0xa8, 0xc0, 0xff));
            }
            draw_icon(c, app, r.x + (r.w - 48) / 2, r.y + 6);
            let label = Rect::new(r.x, r.y + 58, r.w, 16);
            c.text_centered(label.offset(1, 1), app.title(), rgb(0x10, 0x10, 0x20));
            c.text_centered(label, app.title(), rgb(0xff, 0xff, 0xff));
        }
    }

    fn draw_window(&self, c: &mut Canvas, app: App) {
        let w = self.windows[app.index()];
        if !c.visible(w.bounds()) {
            return;
        }
        let r = w.rect;
        let focused = self.focused == Some(app);
        c.darken(Rect::new(r.x + SHADOW, r.bottom(), r.w, SHADOW), 90);
        c.darken(Rect::new(r.right(), r.y + SHADOW, SHADOW, r.h - SHADOW), 90);

        let (left, right, text) = if focused {
            (theme::ACCENT, theme::ACCENT_2, rgb(0xff, 0xff, 0xff))
        } else {
            (
                rgb(0x8c, 0x92, 0xa4),
                rgb(0xa8, 0x9c, 0xb4),
                rgb(0xe8, 0xe8, 0xf0),
            )
        };
        c.fill(r, left);
        c.horizontal_gradient(w.title_bar(), left, right);
        c.fill_rect(r.x, r.y, r.w, 1, mix(left, 0xffffff, 100));
        c.outline(r, theme::DARK);
        draw_icon_small(c, app, r.x + 6, r.y + 5);
        c.draw_text(r.x + 28, r.y + 5, app.title(), text);

        let close_face = if focused {
            rgb(0xd8, 0x50, 0x58)
        } else {
            mix(left, right, 128)
        };
        for (button, label, face) in [
            (w.minimize_button(), "_", mix(left, right, 128)),
            (w.close_button(), "×", close_face),
        ] {
            c.fill(button, face);
            c.outline(button, mix(face, 0xffffff, 90));
            c.text_centered(button, label, 0xffffff);
        }

        let client = w.client();
        let mut sub = c.sub(client);
        match app {
            App::Terminal => self.terminal.draw(&mut sub, focused && self.cursor_on),
            App::Paint => self.paint.draw(&mut sub),
            App::Calculator => self.calc.draw(&mut sub),
            App::Demo => demo::draw(&mut sub),
        }
    }

    fn draw_taskbar(&self, c: &mut Canvas) {
        let top = self.height - TASKBAR_H;
        let bar = Rect::new(0, top, self.width, TASKBAR_H);
        if !c.visible(bar) {
            return;
        }
        c.vertical_gradient(bar, rgb(0x2c, 0x30, 0x48), rgb(0x18, 0x1a, 0x28));
        c.fill_rect(0, top, self.width, 1, rgb(0x6a, 0x70, 0xa0));

        let mut bar_canvas = c.sub(bar);
        let c = &mut bar_canvas;
        let start = start_button();
        let (a, b) = if self.menu_open {
            (mix(theme::ACCENT, 0, 60), mix(theme::ACCENT_2, 0, 60))
        } else {
            (mix(theme::ACCENT, 0xffffff, 40), theme::ACCENT_2)
        };
        c.vertical_gradient(start, a, b);
        c.outline(start, rgb(0x9a, 0x9c, 0xff));
        c.fill_circle(start.x + 16, start.y + 14, 7, rgb(0xff, 0xe0, 0x6e));
        c.fill_circle(start.x + 16, start.y + 14, 3, theme::ACCENT);
        c.draw_text(start.x + 30, start.y + 6, "Start", 0xffffff);

        for (i, app) in self.open_apps().enumerate() {
            let r = task_button(i);
            if r.right() > self.width - 170 {
                break;
            }
            let w = self.windows[app.index()];
            let active = self.focused == Some(app) && !w.minimized;
            let face = if active {
                rgb(0x50, 0x5a, 0x90)
            } else {
                rgb(0x34, 0x38, 0x54)
            };
            c.fill(r, face);
            c.outline(
                r,
                if active {
                    rgb(0x9a, 0xa4, 0xe8)
                } else {
                    rgb(0x50, 0x56, 0x78)
                },
            );
            draw_icon_small(c, app, r.x + 6, r.y + 5);
            let color = if w.minimized {
                rgb(0xa0, 0xa4, 0xc0)
            } else {
                0xffffff
            };
            c.draw_text(r.x + 28, r.y + 6, app.title(), color);
        }

        // keyboard layout and clock
        let clock = Rect::new(self.width - 84, 4, 80, 28);
        c.text_centered(clock, self.clock.as_str(), 0xffffff);
        let layout = Rect::new(self.width - 124, 7, 32, 22);
        c.fill(layout, rgb(0x40, 0x46, 0x6a));
        c.outline(layout, rgb(0x70, 0x78, 0xb0));
        c.text_centered(layout, self.layout.name(), 0xffffff);
    }

    fn draw_menu(&self, c: &mut Canvas) {
        let menu = self.menu_rect();
        let r = Rect::new(menu.x, menu.y, menu.w, menu.h - SHADOW);
        c.darken(Rect::new(r.right(), r.y + SHADOW, SHADOW, r.h), 90);
        c.fill(r, theme::FACE);
        c.outline(r, theme::DARK);
        let banner = Rect::new(r.x + 1, r.y + 1, r.w - 2, MENU_BANNER);
        c.horizontal_gradient(banner, theme::ACCENT, theme::ACCENT_2);
        c.draw_text(banner.x + 10, banner.y + 6, "EverOS", 0xffffff);
        for (i, item) in MENU.into_iter().enumerate() {
            let ir = self.menu_item_rect(i);
            let hover = self.menu_hover == Some(i);
            if hover {
                c.fill(ir, theme::ACCENT);
            }
            let color = if hover { 0xffffff } else { theme::TEXT };
            let label = match item {
                MenuItem::Open(app) => {
                    draw_icon_small(c, app, ir.x + 6, ir.y + 7);
                    app.title()
                }
                MenuItem::Restart => {
                    c.fill_circle(ir.x + 14, ir.y + 15, 7, rgb(0x40, 0xa0, 0x60));
                    c.fill_circle(
                        ir.x + 14,
                        ir.y + 15,
                        4,
                        if hover { theme::ACCENT } else { theme::FACE },
                    );
                    "Restart"
                }
                MenuItem::ShutDown => {
                    c.fill_circle(ir.x + 14, ir.y + 15, 7, rgb(0xd0, 0x40, 0x40));
                    c.fill_rect(ir.x + 13, ir.y + 9, 3, 7, 0xffffff);
                    "Shut down"
                }
            };
            c.draw_text(ir.x + 30, ir.y + 7, label, color);
        }
        let sep = self.menu_item_rect(4).y - 6;
        c.fill_rect(r.x + 8, sep, r.w - 16, 1, theme::SHADOW);
    }
}

// ---- layout helpers ---------------------------------------------------------

fn icon_rect(i: usize) -> Rect {
    Rect::new(16, 16 + i as i32 * 92, 88, 80)
}

/// In taskbar coordinates.
fn start_button() -> Rect {
    Rect::new(4, 4, 84, 28)
}

fn task_button(i: usize) -> Rect {
    Rect::new(96 + i as i32 * 144, 4, 140, 28)
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

/// A 48x48 desktop icon.
fn draw_icon(c: &mut Canvas, app: App, x: i32, y: i32) {
    match app {
        App::Terminal => {
            c.fill_rect(x + 2, y + 4, 44, 38, rgb(0x20, 0x22, 0x2c));
            c.fill_rect(x + 2, y + 4, 44, 8, rgb(0x90, 0x98, 0xb0));
            c.outline(Rect::new(x + 2, y + 4, 44, 38), rgb(0xd0, 0xd4, 0xe0));
            c.draw_text(x + 8, y + 18, ">_", rgb(0x7c, 0xe0, 0x8c));
        }
        App::Paint => {
            c.fill_rect(x + 4, y + 4, 40, 40, 0xffffff);
            c.outline(Rect::new(x + 4, y + 4, 40, 40), rgb(0x60, 0x60, 0x70));
            c.fill_circle(x + 16, y + 16, 6, rgb(0xed, 0x1c, 0x24));
            c.fill_circle(x + 30, y + 18, 6, rgb(0x22, 0xb1, 0x4c));
            c.fill_circle(x + 22, y + 30, 6, rgb(0x00, 0xa2, 0xe8));
            for i in 0..4 {
                c.line(
                    x + 26 + i,
                    y + 44,
                    x + 44 + i,
                    y + 26,
                    rgb(0xa0, 0x60, 0x20),
                );
            }
            c.fill_rect(x + 40, y + 24, 6, 5, rgb(0xc0, 0xc0, 0xc8));
        }
        App::Calculator => {
            c.fill_rect(x + 8, y + 2, 32, 44, rgb(0x50, 0x56, 0x6a));
            c.outline(Rect::new(x + 8, y + 2, 32, 44), rgb(0xd0, 0xd4, 0xe0));
            c.fill_rect(x + 12, y + 6, 24, 9, rgb(0xc8, 0xe8, 0xc0));
            for row in 0..3 {
                for col in 0..3 {
                    let color = if (row, col) == (2, 2) {
                        rgb(0xff, 0x9a, 0x40)
                    } else {
                        rgb(0xe8, 0xe8, 0xf0)
                    };
                    c.fill_rect(x + 12 + col * 9, y + 19 + row * 9, 6, 6, color);
                }
            }
        }
        App::Demo => {
            c.vertical_gradient(
                Rect::new(x + 4, y + 4, 40, 40),
                rgb(0x10, 0x18, 0x40),
                rgb(0x50, 0x10, 0x40),
            );
            c.fill_circle(x + 24, y + 24, 9, rgb(0xff, 0xc0, 0x40));
            c.fill_circle(x + 13, y + 14, 4, rgb(0x60, 0xc0, 0xff));
            c.fill_circle(x + 36, y + 34, 3, rgb(0xff, 0x70, 0x70));
            c.outline(Rect::new(x + 4, y + 4, 40, 40), rgb(0xd0, 0xd4, 0xe0));
        }
    }
}

/// A 16x16 icon for title bars, the taskbar and the menu.
fn draw_icon_small(c: &mut Canvas, app: App, x: i32, y: i32) {
    match app {
        App::Terminal => {
            c.fill_rect(x, y + 1, 16, 14, rgb(0x20, 0x22, 0x2c));
            c.outline(Rect::new(x, y + 1, 16, 14), rgb(0xc0, 0xc4, 0xd0));
            c.line(x + 3, y + 5, x + 6, y + 8, rgb(0x7c, 0xe0, 0x8c));
            c.line(x + 6, y + 8, x + 3, y + 11, rgb(0x7c, 0xe0, 0x8c));
            c.fill_rect(x + 8, y + 11, 5, 1, rgb(0x7c, 0xe0, 0x8c));
        }
        App::Paint => {
            c.fill_rect(x, y, 16, 16, 0xffffff);
            c.outline(Rect::new(x, y, 16, 16), rgb(0x60, 0x60, 0x70));
            c.fill_circle(x + 5, y + 5, 2, rgb(0xed, 0x1c, 0x24));
            c.fill_circle(x + 11, y + 6, 2, rgb(0x22, 0xb1, 0x4c));
            c.fill_circle(x + 7, y + 11, 2, rgb(0x00, 0xa2, 0xe8));
        }
        App::Calculator => {
            c.fill_rect(x + 2, y, 12, 16, rgb(0x50, 0x56, 0x6a));
            c.fill_rect(x + 4, y + 2, 8, 3, rgb(0xc8, 0xe8, 0xc0));
            for row in 0..3 {
                for col in 0..3 {
                    c.fill_rect(x + 4 + col * 3, y + 7 + row * 3, 2, 2, 0xffffff);
                }
            }
        }
        App::Demo => {
            c.fill_rect(x, y, 16, 16, rgb(0x30, 0x14, 0x40));
            c.fill_circle(x + 8, y + 8, 4, rgb(0xff, 0xc0, 0x40));
            c.pixel(x + 3, y + 3, 0xffffff);
            c.pixel(x + 12, y + 12, 0xffffff);
        }
    }
}

/// Text at `scale` times the font size.
fn draw_scaled(c: &mut Canvas, x: i32, y: i32, text: &str, scale: i32, color: Color) {
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as i32 * font::WIDTH as i32 * scale;
        for (row, bits) in font::glyph(ch).iter().enumerate() {
            for col in 0..font::WIDTH as i32 {
                if bits & (0x80 >> col) != 0 {
                    c.fill_rect(
                        cx + col * scale,
                        y + row as i32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
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
