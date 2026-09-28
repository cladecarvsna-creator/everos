//! A Telegram client: MTProto 2.0 over TCP (mtproto.rs), the TL format
//! read from the official schema (tl.rs), and the client logic (client.rs)
//! that signs in, keeps the chat list and the open chats, and sends and
//! receives messages. The window is gui/telegram.rs.
//!
//! The client runs in a fiber, so waiting for the network never stops
//! the desktop. It and the window share a [`Shared`]: the window puts
//! commands in, the client puts chats and messages in.
//!
//! Every user brings their own api_id and api_hash from
//! my.telegram.org; they are kept in `/Users/<name>/AppData/telegram.conf`
//! and the signed-in session in `telegram.session` next to it.

pub mod client;
pub mod crypto;
pub mod mtproto;
pub mod tl;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// A chat: a user, a basic group or a channel (supergroups are channels).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Peer {
    User(i64),
    Chat(i64),
    Channel(i64),
}

/// What the client is doing, and what the window should show.
#[derive(Clone, PartialEq, Debug)]
pub enum Stage {
    /// Reading settings or connecting.
    Starting,
    /// Needs the api_id and api_hash.
    Config,
    Phone,
    /// Waiting for the login code; says where it was sent.
    Code(String),
    /// Two-step verification; the password hint.
    Password(String),
    Ready,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    Config {
        api_id: String,
        api_hash: String,
    },
    Phone(String),
    Code(String),
    Password(String),
    /// Show a chat: load its messages and mark them read.
    Open(Peer),
    /// Load older messages of a chat.
    Older(Peer),
    Send(Peer, String),
    Reload,
    LogOut,
    /// Look for people, groups and channels by name or username.
    Search(String),
    /// Find a username (from @name or a t.me link) and show that chat.
    Resolve(String),
    Join(Peer),
    Leave(Peer),
    /// Save the file or photo of a message to the Downloads folder.
    Download(Peer, i64),
}

/// Where a file is on Telegram's servers.
#[derive(Clone, Debug, PartialEq)]
pub struct FileLoc {
    pub id: i64,
    pub access_hash: i64,
    pub file_reference: Vec<u8>,
    pub dc: i32,
    /// A photo, in this size ("x"); otherwise a document.
    pub photo_size: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Photo {
    pub loc: FileLoc,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub loc: FileLoc,
    pub name: String,
    pub size: i64,
}

/// A link in a message's text, from character `start` to `end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub start: usize,
    pub end: usize,
    /// An address, or "@name" for a username.
    pub target: String,
}

