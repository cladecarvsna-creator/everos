//! A tiny command line: reads keys, echoes them and runs commands.

use crate::console::{self, Color, CONSOLE};
use crate::gui::{self, App};
use crate::interrupts;
use crate::keyboard::Key;
use crate::multiboot::BootInfo;
use crate::{print, println, users};

const MAX_LINE: usize = 250;

pub struct Shell {
    line: [char; MAX_LINE],
    len: usize,
    /// The previous command, recalled with the Up arrow.
    last: [char; MAX_LINE],
    last_len: usize,
}

impl Shell {
    pub const fn new() -> Self {
        Self {
            line: ['\0'; MAX_LINE],
            len: 0,
            last: ['\0'; MAX_LINE],
            last_len: 0,
        }
    }

    pub fn prompt(&self) {
        console::print_colored(Color::LightGreen, format_args!("everos"));
        console::print_colored(Color::LightGray, format_args!("> "));
    }

    pub fn on_key(&mut self, key: Key, boot: &BootInfo) {
        match key {
            Key::Char(c) if self.len < MAX_LINE => {
                self.line[self.len] = c;
                self.len += 1;
                print!("{}", c);
            }
            Key::Backspace if self.len > 0 => {
                self.len -= 1;
                CONSOLE.lock().backspace();
            }
            Key::Up => {
                self.erase_line();
                self.line = self.last;
                self.len = self.last_len;
                for &c in &self.line[..self.len] {
                    print!("{}", c);
                }
            }
            Key::Escape => self.erase_line(),
            Key::Ctrl('c') => {
                println!("^C");
                self.len = 0;
                self.prompt();
            }
            Key::Ctrl('l') => {
                CONSOLE.lock().clear();
                self.len = 0;
                self.prompt();
            }
            Key::Enter => {
                println!();
                if self.len > 0 {
                    self.last = self.line;
                    self.last_len = self.len;
                }
                let mut buf = [0u8; MAX_LINE * 4];
                let mut used = 0;
                for &c in &self.line[..self.len] {
                    used += c.encode_utf8(&mut buf[used..]).len();
                }
                self.len = 0;
                let line = core::str::from_utf8(&buf[..used]).unwrap_or("");
                self.run(line, boot);
                self.prompt();
            }
            _ => {}
        }
    }

    fn erase_line(&mut self) {
        let mut con = CONSOLE.lock();
        for _ in 0..self.len {
            con.backspace();
        }
        self.len = 0;
    }

    fn run(&mut self, line: &str, boot: &BootInfo) {
        let line = line.trim();
        let (command, args) = line.split_once(' ').unwrap_or((line, ""));
        match command {
            "" => {}
            "help" => {
                println!("Commands:");
                println!("  help    this list");
                println!("  clear   clear the screen (also Ctrl+L)");
                println!("  echo    print the arguments");
                println!("  info    screen, memory and uptime");
                println!("  paint   open Paint");
                println!("  calc    open the calculator");
                println!("  gfx     graphics demo");
                println!("  browser open the web browser (browser <address> goes there)");
                println!("  fetch   download a web page and show its title and links");
                println!("  exit    close the terminal window");
                println!("  whoami  show who is signed in; 'users' lists everyone");
                println!("  useradd add a user: useradd <name> [password]");
                println!("  passwd  set a password: passwd [<name>] <password>");
                println!("  lock    show the lock screen");
                println!("  colors  show the text colours");
                println!("  panic   test the kernel panic screen");
                println!("Keys: Alt+Shift switches EN/RU, Up recalls the last command.");
            }
            "clear" => CONSOLE.lock().clear(),
            "echo" => println!("{}", args),
            "info" => info(boot),
            "colors" => colors(),
            "paint" => open(App::Paint),
            "calc" => open(App::Calculator),
            "gfx" => open(App::Demo),
            "browser" | "web" => {
                if !args.trim().is_empty() {
                    gui::request_address(args.trim());
                }
                open(App::Browser);
            }
            "exit" => {
                if !gui::request_close(App::Terminal) {
                    println!("There is no desktop to go back to in text mode.");
                }
            }
            "fetch" => fetch(args.trim()),
            "whoami" => match users::current_name() {
                Some(name) => println!("{}", name.as_str()),
                None => println!("nobody"),
            },
            "users" => {
                for i in 0..users::count() {
                    let password = if users::has_password(i) {
                        "password set"
                    } else {
                        "no password"
                    };
                    if let Some(name) = users::name(i) {
                        println!("{:<16} {}", name.as_str(), password);
                    }
                }
            }
            "useradd" => useradd(args.trim()),
            "passwd" => passwd(args.trim()),
            "lock" => {
                if !gui::request_lock() {
                    println!("There is no lock screen in text mode.");
                }
            }
            "panic" => panic!("panic requested from the shell"),
            _ => console::print_colored(
                Color::LightRed,
                format_args!("unknown command: {} (try 'help')\n", command),
            ),
        }
    }
}

