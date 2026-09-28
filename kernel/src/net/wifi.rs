//! Wi-Fi: finding networks, joining them with WPA2-Personal (wpa.rs),
//! remembering them, and joining the last one again after a restart.
//!
//! What actually sends radio frames is an adapter. EverOS has no driver
//! for a real Wi-Fi chip yet: they need their firmware loaded and a full
//! 802.11 MAC, and QEMU emulates no Wi-Fi card to develop one against.
//! A real card is still recognised on the PCI bus and named in Settings.
//!
//! To try everything above the driver, there is a virtual adapter
//! (Settings > Network & internet, or `wifi virtual on`). It pretends to
//! hear a few access points. Joining one runs the real 4-way handshake
//! against an access point simulated here, so a wrong password fails the
//! way it would on a real network. Once joined, packets go out through
//! the wired card (e1000), and while no network is joined the wired card
//! is kept silent, as if the cable were the radio.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::config;
use super::wpa::{self, Authenticator, Supplicant};
use crate::interrupts;
use crate::sync::IrqMutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    Open,
    Wpa2,
}

#[derive(Clone, Debug)]
pub struct Network {
    pub ssid: String,
    pub bssid: [u8; 6],
    /// 0 to 100.
    pub signal: u8,
    pub security: Security,
    pub saved: bool,
}

impl Network {
    /// Bars in the icon, 1 to 4.
    pub fn bars(&self) -> u8 {
        bars(self.signal)
    }
}

