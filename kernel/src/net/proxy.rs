//! Connections through a proxy server, for the browser and Telegram.
//!
//! An HTTP proxy is asked to open a tunnel with `CONNECT host:port`
//! (plain HTTP pages are instead sent to it as whole URLs, see
//! web/http.rs). A SOCKS5 proxy (RFC 1928) is asked the same in binary,
//! with a user name and password if set (RFC 1929). Both get the host's
//! name, not its address, so the proxy looks names up itself.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::config::{self, Proxy, ProxyKind};
use super::TcpStream;

/// How to reach a host.
pub enum Route {
    Direct,
    Via(Proxy),
}

/// Whether connections to `host` go through the proxy.
pub fn route(host: &str) -> Route {
    let p = config::get().proxy;
    if p.kind == ProxyKind::Off || p.host.is_empty() || bypassed(&p.bypass, host) {
        Route::Direct
    } else {
        Route::Via(p)
    }
}

/// Whether `host` matches one of the `;`-separated patterns.
fn bypassed(list: &str, host: &str) -> bool {
    list.split([';', ',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .any(|p| wildcard(&p.to_ascii_lowercase(), &host.to_ascii_lowercase()))
}

/// `*` matches any run of characters.
fn wildcard(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    if !text.starts_with(head) {
        return false;
    }
    let text = &text[head.len()..];
    (0..=text.len()).any(|i| text.is_char_boundary(i) && wildcard(rest, &text[i..]))
}

/// Open a TCP connection to `host:port`, through the proxy if one is
/// set for it.
pub fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    match route(host) {
        Route::Direct => {
            let ip = super::resolve(host)?;
            TcpStream::connect(ip, port)
        }
        Route::Via(p) => tunnel(&p, host, port),
    }
}

/// A plain connection to the proxy server itself.
pub fn connect_to_proxy(p: &Proxy) -> Result<TcpStream, String> {
    let ip = super::resolve(&p.host).map_err(|e| format!("proxy: {}", e))?;
    TcpStream::connect(ip, p.port).map_err(|e| format!("proxy {}:{}: {}", p.host, p.port, e))
}

/// A connection to `host:port` through the proxy `p`.
pub fn tunnel(p: &Proxy, host: &str, port: u16) -> Result<TcpStream, String> {
    let mut s = connect_to_proxy(p)?;
    match p.kind {
        ProxyKind::Http => http_connect(&mut s, p, host, port)?,
        ProxyKind::Socks5 => socks5_connect(&mut s, p, host, port)?,
        ProxyKind::Off => {}
    }
    crate::serial::write_str(&format!(
        "proxy: tunnel to {}:{} via {} {}:{}\n",
        host,
        port,
        p.kind.name(),
        p.host,
        p.port
    ));
    Ok(s)
}

/// The Proxy-Authorization header line, if a user name is set.
pub fn auth_header(p: &Proxy) -> Option<String> {
    if p.user.is_empty() {
        return None;
    }
    let pair = format!("{}:{}", p.user, p.pass);
    Some(format!(
        "Proxy-Authorization: Basic {}\r\n",
        base64(pair.as_bytes())
    ))
}

fn http_connect(s: &mut TcpStream, p: &Proxy, host: &str, port: u16) -> Result<(), String> {
    let mut req = format!(
        "CONNECT {h}:{port} HTTP/1.1\r\nHost: {h}:{port}\r\n\
         User-Agent: EverOS\r\nProxy-Connection: keep-alive\r\n",
        h = host,
        port = port
    );
    if let Some(a) = auth_header(p) {
        req.push_str(&a);
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    // read the reply one byte at a time: whatever comes after it already
    // belongs to the tunnel
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        if s.read(&mut b)? == 0 {
            return Err(String::from("proxy closed the connection"));
        }
        head.push(b[0]);
        if head.len() > 16 * 1024 {
            return Err(String::from("proxy: reply too long"));
        }
    }
    let line = head.split(|&b| b == b'\r').next().unwrap_or(&[]);
    let line = String::from_utf8_lossy(line);
    let code: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    match code {
        200..=299 => Ok(()),
        407 => Err(String::from("proxy: needs a user name and password (407)")),
        _ => Err(format!("proxy refused the connection: {}", line.trim())),
    }
}

fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> Result<(), String> {
    let mut got = 0;
    while got < buf.len() {
        let n = s.read(&mut buf[got..])?;
        if n == 0 {
            return Err(String::from("proxy closed the connection"));
        }
        got += n;
    }
    Ok(())
}

fn socks5_connect(s: &mut TcpStream, p: &Proxy, host: &str, port: u16) -> Result<(), String> {
    let with_login = !p.user.is_empty();
    // offer "no login", and user/password when we have one
    if with_login {
        s.write_all(&[5, 2, 0, 2])?;
    } else {
        s.write_all(&[5, 1, 0])?;
    }
    let mut reply = [0u8; 2];
    read_exact(s, &mut reply)?;
    if reply[0] != 5 {
        return Err(String::from("not a SOCKS5 proxy"));
    }
    match reply[1] {
        0 => {}
        2 if with_login => {
            let (u, pw) = (p.user.as_bytes(), p.pass.as_bytes());
            if u.len() > 255 || pw.len() > 255 {
                return Err(String::from("proxy: user name or password too long"));
            }
            let mut msg = Vec::with_capacity(3 + u.len() + pw.len());
            msg.push(1);
            msg.push(u.len() as u8);
            msg.extend_from_slice(u);
            msg.push(pw.len() as u8);
            msg.extend_from_slice(pw);
            s.write_all(&msg)?;
            read_exact(s, &mut reply)?;
            if reply[1] != 0 {
                return Err(String::from("proxy: wrong user name or password"));
            }
        }
        0xff => return Err(String::from("proxy: needs a user name and password")),
        m => return Err(format!("proxy: unsupported login method {}", m)),
    }
    // CONNECT to a host name, or an IPv4 address given as digits
    let mut msg = alloc::vec![5, 1, 0];
    match super::parse_ipv4(host) {
        Some(ip) => {
            msg.push(1);
            msg.extend_from_slice(&ip.octets());
        }
        None => {
            if host.len() > 255 {
                return Err(String::from("host name too long"));
            }
            msg.push(3);
            msg.push(host.len() as u8);
            msg.extend_from_slice(host.as_bytes());
        }
    }
    msg.extend_from_slice(&port.to_be_bytes());
    s.write_all(&msg)?;
    let mut head = [0u8; 4];
    read_exact(s, &mut head)?;
    if head[1] != 0 {
        let why = match head[1] {
            2 => "not allowed by the proxy's rules",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "timed out",
            _ => "failed",
        };
        return Err(format!("proxy: {} ({})", why, head[1]));
    }
    // the address the proxy connected from, which we don't need
    let rest = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut n = [0u8; 1];
            read_exact(s, &mut n)?;
            n[0] as usize
        }
        _ => return Err(String::from("proxy: bad reply")),
    };
    let mut skip = alloc::vec![0u8; rest + 2];
    read_exact(s, &mut skip)?;
    Ok(())
}

pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Check the proxy by opening a tunnel to `host:port`. For Settings'
/// "Check" button and the shell.
pub fn check(p: &Proxy, host: &str, port: u16) -> Result<(), String> {
    if p.kind == ProxyKind::Off {
        return Err(String::from("the proxy is off"));
    }
    if p.host.is_empty() {
        return Err(String::from("no proxy address"));
    }
    tunnel(p, host, port).map(drop)
}

/// Checks that need no network, for `proxy selftest`.
pub fn selftest() -> bool {
    base64(b"user:pass") == "dXNlcjpwYXNz"
        && base64(b"a") == "YQ=="
        && base64(b"ab") == "YWI="
        && wildcard("127.*", "127.0.0.1")
        && wildcard("*.local", "pc.local")
        && !wildcard("*.local", "local")
        && bypassed("localhost; 10.0.2.*", "10.0.2.2")
        && !bypassed("localhost;127.*", "example.com")
}