fn error(message: &str) {
    console::print_colored(Color::LightRed, format_args!("{}\n", message));
}

fn is_root() -> bool {
    users::current_name().is_some_and(|name| name.as_str() == "root")
}

fn useradd(args: &str) {
    let mut parts = args.split_whitespace();
    let (Some(name), password) = (parts.next(), parts.next()) else {
        println!("usage: useradd <name> [password]");
        return;
    };
    if !is_root() {
        error("only root can add users");
        return;
    }
    match users::add(name, password.unwrap_or("")) {
        Ok(()) => println!("added {}; they can sign in on the lock screen", name),
        Err(e) => error(e.message()),
    }
}

fn passwd(args: &str) {
    let mut parts = args.split_whitespace();
    let (name, password) = match (parts.next(), parts.next()) {
        (Some(name), Some(password)) => (Some(name), password),
        (Some(password), None) => (None, password),
        _ => {
            println!("usage: passwd [<name>] <password>");
            return;
        }
    };
    let me = users::current_name().unwrap_or_default();
    let name = name.unwrap_or(me.as_str());
    if name != me.as_str() && !is_root() {
        error("only root can change other users' passwords");
        return;
    }
    match users::set_password(name, password) {
        Ok(()) => println!("password changed for {}", name),
        Err(e) => error(e.message()),
    }
}

fn open(app: App) {
    if !gui::request_open(app) {
        println!("No graphics: GRUB started EverOS in text mode.");
    }
}

fn info(boot: &BootInfo) {
    let (cols, rows) = CONSOLE.lock().size();
    match &boot.framebuffer {
        Some(fb) => println!(
            "Screen:     {}x{}, {} bits per pixel, {}x{} characters",
            fb.width,
            fb.height,
            fb.bytes_per_pixel * 8,
            cols,
            rows
        ),
        None => println!("Screen:     VGA text mode, {}x{} characters", cols, rows),
    }
    println!(
        "Memory:     {} MiB above 1 MiB",
        boot.upper_memory_kib / 1024
    );
    println!("Bootloader: {}", boot.bootloader);
    let seconds = interrupts::ticks() / interrupts::TIMER_HZ;
    println!(
        "Uptime:     {}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
}

fn colors() {
    const NAMES: [(Color, &str); 16] = [
        (Color::Black, "black"),
        (Color::Blue, "blue"),
        (Color::Green, "green"),
        (Color::Cyan, "cyan"),
        (Color::Red, "red"),
        (Color::Magenta, "magenta"),
        (Color::Brown, "brown"),
        (Color::LightGray, "light gray"),
        (Color::DarkGray, "dark gray"),
        (Color::LightBlue, "light blue"),
        (Color::LightGreen, "light green"),
        (Color::LightCyan, "light cyan"),
        (Color::LightRed, "light red"),
        (Color::Pink, "pink"),
        (Color::Yellow, "yellow"),
        (Color::White, "white"),
    ];
    for (i, (color, name)) in NAMES.iter().enumerate() {
        CONSOLE.lock().set_color(Color::Black, *color);
        print!("    ");
        // black text would be invisible, so name it in dark gray
        let label = if *color == Color::Black {
            Color::DarkGray
        } else {
            *color
        };
        CONSOLE.lock().set_color(label, Color::Black);
        print!(" {:<12}", name);
        CONSOLE.lock().set_color(Color::LightGray, Color::Black);
        if i % 4 == 3 {
            println!();
        }
    }
}

/// Download a page in the terminal: the network test without the browser.
fn fetch(address: &str) {
    if address.is_empty() {
        println!("usage: fetch <address>");
        return;
    }
    if crate::net::init().is_none() {
        println!("No network card. Start QEMU with -nic user,model=e1000");
        return;
    }
    let Some(url) = crate::web::address_to_url(address) else {
        println!("bad address: {}", address);
        return;
    };
    println!("Loading {} ...", url);
    let page = crate::web::load(&url, None);
    let text: usize = page
        .doc
        .items
        .iter()
        .map(|i| match i {
            crate::web::html::Item::Text(t, _) => t.chars().count(),
            _ => 0,
        })
        .sum();
    // on one line, so the boot test can find it on the serial port
    println!(
        "fetch: \"{}\", {} characters of text, {} links",
        page.doc.title,
        text,
        page.doc.links.len()
    );
    for link in page.doc.links.iter().take(8) {
        println!("  {}", link);
    }
}