pub fn bars(signal: u8) -> u8 {
    match signal {
        0..=30 => 1,
        31..=55 => 2,
        56..=75 => 3,
        _ => 4,
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Adapter {
    None,
    /// A Wi-Fi card EverOS has no driver for: its name.
    Unsupported(String),
    Virtual,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    Off,
    Disconnected,
    Connecting(String),
    Connected {
        ssid: String,
        signal: u8,
    },
    /// Joining failed: the network and why.
    Failed(String, String),
}

/// Joining a network, one step per poll.
struct Job {
    ssid: String,
    bssid: [u8; 6],
    key: Option<[u8; 32]>,
    /// Remember the network once joined.
    save: bool,
    step: Step,
    next_at: u64,
    deadline: u64,
    supplicant: Option<Supplicant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Associate,
    Handshake,
    Done,
}

struct Wifi {
    adapter: Adapter,
    detected: bool,
    radio: VirtualRadio,
    state: State,
    job: Option<Job>,
    networks: Vec<Network>,
    scanned_at: Option<u64>,
    auto_tried: bool,
    /// The last failed join, until the window showing it takes it.
    failure: Option<(String, String)>,
}

static WIFI: IrqMutex<Wifi> = IrqMutex::new(Wifi {
    adapter: Adapter::None,
    detected: false,
    radio: VirtualRadio::new(),
    state: State::Off,
    job: None,
    networks: Vec::new(),
    scanned_at: None,
    auto_tried: false,
    failure: None,
});

const HZ: u64 = interrupts::TIMER_HZ;

fn log(text: &str) {
    crate::serial::write_str("wifi: ");
    crate::serial::write_str(text);
    crate::serial::write_str("\n");
}

/// Wi-Fi cards by PCI vendor, for naming ones we can't drive.
const VENDORS: [(u16, &str); 6] = [
    (0x8086, "Intel"),
    (0x168c, "Qualcomm Atheros"),
    (0x17cb, "Qualcomm"),
    (0x10ec, "Realtek"),
    (0x14e4, "Broadcom"),
    (0x14c3, "MediaTek"),
];

/// Look for a Wi-Fi card on the PCI bus: network controllers of the
/// "other" kind (class 2, subclass 0x80) from Wi-Fi chip makers.
fn detect() -> Adapter {
    for (_, vendor, device, class) in crate::pci::all() {
        if class != (0x02, 0x80) {
            continue;
        }
        if let Some((_, name)) = VENDORS.iter().find(|(v, _)| *v == vendor) {
            return Adapter::Unsupported(alloc::format!(
                "{} Wi-Fi ({:04x}:{:04x})",
                name,
                vendor,
                device
            ));
        }
    }
    Adapter::None
}

impl Wifi {
    fn ensure_detected(&mut self) {
        if self.detected {
            return;
        }
        self.detected = true;
        let cfg = config::get();
        let found = detect();
        if let Adapter::Unsupported(name) = &found {
            log(&alloc::format!("found {}, no driver for it yet", name));
        }
        self.adapter = if cfg.wifi_virtual {
            Adapter::Virtual
        } else {
            found
        };
        self.state = if self.adapter == Adapter::Virtual && cfg.wifi_on {
            State::Disconnected
        } else {
            State::Off
        };
    }

    fn usable(&self) -> bool {
        self.adapter == Adapter::Virtual
    }

    fn scan(&mut self) {
        let now = interrupts::ticks();
        if !self.usable() || self.state == State::Off {
            self.networks.clear();
            return;
        }
        if self.scanned_at.is_some_and(|t| now - t < 5 * HZ) {
            return;
        }
        self.scanned_at = Some(now);
        let saved = config::get().wifi_saved;
        self.networks = self
            .radio
            .scan(now)
            .into_iter()
            .map(|mut n| {
                n.saved = saved.iter().any(|(s, _)| *s == n.ssid);
                n
            })
            .collect();
        // strongest first, the joined one on top
        let joined = match &self.state {
            State::Connected { ssid, .. } | State::Connecting(ssid) => Some(ssid.clone()),
            _ => None,
        };
        self.networks.sort_by_key(|n| {
            (
                joined.as_deref() != Some(n.ssid.as_str()),
                255 - n.signal as u32,
            )
        });
    }

    fn start(&mut self, net: &Network, key: Option<[u8; 32]>, save: bool) {
        let now = interrupts::ticks();
        log(&alloc::format!("joining \"{}\"", net.ssid));
        self.radio.leave();
        super::set_blocked(true);
        self.job = Some(Job {
            ssid: net.ssid.clone(),
            bssid: net.bssid,
            key,
            save,
            step: Step::Associate,
            next_at: now + HZ / 3,
            deadline: now + 8 * HZ,
            supplicant: None,
        });
        self.state = State::Connecting(net.ssid.clone());
        self.scanned_at = None;
    }

    fn fail(&mut self, ssid: &str, why: &str) {
        log(&alloc::format!("could not join \"{}\": {}", ssid, why));
        self.job = None;
        self.radio.leave();
        super::set_blocked(true);
        self.state = State::Failed(ssid.to_string(), why.to_string());
        self.failure = Some((ssid.to_string(), why.to_string()));
        // go back to the network we were on, as Windows does
        if config::get().wifi_last != ssid {
            self.auto_tried = false;
        }
    }

    fn step(&mut self) {
        let now = interrupts::ticks();
        let Some(job) = self.job.as_mut() else {
            return;
        };
        if now < job.next_at {
            return;
        }
        let ssid = job.ssid.clone();
        if now > job.deadline {
            let why = if job.step == Step::Handshake {
                "The password is wrong"
            } else {
                "The network did not answer"
            };
            return self.fail(&ssid, why);
        }
        let mac = super::mac().unwrap_or([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        match job.step {
            Step::Associate => {
                let Some(open) = self.radio.associate(job.bssid, mac, job.key, now) else {
                    return self.fail(&ssid, "The network is out of range");
                };
                if open {
                    job.step = Step::Done;
                    job.next_at = now + HZ / 5;
                } else {
                    let Some(key) = job.key else {
                        return self.fail(&ssid, "This network needs a password");
                    };
                    job.supplicant = Some(Supplicant::new(key, job.bssid, mac, nonce()));
                    job.step = Step::Handshake;
                    job.next_at = now + HZ / 10;
                    // an access point that doesn't like our message 2
                    // just says nothing
                    job.deadline = now + 3 * HZ;
                }
            }
            Step::Handshake => {
                // move frames between the access point and us
                job.next_at = now + HZ / 20;
                let sup = job.supplicant.as_mut().unwrap();
                while let Some(frame) = self.radio.take_from_ap() {
                    match sup.on_frame(&frame) {
                        Ok(reply) => self.radio.send_to_ap(&reply),
                        Err(wpa::Error::BadMic) => {
                            return self.fail(&ssid, "The access point used another key")
                        }
                        Err(wpa::Error::Protocol(e)) => {
                            log(&alloc::format!("ignored a frame: {}", e));
                        }
                    }
                }
                if sup.done && self.radio.handshake_done() {
                    log("4-way handshake done, keys installed");
                    job.step = Step::Done;
                }
            }
            Step::Done => {
                let job = self.job.take().unwrap();
                log(&alloc::format!("joined \"{}\"", job.ssid));
                if job.save {
                    let key = job.key.unwrap_or([0; 32]);
                    config::update(|c| {
                        c.wifi_saved.retain(|(s, _)| *s != job.ssid);
                        c.wifi_saved.push((job.ssid.clone(), key));
                        c.wifi_last = job.ssid.clone();
                    });
                }
                let signal = self.radio.signal(job.bssid, now);
                self.state = State::Connected {
                    ssid: job.ssid,
                    signal,
                };
                self.scanned_at = None;
                // a new network: ask for an address again
                super::set_blocked(false);
            }
        }
    }

    fn disconnect(&mut self) {
        if let State::Connected { ssid, .. } | State::Connecting(ssid) = &self.state {
            log(&alloc::format!("left \"{}\"", ssid));
        }
        self.job = None;
        self.radio.leave();
        super::set_blocked(self.usable());
        self.state = if self.state == State::Off {
            State::Off
        } else {
            State::Disconnected
        };
        self.scanned_at = None;
    }
}

/// A fresh random nonce.
fn nonce() -> [u8; 32] {
    let t = unsafe { core::arch::x86_64::_rdtsc() };
    let a = wpa::hmac_sha1(
        &t.to_le_bytes(),
        &[b"EverOS nonce", &interrupts::ticks().to_le_bytes()],
    );
    let t2 = unsafe { core::arch::x86_64::_rdtsc() };
    let b = wpa::hmac_sha1(&a, &[&t2.to_le_bytes()]);
    let mut n = [0u8; 32];
    n[..20].copy_from_slice(&a);
    n[20..].copy_from_slice(&b[..12]);
    n
}

// ---- the public side -----------------------------------------------------------

/// Find the adapter; join the last network if it is in range.
pub fn init() {
    let mut w = WIFI.lock();
    w.ensure_detected();
    super::set_blocked(w.usable());
}

pub fn adapter() -> Adapter {
    let mut w = WIFI.lock();
    w.ensure_detected();
    w.adapter.clone()
}

/// None when there is no Wi-Fi to use.
pub fn state() -> Option<State> {
    let mut w = WIFI.lock();
    w.ensure_detected();
    w.usable().then(|| w.state.clone())
}

/// The network that last failed to join, and why, once.
pub fn take_failure() -> Option<(String, String)> {
    WIFI.lock().failure.take()
}

/// Networks in range, strongest first. Scans every few seconds.
pub fn networks() -> Vec<Network> {
    let mut w = WIFI.lock();
    w.ensure_detected();
    w.scan();
    w.networks.clone()
}

/// Turn the virtual adapter on or off.
pub fn set_virtual(on: bool) {
    config::update(|c| c.wifi_virtual = on);
    let mut w = WIFI.lock();
    w.disconnect();
    w.detected = false;
    w.auto_tried = false;
    w.ensure_detected();
    super::set_blocked(w.usable());
    if !w.usable() {
        w.networks.clear();
    }
}

/// Turn the radio on or off.
pub fn set_on(on: bool) {
    config::update(|c| c.wifi_on = on);
    let mut w = WIFI.lock();
    if !w.usable() {
        return;
    }
    if on {
        if w.state == State::Off {
            w.state = State::Disconnected;
            w.auto_tried = false;
            w.scanned_at = None;
        }
    } else {
        w.disconnect();
        w.state = State::Off;
        w.networks.clear();
    }
}

/// Start joining `ssid`. A password is needed for a secured network
/// that isn't remembered.
pub fn connect(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let mut w = WIFI.lock();
    w.ensure_detected();
    if !w.usable() {
        return Err(String::from("No Wi-Fi adapter"));
    }
    if w.state == State::Off {
        return Err(String::from("Wi-Fi is off"));
    }
    w.scanned_at = None;
    w.scan();
    let net = w
        .networks
        .iter()
        .find(|n| n.ssid == ssid)
        .cloned()
        .ok_or_else(|| String::from("That network is not in range"))?;
    let key = match (net.security, password) {
        (Security::Open, _) => None,
        (Security::Wpa2, Some(p)) => {
            let n = p.chars().count();
            if !(8..=63).contains(&n) {
                return Err(String::from("A WPA2 password has 8 to 63 characters"));
            }
            Some(wpa::psk(p, ssid))
        }
        (Security::Wpa2, None) => {
            let saved = config::get().wifi_saved;
            let Some((_, k)) = saved.iter().find(|(s, _)| s == ssid) else {
                return Err(String::from("Enter the network security key"));
            };
            Some(*k)
        }
    };
    w.start(&net, key, true);
    Ok(())
}

pub fn disconnect() {
    WIFI.lock().disconnect();
    config::update(|c| c.wifi_last.clear());
}

/// Forget a remembered network, leaving it if joined.
pub fn forget(ssid: &str) {
    config::update(|c| {
        c.wifi_saved.retain(|(s, _)| s != ssid);
        if c.wifi_last == ssid {
            c.wifi_last.clear();
        }
    });
    let mut w = WIFI.lock();
    let joined =
        matches!(&w.state, State::Connected { ssid: s, .. } | State::Connecting(s) if s == ssid);
    if joined {
        w.disconnect();
    }
    w.scanned_at = None;
}

/// Move a join along, and join the last network after startup. Called
/// from the desktop's main loop.
pub fn poll() {
    let mut w = WIFI.lock();
    if !w.detected || !w.usable() {
        return;
    }
    w.step();
    if !w.auto_tried && matches!(w.state, State::Disconnected | State::Failed(..)) {
        w.auto_tried = true;
        let cfg = config::get();
        if !cfg.wifi_last.is_empty() {
            w.scan();
            let net = w.networks.iter().find(|n| n.ssid == cfg.wifi_last).cloned();
            let key = cfg.wifi_saved.iter().find(|(s, _)| *s == cfg.wifi_last);
            if let (Some(net), Some((_, key))) = (net, key) {
                let key = (net.security == Security::Wpa2).then_some(*key);
                w.start(&net, key, false);
            }
        }
    }
    // the signal of the joined network drifts a little
    let now = interrupts::ticks();
    if let State::Connected { ssid, .. } = &w.state {
        let ssid = ssid.clone();
        if let Some(bssid) = w.radio.bssid(&ssid) {
            let signal = w.radio.signal(bssid, now);
            w.state = State::Connected { ssid, signal };
        }
    }
}

// ---- the virtual adapter ---------------------------------------------------------

/// Access points the virtual adapter hears: name, password (None for an
/// open network), signal, and its MAC address.
const ACCESS_POINTS: [(&str, Option<&str>, u8, [u8; 6]); 5] = [
    (
        "EverOS-Home",
        Some("everos2026"),
        92,
        [0x02, 0xe5, 0x00, 0x00, 0x00, 0x01],
    ),
    (
        "Cafe Free Wi-Fi",
        None,
        64,
        [0x02, 0xe5, 0x00, 0x00, 0x00, 0x02],
    ),
    (
        "TP-Link_5G_4F2A",
        Some("neighbour-secret"),
        47,
        [0x02, 0xe5, 0x00, 0x00, 0x00, 0x03],
    ),
    (
        "Office-Guest",
        Some("guest12345"),
        38,
        [0x02, 0xe5, 0x00, 0x00, 0x00, 0x04],
    ),
    (
        "DIRECT-7B-Printer",
        Some("printer-pin-8421"),
        21,
        [0x02, 0xe5, 0x00, 0x00, 0x00, 0x05],
    ),
];

/// Pretend radio: access points with a signal that drifts, and one
/// association at a time running the access point's side of WPA2.
struct VirtualRadio {
    ap: Option<Authenticator>,
    joined: Option<[u8; 6]>,
    /// Frames from the access point to us.
    inbox: Vec<Vec<u8>>,
}

impl VirtualRadio {
    const fn new() -> Self {
        VirtualRadio {
            ap: None,
            joined: None,
            inbox: Vec::new(),
        }
    }

    fn signal(&self, bssid: [u8; 6], now: u64) -> u8 {
        let Some(&(_, _, base, _)) = ACCESS_POINTS.iter().find(|a| a.3 == bssid) else {
            return 0;
        };
        // a slow wobble of a few percent
        let t = (now / (3 * HZ) + bssid[5] as u64 * 7) % 8;
        let wobble = [0i32, 2, 3, 1, -1, -3, -2, 0][t as usize];
        (base as i32 + wobble).clamp(1, 100) as u8
    }

    fn bssid(&self, ssid: &str) -> Option<[u8; 6]> {
        ACCESS_POINTS.iter().find(|a| a.0 == ssid).map(|a| a.3)
    }

    fn scan(&self, now: u64) -> Vec<Network> {
        ACCESS_POINTS
            .iter()
            .map(|&(ssid, pass, _, bssid)| Network {
                ssid: ssid.to_string(),
                bssid,
                signal: self.signal(bssid, now),
                security: if pass.is_some() {
                    Security::Wpa2
                } else {
                    Security::Open
                },
                saved: false,
            })
            .collect()
    }

    /// Join an access point. Some(true) if it is open, Some(false) if
    /// the handshake follows (its message 1 is then waiting).
    fn associate(
        &mut self,
        bssid: [u8; 6],
        mac: [u8; 6],
        _key: Option<[u8; 32]>,
        now: u64,
    ) -> Option<bool> {
        let &(ssid, pass, _, _) = ACCESS_POINTS.iter().find(|a| a.3 == bssid)?;
        self.leave();
        self.joined = Some(bssid);
        let Some(pass) = pass else {
            return Some(true);
        };
        let gtk = wpa::hmac_sha1(&now.to_le_bytes(), &[b"group key"]);
        let mut ap = Authenticator::new(
            wpa::psk(pass, ssid),
            bssid,
            mac,
            nonce(),
            gtk[..16].try_into().unwrap(),
        );
        self.inbox.push(ap.start());
        self.ap = Some(ap);
        Some(false)
    }

    fn take_from_ap(&mut self) -> Option<Vec<u8>> {
        (!self.inbox.is_empty()).then(|| self.inbox.remove(0))
    }

    /// A frame from us to the access point. A frame signed with another
    /// key gets no answer, as on a real network.
    fn send_to_ap(&mut self, frame: &[u8]) {
        let Some(ap) = self.ap.as_mut() else {
            return;
        };
        match ap.on_frame(frame) {
            Ok(Some(reply)) => self.inbox.push(reply),
            Ok(None) => {}
            Err(wpa::Error::BadMic) => log("access point: message 2 has a bad MIC"),
            Err(wpa::Error::Protocol(e)) => log(e),
        }
    }

    fn handshake_done(&self) -> bool {
        self.ap.as_ref().is_some_and(|a| a.done)
    }

    fn leave(&mut self) {
        self.ap = None;
        self.joined = None;
        self.inbox.clear();
    }
}
