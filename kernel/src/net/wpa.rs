//! WPA2-Personal (IEEE 802.11i, "WPA2-PSK" with AES-CCMP): turning the
//! password into a key, and the 4-way handshake that proves both sides
//! know it and gives them fresh keys for the session.
//!
//! - PMK = PBKDF2-HMAC-SHA1(password, network name, 4096 rounds, 32 bytes)
//! - PTK = PRF-384(PMK, "Pairwise key expansion", both MAC addresses and
//!   both random nonces); its first 16 bytes (KCK) sign handshake frames,
//!   the next 16 (KEK) encrypt the group key, the last 16 (TK) encrypt data
//! - the group key comes in message 3, wrapped with AES key wrap (RFC 3394)
//!
//! The supplicant is the side a computer plays; the authenticator is the
//! access point's side, used by the virtual Wi-Fi adapter (wifi.rs) and
//! by the self-test.

use alloc::string::String;
use alloc::vec::Vec;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use sha1::{Digest, Sha1};

// ---- primitives --------------------------------------------------------------

pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha1::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// The 256-bit key WPA2 makes from a password (8 to 63 characters) and
/// the network name.
pub fn psk(passphrase: &str, ssid: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (block, chunk) in out.chunks_mut(20).enumerate() {
        let index = (block as u32 + 1).to_be_bytes();
        let mut u = hmac_sha1(passphrase.as_bytes(), &[ssid.as_bytes(), &index]);
        let mut t = u;
        for _ in 1..4096 {
            u = hmac_sha1(passphrase.as_bytes(), &[&u]);
            for (a, b) in t.iter_mut().zip(u) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    out
}

/// The 802.11i pseudo-random function: `len` bytes from HMAC-SHA1.
pub fn prf(key: &[u8], label: &str, data: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i = 0u8;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha1(key, &[label.as_bytes(), &[0], data, &[i]]));
        i += 1;
    }
    out.truncate(len);
    out
}

/// The session keys both sides derive.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

