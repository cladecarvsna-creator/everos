//! Network settings kept on the disk in `/$network.conf`: the proxy and
//! the Wi-Fi networks EverOS remembers. One `key=value` per line.
//!
//! Wi-Fi passwords are not stored as typed: only the 256-bit key WPA2
//! makes from the password and the network name (as wpa_supplicant's
//! `psk=` does), which is enough to join that network and nothing else.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::sync::IrqMutex;

const FILE: &str = "/$network.conf";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProxyKind {
    Off,
    /// An HTTP proxy: CONNECT tunnels for HTTPS and other TCP, absolute
    /// URLs for plain HTTP.
    Http,
    Socks5,
}

impl ProxyKind {
    pub fn name(self) -> &'static str {
        match self {
            ProxyKind::Off => "off",
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5",
        }
    }

    pub fn parse(s: &str) -> Option<ProxyKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "" => Some(ProxyKind::Off),
            "http" | "https" => Some(ProxyKind::Http),
            "socks" | "socks5" => Some(ProxyKind::Socks5),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    /// Hosts reached directly, separated by `;`, with `*` wildcards.
    pub bypass: String,
}

pub const DEFAULT_BYPASS: &str = "localhost;127.*";

impl Default for Proxy {
    fn default() -> Self {
        Proxy {
            kind: ProxyKind::Off,
            host: String::new(),
            port: 3128,
            user: String::new(),
            pass: String::new(),
            bypass: String::from(DEFAULT_BYPASS),
        }
    }
}

#[derive(Clone, Default)]
pub struct Config {
    pub proxy: Proxy,
    /// The test adapter that stands in for a Wi-Fi card (see wifi.rs).
    pub wifi_virtual: bool,
    /// Wi-Fi radio on or off.
    pub wifi_on: bool,
    /// Remembered networks: name and key.
    pub wifi_saved: Vec<(String, [u8; 32])>,
    /// The network to join when the adapter comes up.
    pub wifi_last: String,
}

static CONFIG: IrqMutex<Option<Config>> = IrqMutex::new(None);

/// The settings, read from the disk the first time.
pub fn get() -> Config {
    if let Some(c) = CONFIG.lock().as_ref() {
        return c.clone();
    }
    let c = load();
    *CONFIG.lock() = Some(c.clone());
    c
}

/// Change the settings and save them.
pub fn update(f: impl FnOnce(&mut Config)) {
    let mut c = get();
    f(&mut c);
    save(&c);
    *CONFIG.lock() = Some(c);
}

fn load() -> Config {
    let mut c = Config {
        wifi_on: true,
        ..Config::default()
    };
    let Ok(data) = crate::fs::read(FILE) else {
        return c;
    };
    let text = String::from_utf8_lossy(&data);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_end_matches('\r');
        match key.trim() {
            "proxy" => c.proxy.kind = ProxyKind::parse(value).unwrap_or(ProxyKind::Off),
            "proxy_host" => c.proxy.host = value.to_string(),
            "proxy_port" => c.proxy.port = value.parse().unwrap_or(3128),
            "proxy_user" => c.proxy.user = value.to_string(),
            "proxy_pass" => c.proxy.pass = value.to_string(),
            "proxy_bypass" => c.proxy.bypass = value.to_string(),
            "wifi_virtual" => c.wifi_virtual = value == "1",
            "wifi_on" => c.wifi_on = value != "0",
            "wifi_last" => c.wifi_last = value.to_string(),
            "wifi" => {
                if let Some((ssid, key)) = value.rsplit_once('\t') {
                    if let Some(key) = unhex32(key) {
                        c.wifi_saved.push((ssid.to_string(), key));
                    }
                }
            }
            _ => {}
        }
    }
    c
}

fn save(c: &Config) {
    use core::fmt::Write;
    let mut out = String::new();
    let p = &c.proxy;
    let _ = writeln!(out, "proxy={}", p.kind.name());
    let _ = writeln!(out, "proxy_host={}", p.host);
    let _ = writeln!(out, "proxy_port={}", p.port);
    let _ = writeln!(out, "proxy_user={}", p.user);
    let _ = writeln!(out, "proxy_pass={}", p.pass);
    let _ = writeln!(out, "proxy_bypass={}", p.bypass);
    let _ = writeln!(out, "wifi_virtual={}", c.wifi_virtual as u8);
    let _ = writeln!(out, "wifi_on={}", c.wifi_on as u8);
    let _ = writeln!(out, "wifi_last={}", c.wifi_last);
    for (ssid, key) in &c.wifi_saved {
        let _ = write!(out, "wifi={}\t", ssid);
        for b in key {
            let _ = write!(out, "{:02x}", b);
        }
        out.push('\n');
    }
    if crate::fs::write(FILE, out.as_bytes()).is_err() {
        crate::serial::write_str("net: could not save /$network.conf\n");
    }
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}
