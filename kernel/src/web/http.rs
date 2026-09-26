//! HTTP/1.1 client over plain TCP or TLS 1.3 (https). Downloads a whole
//! page into memory and follows redirects.
//!
//! HTTPS encrypts the connection but does not check the server's
//! certificate: EverOS has no list of trusted certificate authorities.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use embedded_tls::blocking::{
    Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider,
};
use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};

use super::url::Url;
use crate::net::{self, TcpStream};

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 8;

pub struct Response {
    /// Where the page came from, after redirects.
    pub url: Url,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Download `url`, following redirects. With `form`, send it as a POST.
pub fn get(url: &Url, form: Option<&str>) -> Result<Response, String> {
    match form {
        Some(f) => request(
            "POST",
            url,
            Some(("application/x-www-form-urlencoded", f.as_bytes())),
        ),
        None => request("GET", url, None),
    }
}

/// Send a request with any method and an optional (content type, body).
pub fn request(
    method: &str,
    url: &Url,
    mut body: Option<(&str, &[u8])>,
) -> Result<Response, String> {
    let mut url = url.clone();
    let mut method = method.to_string();
    for _ in 0..MAX_REDIRECTS {
        let raw = fetch_raw(&method, &url, body)?;
        let head = parse_head(&raw).ok_or("bad reply from server")?;
        for c in &head.cookies {
            store_cookie(&url.host, c);
        }
        if (300..400).contains(&head.status) && head.status != 304 {
            if let Some(location) = &head.location {
                url = url.join(location).ok_or("bad redirect")?;
                // redirects after a POST are plain GETs
                if head.status != 307 && head.status != 308 {
                    method = String::from("GET");
                    body = None;
                }
                continue;
            }
        }
        let data = &raw[head.body_start..];
        let data = if head.chunked {
            dechunk(data)
        } else {
            match head.content_length {
                Some(n) => data[..n.min(data.len())].to_vec(),
                None => data.to_vec(),
            }
        };
        return Ok(Response {
            url,
            status: head.status,
            content_type: head.content_type,
            body: data,
        });
    }
    Err("too many redirects".to_string())
}

/// Cookies by host: name and value.
static COOKIES: crate::sync::IrqMutex<BTreeMap<String, Vec<(String, String)>>> =
    crate::sync::IrqMutex::new(BTreeMap::new());

/// Store a Set-Cookie header (or a document.cookie assignment) for a host.
pub fn store_cookie(host: &str, header: &str) {
    let first = header.split(';').next().unwrap_or("");
    let Some((name, value)) = first.split_once('=') else {
        return;
    };
    let (name, value) = (name.trim().to_string(), value.trim().to_string());
    let expired = header.to_ascii_lowercase().contains("max-age=0");
    // cookies set for the parent domain go to the host we talk to
    let mut jar = COOKIES.lock();
    let list = jar.entry(host.to_string()).or_default();
    list.retain(|(n, _)| *n != name);
    if !expired && list.len() < 64 {
        list.push((name, value));
    }
}

/// The Cookie header value for a host.
pub fn cookies(host: &str) -> String {
    let jar = COOKIES.lock();
    let mut out = String::new();
    if let Some(list) = jar.get(host) {
        for (n, v) in list {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(n);
            out.push('=');
            out.push_str(v);
        }
    }
    out
}

fn fetch_raw(method: &str, url: &Url, body: Option<(&str, &[u8])>) -> Result<Vec<u8>, String> {
    log(&format!("{} ", method), &url.to_string());
    let ip = net::resolve(&url.host)?;
    let mut stream = TcpStream::connect(ip, url.port)?;
    let host = if url.port == if url.https { 443 } else { 80 } {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Mozilla/5.0 (EverOS; x86_64) EverBrowser/0.2\r\n\
         Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\nAccept-Language: ru,en;q=0.8\r\n\
         Accept-Encoding: identity\r\nConnection: close\r\n",
        method, url.path, host
    );
    let cookie = cookies(&url.host);
    if !cookie.is_empty() {
        request.push_str(&format!("Cookie: {}\r\n", cookie));
    }
    let mut request = request.into_bytes();
    match body {
        Some((ty, data)) => {
            request.extend_from_slice(
                format!(
                    "Content-Type: {}\r\nContent-Length: {}\r\n\r\n",
                    ty,
                    data.len()
                )
                .as_bytes(),
            );
            request.extend_from_slice(data);
        }
        None => request.extend_from_slice(b"\r\n"),
    }
    if !url.https {
        stream.write_all(&request)?;
        return read_response(|buf| stream.read(buf));
    }

    let mut read_buf = vec![0u8; 16 * 1024 + 512];
    let mut write_buf = vec![0u8; 16 * 1024 + 512];
    let config = TlsConfig::new()
        .with_server_name(&url.host)
        .enable_rsa_signatures();
    let mut tls: TlsConnection<'_, TcpStream, Aes128GcmSha256> =
        TlsConnection::new(stream, &mut read_buf, &mut write_buf);
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(Rng::new()),
    ))
    .map_err(|e| format!("TLS handshake failed: {:?}", e))?;
    let mut sent = 0;
    while sent < request.len() {
        let n = tls
            .write(&request[sent..])
            .map_err(|e| format!("TLS: {:?}", e))?;
        tls.flush().map_err(|e| format!("TLS: {:?}", e))?;
        if n == 0 {
            return Err("TLS: could not send".to_string());
        }
        sent += n;
    }
    read_response(|buf| match tls.read(buf) {
        Ok(n) => Ok(n),
        // the server closing the connection ends the page
        Err(embedded_tls::TlsError::ConnectionClosed) => Ok(0),
        Err(e) => Err(format!("TLS: {:?}", e)),
    })
}