pub fn ptk(
    pmk: &[u8; 32],
    aa: &[u8; 6],
    spa: &[u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> Ptk {
    let mut data = Vec::with_capacity(76);
    let (a, b) = if aa < spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(a);
    data.extend_from_slice(b);
    let (a, b) = if anonce < snonce {
        (anonce, snonce)
    } else {
        (snonce, anonce)
    };
    data.extend_from_slice(a);
    data.extend_from_slice(b);
    let k = prf(pmk, "Pairwise key expansion", &data, 48);
    let mut p = Ptk {
        kck: [0; 16],
        kek: [0; 16],
        tk: [0; 16],
    };
    p.kck.copy_from_slice(&k[..16]);
    p.kek.copy_from_slice(&k[16..32]);
    p.tk.copy_from_slice(&k[32..48]);
    p
}

/// AES key wrap (RFC 3394). `data` is a multiple of 8 bytes.
pub fn aes_wrap(kek: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let aes = Aes128::new(kek.into());
    let n = data.len() / 8;
    let mut a = [0xa6u8; 8];
    let mut r: Vec<[u8; 8]> = data.chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in 0..6 {
        for (i, ri) in r.iter_mut().enumerate() {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(ri);
            aes.encrypt_block((&mut b).into());
            let t = (n * j + i + 1) as u64;
            a.copy_from_slice(&b[..8]);
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            ri.copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    for ri in r {
        out.extend_from_slice(&ri);
    }
    out
}

/// AES key unwrap; None if the integrity check fails.
pub fn aes_unwrap(kek: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 16 || !data.len().is_multiple_of(8) {
        return None;
    }
    let aes = Aes128::new(kek.into());
    let n = data.len() / 8 - 1;
    let mut a: [u8; 8] = data[..8].try_into().unwrap();
    let mut r: Vec<[u8; 8]> = data[8..].chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = (n * j + i + 1) as u64;
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.decrypt_block((&mut b).into());
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    if a != [0xa6; 8] {
        return None;
    }
    Some(r.concat())
}

// ---- EAPOL-Key frames --------------------------------------------------------------

/// Key information bits.
const VERSION_AES: u16 = 2; // HMAC-SHA1 MIC, AES key wrap
const PAIRWISE: u16 = 1 << 3;
const INSTALL: u16 = 1 << 6;
const ACK: u16 = 1 << 7;
const MIC: u16 = 1 << 8;
const SECURE: u16 = 1 << 9;
const ENCRYPTED: u16 = 1 << 12;

/// Offsets in an EAPOL-Key frame (4-byte EAPOL header first).
const INFO: usize = 5;
const REPLAY: usize = 9;
const NONCE: usize = 17;
const MIC_AT: usize = 81;
const DATA_LEN: usize = 97;
const HEADER: usize = 99;

/// The RSN element of a WPA2-PSK, CCMP-only network.
pub const RSN_IE: [u8; 22] = [
    0x30, 20, 1, 0, // version 1
    0x00, 0x0f, 0xac, 4, // group cipher CCMP
    1, 0, 0x00, 0x0f, 0xac, 4, // one pairwise cipher: CCMP
    1, 0, 0x00, 0x0f, 0xac, 2, // one key management: PSK
    0, 0, // capabilities
];

#[derive(Clone)]
pub struct KeyFrame {
    pub info: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub mic: [u8; 16],
    pub data: Vec<u8>,
}

impl KeyFrame {
    fn new(info: u16, replay: u64, nonce: [u8; 32], data: Vec<u8>) -> Self {
        KeyFrame {
            info: info | VERSION_AES,
            replay,
            nonce,
            mic: [0; 16],
            data,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let body = 95 + self.data.len();
        let mut f = Vec::with_capacity(4 + body);
        f.extend_from_slice(&[2, 3]); // 802.1X-2004, EAPOL-Key
        f.extend_from_slice(&(body as u16).to_be_bytes());
        f.push(2); // RSN key descriptor
        f.extend_from_slice(&self.info.to_be_bytes());
        f.extend_from_slice(&16u16.to_be_bytes()); // CCMP key length
        f.extend_from_slice(&self.replay.to_be_bytes());
        f.extend_from_slice(&self.nonce);
        f.extend_from_slice(&[0; 16]); // IV
        f.extend_from_slice(&[0; 8]); // RSC
        f.extend_from_slice(&[0; 8]); // reserved
        f.extend_from_slice(&self.mic);
        f.extend_from_slice(&(self.data.len() as u16).to_be_bytes());
        f.extend_from_slice(&self.data);
        f
    }

    pub fn decode(f: &[u8]) -> Option<KeyFrame> {
        if f.len() < HEADER || f[1] != 3 || f[4] != 2 {
            return None;
        }
        let len = u16::from_be_bytes([f[DATA_LEN], f[DATA_LEN + 1]]) as usize;
        let data = f.get(HEADER..HEADER + len)?.to_vec();
        Some(KeyFrame {
            info: u16::from_be_bytes([f[INFO], f[INFO + 1]]),
            replay: u64::from_be_bytes(f[REPLAY..REPLAY + 8].try_into().ok()?),
            nonce: f[NONCE..NONCE + 32].try_into().ok()?,
            mic: f[MIC_AT..MIC_AT + 16].try_into().ok()?,
            data,
        })
    }

    /// Encode, with the MIC computed over the frame with a zero MIC.
    fn signed(mut self, kck: &[u8; 16]) -> Vec<u8> {
        self.info |= MIC;
        self.mic = [0; 16];
        let mut f = self.encode();
        let mic = hmac_sha1(kck, &[&f]);
        f[MIC_AT..MIC_AT + 16].copy_from_slice(&mic[..16]);
        f
    }
}

/// Whether the frame `f` carries a correct MIC for `kck`.
fn mic_ok(f: &[u8], kck: &[u8; 16]) -> bool {
    if f.len() < HEADER {
        return false;
    }
    let mut zeroed = f.to_vec();
    zeroed[MIC_AT..MIC_AT + 16].fill(0);
    let mic = hmac_sha1(kck, &[&zeroed]);
    // compare without stopping at the first difference
    mic[..16]
        .iter()
        .zip(&f[MIC_AT..MIC_AT + 16])
        .fold(0u8, |d, (a, b)| d | (a ^ b))
        == 0
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A frame we didn't expect, or a malformed one.
    Protocol(&'static str),
    /// The access point's frame is signed with another key: it doesn't
    /// know our password (or someone is in the middle).
    BadMic,
}

// ---- the computer's side ---------------------------------------------------------

pub struct Supplicant {
    pmk: [u8; 32],
    aa: [u8; 6],
    spa: [u8; 6],
    snonce: [u8; 32],
    replay: Option<u64>,
    ptk: Option<Ptk>,
    pub gtk: Option<Vec<u8>>,
    pub done: bool,
}

impl Supplicant {
    pub fn new(pmk: [u8; 32], aa: [u8; 6], spa: [u8; 6], snonce: [u8; 32]) -> Self {
        Supplicant {
            pmk,
            aa,
            spa,
            snonce,
            replay: None,
            ptk: None,
            gtk: None,
            done: false,
        }
    }

    /// Handle a frame from the access point; returns our answer.
    pub fn on_frame(&mut self, f: &[u8]) -> Result<Vec<u8>, Error> {
        let k = KeyFrame::decode(f).ok_or(Error::Protocol("not an EAPOL-Key frame"))?;
        if k.info & ACK == 0 || k.info & PAIRWISE == 0 {
            return Err(Error::Protocol("unexpected key frame"));
        }
        if self.replay.is_some_and(|r| k.replay <= r) {
            return Err(Error::Protocol("replayed frame"));
        }
        if k.info & MIC == 0 {
            // message 1: the access point's nonce
            let ptk = ptk(&self.pmk, &self.aa, &self.spa, &k.nonce, &self.snonce);
            self.ptk = Some(ptk);
            self.replay = Some(k.replay);
            let reply = KeyFrame::new(PAIRWISE, k.replay, self.snonce, RSN_IE.to_vec());
            return Ok(reply.signed(&ptk.kck));
        }
        // message 3: signed, with the group key
        let ptk = self
            .ptk
            .ok_or(Error::Protocol("message 3 before message 1"))?;
        if !mic_ok(f, &ptk.kck) {
            return Err(Error::BadMic);
        }
        if k.info & INSTALL == 0 || k.info & ENCRYPTED == 0 {
            return Err(Error::Protocol("message 3 without keys"));
        }
        self.replay = Some(k.replay);
        let data = aes_unwrap(&ptk.kek, &k.data).ok_or(Error::Protocol("bad key data"))?;
        self.gtk = find_gtk(&data);
        if self.gtk.is_none() {
            return Err(Error::Protocol("no group key"));
        }
        self.done = true;
        let reply = KeyFrame::new(PAIRWISE | SECURE, k.replay, [0; 32], Vec::new());
        Ok(reply.signed(&ptk.kck))
    }
}

/// The group key from the key data elements (GTK KDE: dd, len, 00-0f-ac, 1).
fn find_gtk(data: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    while i + 2 <= data.len() {
        let (id, len) = (data[i], data[i + 1] as usize);
        let body = data.get(i + 2..i + 2 + len)?;
        if id == 0xdd && len >= 6 && body[..4] == [0x00, 0x0f, 0xac, 1] {
            return Some(body[6..].to_vec());
        }
        if id == 0xdd && len == 0 {
            break; // padding
        }
        i += 2 + len;
    }
    None
}

// ---- the access point's side -----------------------------------------------------

pub struct Authenticator {
    pmk: [u8; 32],
    aa: [u8; 6],
    spa: [u8; 6],
    anonce: [u8; 32],
    gtk: [u8; 16],
    replay: u64,
    ptk: Option<Ptk>,
    pub done: bool,
}

impl Authenticator {
    pub fn new(pmk: [u8; 32], aa: [u8; 6], spa: [u8; 6], anonce: [u8; 32], gtk: [u8; 16]) -> Self {
        Authenticator {
            pmk,
            aa,
            spa,
            anonce,
            gtk,
            replay: 1,
            ptk: None,
            done: false,
        }
    }

    /// Message 1.
    pub fn start(&mut self) -> Vec<u8> {
        KeyFrame::new(PAIRWISE | ACK, self.replay, self.anonce, Vec::new()).encode()
    }

    /// Handle message 2 or 4. Message 2 with a bad MIC means the other
    /// side used another password: real access points then say nothing.
    pub fn on_frame(&mut self, f: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let k = KeyFrame::decode(f).ok_or(Error::Protocol("not an EAPOL-Key frame"))?;
        if k.replay != self.replay {
            return Err(Error::Protocol("wrong replay counter"));
        }
        if self.ptk.is_none() {
            let ptk = ptk(&self.pmk, &self.aa, &self.spa, &self.anonce, &k.nonce);
            if !mic_ok(f, &ptk.kck) {
                return Err(Error::BadMic);
            }
            self.ptk = Some(ptk);
            // message 3: our RSN element and the wrapped group key
            let mut data = RSN_IE.to_vec();
            data.extend_from_slice(&[0xdd, 22, 0x00, 0x0f, 0xac, 1, 1, 0]);
            data.extend_from_slice(&self.gtk);
            data.push(0xdd); // pad to a multiple of 8
            while !data.len().is_multiple_of(8) {
                data.push(0);
            }
            let data = aes_wrap(&ptk.kek, &data);
            self.replay += 1;
            let m3 = KeyFrame::new(
                PAIRWISE | INSTALL | ACK | SECURE | ENCRYPTED,
                self.replay,
                self.anonce,
                data,
            );
            return Ok(Some(m3.signed(&ptk.kck)));
        }
        let ptk = self.ptk.unwrap();
        if !mic_ok(f, &ptk.kck) {
            return Err(Error::BadMic);
        }
        self.done = true;
        Ok(None)
    }
}

// ---- self-test -------------------------------------------------------------------------

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// Known answers from the standards, then a whole handshake with the
/// right password and one with a wrong password. Returns what failed.
pub fn selftest() -> Result<(), String> {
    let check = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(String::from(what))
        }
    };
    // RFC 2202 test case 1
    check(
        hmac_sha1(&[0x0b; 20], &[b"Hi There"])[..]
            == hex("b617318655057264e28bc0b6fb378c8ef146be00")[..],
        "HMAC-SHA1",
    )?;
    // IEEE 802.11i-2004, H.4
    check(
        psk("password", "IEEE")[..]
            == hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")[..],
        "PBKDF2 (password, IEEE)",
    )?;
    check(
        psk("ThisIsAPassword", "ThisIsASSID")[..]
            == hex("0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af")[..],
        "PBKDF2 (ThisIsAPassword)",
    )?;
    // RFC 3394 section 4.1
    let kek: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
    let wrapped = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    check(
        aes_wrap(&kek, &hex("00112233445566778899aabbccddeeff")) == wrapped,
        "AES key wrap",
    )?;
    check(
        aes_unwrap(&kek, &wrapped) == Some(hex("00112233445566778899aabbccddeeff")),
        "AES key unwrap",
    )?;

    // a whole handshake
    let aa = [0x02, 0xe5, 0x00, 0x00, 0x00, 0x01];
    let spa = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let run = |client: &str| -> Result<bool, String> {
        let mut ap = Authenticator::new(psk("everos2026", "Test"), aa, spa, [7; 32], [9; 16]);
        let mut sta = Supplicant::new(psk(client, "Test"), aa, spa, [3; 32]);
        let m2 = sta
            .on_frame(&ap.start())
            .map_err(|_| String::from("message 1"))?;
        let m3 = match ap.on_frame(&m2) {
            Ok(Some(m3)) => m3,
            Err(Error::BadMic) => return Ok(false),
            _ => return Err(String::from("message 2")),
        };
        let m4 = sta.on_frame(&m3).map_err(|_| String::from("message 3"))?;
        ap.on_frame(&m4).map_err(|_| String::from("message 4"))?;
        Ok(ap.done && sta.done && sta.gtk.as_deref() == Some(&[9u8; 16][..]))
    };
    check(run("everos2026")?, "handshake with the right password")?;
    check(!run("wrong-password")?, "handshake with a wrong password")?;
    Ok(())
}
