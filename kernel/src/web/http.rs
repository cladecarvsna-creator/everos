//! HTTP/1.1 client over plain TCP or TLS 1.3 (https). Downloads a whole
//! page into memory and follows redirects.
//!
//! HTTPS encrypts the connection but does not check the server's
//! certificate: EverOS has no list of trusted certificate authorities.

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
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Download `url`, following redirects. With `form`, send it as a POST.
pub fn get(url: &Url, mut form: Option<&str>) -> Result<Response, String> {
    let mut url = url.clone();
    for _ in 0..MAX_REDIRECTS {
        let raw = fetch_raw(&url, form)?;
        // redirects after a POST are plain GETs
        form = None;
        let head = parse_head(&raw).ok_or("bad reply from server")?;
        if (300..400).contains(&head.status) {
            if let Some(location) = &head.location {
                url = url.join(location).ok_or("bad redirect")?;
                continue;
            }
        }
        let body = &raw[head.body_start..];
        let body = if head.chunked {
            dechunk(body)
        } else {
            match head.content_length {
                Some(n) => body[..n.min(body.len())].to_vec(),
                None => body.to_vec(),
            }
        };
        return Ok(Response {
            url,
            content_type: head.content_type,
            body,
        });
    }
    Err("too many redirects".to_string())
}

fn fetch_raw(url: &Url, form: Option<&str>) -> Result<Vec<u8>, String> {
    log(
        if form.is_some() { "POST " } else { "GET " },
        &url.to_string(),
    );
    let ip = net::resolve(&url.host)?;
    let mut stream = TcpStream::connect(ip, url.port)?;
    let host = if url.port == if url.https { 443 } else { 80 } {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Mozilla/5.0 (EverOS; x86_64) EverBrowser/0.1\r\n\
         Accept: text/html,text/plain;q=0.9,*/*;q=0.5\r\nAccept-Language: ru,en;q=0.8\r\n\
         Accept-Encoding: identity\r\nConnection: close\r\n",
        if form.is_some() { "POST" } else { "GET" },
        url.path,
        host
    );
    match form {
        Some(body) => {
            request.push_str(&format!(
                "Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            ));
        }
        None => request.push_str("\r\n"),
    }
    if !url.https {
        stream.write_all(request.as_bytes())?;
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
    tls.write(request.as_bytes())
        .and_then(|_| tls.flush())
        .map_err(|e| format!("TLS: {:?}", e))?;
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