/// A photo, made small enough to show.
#[derive(Debug)]
pub struct Picture {
    pub w: i32,
    pub h: i32,
    /// 0xAARRGGBB.
    pub pixels: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Download {
    /// Parts of a thousand done.
    Going(u32),
    /// Saved at this path.
    Done(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Chat {
    pub peer: Peer,
    pub title: String,
    /// The last message, as one line.
    pub last: String,
    pub last_out: bool,
    pub date: i64,
    pub unread: i64,
    pub pinned: bool,
    /// Our messages up to this one were read.
    pub read_out: i64,
    pub kind: ChatKind,
    /// Without the @; empty when there is none.
    pub username: String,
    /// We are in it (a chat found by search may not be).
    pub member: bool,
    /// We may write in it (in channels only admins can).
    pub can_post: bool,
    /// Members or subscribers, when known.
    pub members: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Saved,
    Bot,
    Group,
    Channel,
}

#[derive(Clone, Debug)]
pub struct Message {
    /// 0 while it is being sent.
    pub id: i64,
    pub out: bool,
    /// Who wrote it, for groups.
    pub from: String,
    pub from_id: i64,
    pub text: String,
    /// "Photo", "Sticker", ... for messages with something attached.
    pub media: Option<String>,
    /// A service message ("joined the group"), drawn in the middle.
    pub service: bool,
    pub date: i64,
    pub edited: bool,
    /// Our random id while it is being sent.
    pub random_id: i64,
    pub failed: bool,
    pub photo: Option<Photo>,
    /// Something to save: a file, a video, music...
    pub file: Option<Document>,
    pub links: Vec<Link>,
}

#[derive(Default, Debug)]
pub struct History {
    /// Oldest first.
    pub messages: Vec<Message>,
    /// Everything back to the first message is here.
    pub complete: bool,
    pub loading: bool,
}

#[derive(Debug)]
pub struct Shared {
    pub stage: Stage,
    /// Shown under the form or at the top of the chats.
    pub error: Option<String>,
    /// Waiting for the server on something the user asked for.
    pub busy: bool,
    /// Whether the client is connected.
    pub online: bool,
    pub chats: Vec<Chat>,
    pub history: BTreeMap<Peer, History>,
    /// Our own name.
    pub me: String,
    pub commands: VecDeque<Cmd>,
    /// Bumped on every change, so the window knows to draw again.
    pub version: u64,
    /// Seconds to add to a Telegram time to get the local time.
    pub tz: i64,
    /// The chat the window shows: new messages there are read at once.
    pub open: Option<Peer>,
    /// Chats found by searching (people and channels not in the list).
    pub found: Vec<Chat>,
    /// What `found` was searched for.
    pub found_for: String,
    /// Photos by id; None when one could not be loaded.
    pub photos: BTreeMap<i64, Option<Picture>>,
    /// Files being saved or saved, by id.
    pub downloads: BTreeMap<i64, Download>,
    /// A chat the window should show (after following a username).
    pub goto: Option<Peer>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            stage: Stage::Starting,
            error: None,
            busy: false,
            online: false,
            chats: Vec::new(),
            history: BTreeMap::new(),
            me: String::new(),
            commands: VecDeque::new(),
            version: 1,
            tz: 0,
            open: None,
            found: Vec::new(),
            found_for: String::new(),
            photos: BTreeMap::new(),
            downloads: BTreeMap::new(),
            goto: None,
        }
    }

    pub fn changed(&mut self) {
        self.version += 1;
    }

    /// A chat in the list, or one found by search.
    pub fn chat(&self, peer: Peer) -> Option<&Chat> {
        self.chats
            .iter()
            .chain(self.found.iter())
            .find(|c| c.peer == peer)
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// Check the cryptography against known answers, for the boot test and
/// the shell's `telegram selftest`. Returns what failed.
pub fn self_test() -> Result<(), &'static str> {
    // AES-256-IGE, the answer Telethon gives for the same input
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let iv: [u8; 32] = core::array::from_fn(|i| 32 + i as u8);
    let plain = [0x41u8; 32];
    let enc = crypto::ige_encrypt(&plain, &key, &iv);
    let expected = crypto::from_hex(concat!(
        "bf7297874c7c82d813bb4ea09d70cde4",
        "deb247ac0342ef61b416dbfbb3efd30f"
    ));
    if enc != expected || crypto::ige_decrypt(&enc, &key, &iv) != plain {
        return Err("aes-ige");
    }
    if crypto::factor(0x17ED48941A08F981) != Some((0x494C553B, 0x53911073)) {
        return Err("factoring pq");
    }
    // the fingerprints Telegram gives for its keys
    for i in 0..crypto::SERVER_KEY_COUNT {
        let (f, n) = crypto::server_key_at(i);
        if crypto::fingerprint(&n) != f {
            return Err("rsa key fingerprints");
        }
    }
    let s = tl::schema();
    let m = s.by_name("message").ok_or("schema: message")?;
    if m.id != 0x7600_b9d3 {
        return Err("schema: message id");
    }
    let o = tl::Obj::new(
        "inputPeerUser",
        &[("user_id", 777i64.into()), ("access_hash", (-5i64).into())],
    );
    let bytes = tl::encode(&o);
    if tl::Reader::new(&bytes).obj().ok() != Some(o) {
        return Err("tl round trip");
    }
    Ok(())
}