/// Read until the connection closes or the body is complete.
fn read_response(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, String>,
) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match read(&mut buf) {
            Ok(n) => n,
            // keep what arrived if the connection broke late
            Err(e) if parse_head(&data).is_some() => {
                log("read ended: ", &e);
                0
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() > MAX_BODY {
            return Err("page too large".to_string());
        }
        if complete(&data) {
            break;
        }
    }
    if data.is_empty() {
        return Err("empty reply".to_string());
    }
    Ok(data)
}

fn complete(data: &[u8]) -> bool {
    let Some(head) = parse_head(data) else {
        return false;
    };
    let body = &data[head.body_start..];
    if head.status == 204 || head.status == 304 {
        return true;
    }
    if head.chunked {
        return body.ends_with(b"0\r\n\r\n");
    }
    head.content_length.is_some_and(|n| body.len() >= n)
}

struct Head {
    status: u16,
    content_type: String,
    location: Option<String>,
    content_length: Option<usize>,
    chunked: bool,
    body_start: usize,
    cookies: Vec<String>,
}

fn parse_head(data: &[u8]) -> Option<Head> {
    let end = data.windows(4).position(|w| w == b"\r\n\r\n")?;
    let text = String::from_utf8_lossy(&data[..end]);
    let mut lines = text.split("\r\n");
    let status_line = lines.next()?;
    let status = status_line.split(' ').nth(1)?.parse().ok()?;
    let mut head = Head {
        status,
        content_type: String::new(),
        location: None,
        content_length: None,
        chunked: false,
        body_start: end + 4,
        cookies: Vec::new(),
    };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => head.content_type = value.to_ascii_lowercase(),
            "location" => head.location = Some(value.to_string()),
            "content-length" => head.content_length = value.parse().ok(),
            "transfer-encoding" => head.chunked = value.to_ascii_lowercase().contains("chunked"),
            "set-cookie" => head.cookies.push(value.to_string()),
            _ => {}
        }
    }
    Some(head)
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = body.windows(2).position(|w| w == b"\r\n") {
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        body = &body[take..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    out
}

fn log(what: &str, detail: &str) {
    crate::serial::write_str("\nbrowser: ");
    crate::serial::write_str(what);
    crate::serial::write_str(detail);
    crate::serial::write_str("\n");
}

/// Random numbers for TLS keys: SHA-256 over a counter and a seed taken
/// from the CPU's time stamp counter, which jitters between runs.
struct Rng {
    seed: [u8; 32],
    counter: u64,
}

impl Rng {
    fn new() -> Self {
        let mut h = Sha256::new();
        for _ in 0..64 {
            h.update(unsafe { core::arch::x86_64::_rdtsc() }.to_le_bytes());
            h.update(crate::interrupts::ticks().to_le_bytes());
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }
        Rng {
            seed: h.finalize().into(),
            counter: 0,
        }
    }
}

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(32) {
            let mut h = Sha256::new();
            h.update(self.seed);
            h.update(self.counter.to_le_bytes());
            h.update(unsafe { core::arch::x86_64::_rdtsc() }.to_le_bytes());
            self.counter += 1;
            let out = h.finalize();
            chunk.copy_from_slice(&out[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for Rng {}
