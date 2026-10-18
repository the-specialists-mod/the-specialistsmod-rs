//! Port addition: the `TSPkg` user message (server offers of client plugins, and the version
//! notices of §13) and the client's `tspkg` replies (`docs/implementation/client-plugins.md` §6.2,
//! §6.5, §13.3). TS 3.0 has neither.
//!
//! The server encodes with [`Message::encode`] and the client reassembles and decodes with a
//! [`Reassembler`]; the client formats its replies with [`Reply::command`] and
//! [`list_commands`], and the server reads them with [`Reply::parse`]. Tests pin each encoder
//! to its decoder.
//!
//! # Framing
//!
//! GoldSrc refuses a user message over 192 bytes (ReHLDS `MAX_USER_MSG_DATA`, a `Host_Error`),
//! and an offer can be longer, so every body is sent in chunks, each one `TSPkg` message:
//!
//! ```text
//! byte  op        1 offer, 2 withdraw, 3 require, 4 block, 5 port_version
//! byte  msg_id    the same on every chunk of one body; wraps at 256
//! byte  chunk     index from 0; the high bit (0x80) is set on the last chunk
//! ...   body      at most CHUNK_BODY (189) bytes of the body
//! ```
//!
//! The server sends a body's chunks back to back in the reliable stream, so a client keeps one
//! body in progress: a chunk for another `msg_id` or op, or out of order, drops it. A body has
//! at most [`MAX_CHUNKS`] chunks.
//!
//! # Bodies
//!
//! Strings are NUL-terminated, at most the byte counts below, with no control or
//! bidirectional-override characters (the cards show them).
//!
//! ```text
//! op 1 offer          string id (≤ 32), string version (≤ 16), long size (unsigned),
//!                     byte×32 sha256 of the .tspkg, byte flags (1 required, 2 fastdl),
//!                     string url (≤ 255, the full URL to fetch), string note (≤ 64)
//! op 2 withdraw       string id, string version
//! op 3 require        string id, string requirement (≤ 96), byte flags (1 required)
//! op 4 block          string id, string requirement, byte action (1 suspend, 2 kick),
//!                     string reason (≤ 64)
//! op 5 port_version   string id ("tsrs"), string requirement, string url (≤ 255)
//! ```
//!
//! `fastdl` marks a URL the server built from its `sv_downloadurl` (client.dll can't read the
//! engine's download URL), which may be plain HTTP; any other URL is `https://`.

use std::fmt;

/// The message's name: [`super::Msg::TsPkg`], registered after TS's 64 messages and `TSExt`
/// (variable length).
pub const NAME: &std::ffi::CStr = c"TSPkg";
/// The most a GoldSrc user message may carry.
pub const MAX_MESSAGE: usize = 192;
/// The chunk header: op, msg_id, chunk.
pub const HEADER: usize = 3;
/// The most body bytes one chunk carries.
pub const CHUNK_BODY: usize = MAX_MESSAGE - HEADER;
/// The most chunks one body may have.
pub const MAX_CHUNKS: usize = 4;
/// The chunk byte's last-chunk flag.
pub const LAST_CHUNK: u8 = 0x80;

/// The longest plugin id (`[a-z0-9_-]{1,32}`, or `manual:<name>` / `manual:*` in a block).
pub const MAX_ID: usize = 40;
/// The longest plugin version (a manifest's limit).
pub const MAX_VERSION: usize = 16;
/// The longest version requirement.
pub const MAX_REQUIREMENT: usize = 96;
/// The longest URL.
pub const MAX_URL: usize = 255;
/// The longest note (offer) or reason (block, reply).
pub const MAX_NOTE: usize = 64;
/// The id op 5 carries.
pub const PORT_ID: &str = "tsrs";

/// Op 1's and op 3's `required` flag.
pub const FLAG_REQUIRED: u8 = 1;
/// Op 1's `fastdl` flag: the URL is the server's `sv_downloadurl` plus the offer's path.
pub const FLAG_FASTDL: u8 = 2;

/// An offer (op 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub version: String,
    /// The package's size in bytes.
    pub size: u32,
    /// SHA-256 of the `.tspkg`.
    pub sha256: [u8; 32],
    /// `FLAG_REQUIRED`, `FLAG_FASTDL`.
    pub flags: u8,
    /// The full URL of the `.tspkg`; the manifest is beside it (`.manifest.toml`, `.manifest.sig`).
    pub url: String,
    pub note: String,
}

impl Offer {
    pub fn required(&self) -> bool {
        self.flags & FLAG_REQUIRED != 0
    }
    pub fn fastdl(&self) -> bool {
        self.flags & FLAG_FASTDL != 0
    }
}

/// A block's action (op 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockAction {
    Suspend = 1,
    Kick = 2,
}

/// One `TSPkg` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Offer(Offer),
    Withdraw {
        id: String,
        version: String,
    },
    Require {
        id: String,
        requirement: String,
        flags: u8,
    },
    Block {
        id: String,
        requirement: String,
        action: BlockAction,
        reason: String,
    },
    /// The port version the server needs (§13.1); the client shows the update card.
    PortVersion {
        requirement: String,
        url: String,
    },
}

/// Why a body can't be encoded or decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A field is too long, has a NUL or a control character, or isn't UTF-8.
    Field(&'static str, String),
    /// The body needs more than [`MAX_CHUNKS`] chunks.
    TooLong(usize),
    /// An unknown op, block action, or chunk header.
    Bad(String),
    /// The body ended early, or has bytes after its last field.
    Truncated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Field(field, why) => write!(f, "{field}: {why}"),
            Error::TooLong(n) => write!(f, "a body of {n} bytes needs more than {MAX_CHUNKS} chunks"),
            Error::Bad(why) => f.write_str(why),
            Error::Truncated => f.write_str("the body is truncated or has trailing bytes"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether `s` is fit for a `TSPkg` string or a `tspkg` reply: at most `max` bytes, without NUL,
/// control or bidirectional-override characters.
pub fn check_text(field: &'static str, s: &str, max: usize) -> Result<(), Error> {
    if s.len() > max {
        return Err(Error::Field(field, format!("longer than {max} bytes")));
    }
    if s.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
        return Err(Error::Field(field, "has control characters".into()));
    }
    Ok(())
}

/// Ids and versions also go into `tspkg` replies as single tokens, so they're printable ASCII
/// without spaces or quotes.
fn check_token(field: &'static str, s: &str, max: usize) -> Result<(), Error> {
    check_text(field, s, max)?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b';') {
        return Err(Error::Field(field, "isn't a printable token".into()));
    }
    Ok(())
}

impl Message {
    pub fn op(&self) -> u8 {
        match self {
            Message::Offer(_) => 1,
            Message::Withdraw { .. } => 2,
            Message::Require { .. } => 3,
            Message::Block { .. } => 4,
            Message::PortVersion { .. } => 5,
        }
    }

    /// The body, checked against every limit.
    pub fn body(&self) -> Result<Vec<u8>, Error> {
        let mut b = Vec::new();
        let s = |b: &mut Vec<u8>, field, v: &str, max, token| -> Result<(), Error> {
            if token {
                check_token(field, v, max)?
            } else {
                check_text(field, v, max)?
            }
            b.extend_from_slice(v.as_bytes());
            b.push(0);
            Ok(())
        };
        match self {
            Message::Offer(o) => {
                s(&mut b, "id", &o.id, MAX_ID, true)?;
                s(&mut b, "version", &o.version, MAX_VERSION, true)?;
                b.extend_from_slice(&o.size.to_le_bytes());
                b.extend_from_slice(&o.sha256);
                b.push(o.flags);
                s(&mut b, "url", &o.url, MAX_URL, true)?;
                s(&mut b, "note", &o.note, MAX_NOTE, false)?;
            }
            Message::Withdraw { id, version } => {
                s(&mut b, "id", id, MAX_ID, true)?;
                s(&mut b, "version", version, MAX_VERSION, true)?;
            }
            Message::Require { id, requirement, flags } => {
                s(&mut b, "id", id, MAX_ID, true)?;
                s(&mut b, "requirement", requirement, MAX_REQUIREMENT, false)?;
                b.push(*flags);
            }
            Message::Block { id, requirement, action, reason } => {
                s(&mut b, "id", id, MAX_ID, true)?;
                s(&mut b, "requirement", requirement, MAX_REQUIREMENT, false)?;
                b.push(*action as u8);
                s(&mut b, "reason", reason, MAX_NOTE, false)?;
            }
            Message::PortVersion { requirement, url } => {
                s(&mut b, "id", PORT_ID, MAX_ID, true)?;
                s(&mut b, "requirement", requirement, MAX_REQUIREMENT, false)?;
                s(&mut b, "url", url, MAX_URL, false)?;
            }
        }
        Ok(b)
    }

    /// The `TSPkg` messages that carry this body, each at most [`MAX_MESSAGE`] bytes.
    pub fn encode(&self, msg_id: u8) -> Result<Vec<Vec<u8>>, Error> {
        let body = self.body()?;
        let n = body.len().div_ceil(CHUNK_BODY).max(1);
        if n > MAX_CHUNKS {
            return Err(Error::TooLong(body.len()));
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let part = &body[(i * CHUNK_BODY).min(body.len())..((i + 1) * CHUNK_BODY).min(body.len())];
            let mut m = Vec::with_capacity(HEADER + part.len());
            m.extend_from_slice(&[self.op(), msg_id, i as u8 | if i + 1 == n { LAST_CHUNK } else { 0 }]);
            m.extend_from_slice(part);
            out.push(m);
        }
        Ok(out)
    }

    /// Decodes a reassembled body.
    pub fn decode(op: u8, body: &[u8]) -> Result<Message, Error> {
        let mut r = Reader { b: body, pos: 0 };
        let m = match op {
            1 => {
                let id = r.token("id", MAX_ID)?;
                let version = r.token("version", MAX_VERSION)?;
                let size = u32::from_le_bytes(r.take::<4>()?);
                let sha256 = r.take::<32>()?;
                let flags = r.take::<1>()?[0];
                let url = r.token("url", MAX_URL)?;
                let note = r.text("note", MAX_NOTE)?;
                Message::Offer(Offer { id, version, size, sha256, flags, url, note })
            }
            2 => Message::Withdraw { id: r.token("id", MAX_ID)?, version: r.token("version", MAX_VERSION)? },
            3 => Message::Require {
                id: r.token("id", MAX_ID)?,
                requirement: r.text("requirement", MAX_REQUIREMENT)?,
                flags: r.take::<1>()?[0],
            },
            4 => {
                let id = r.token("id", MAX_ID)?;
                let requirement = r.text("requirement", MAX_REQUIREMENT)?;
                let action = match r.take::<1>()?[0] {
                    1 => BlockAction::Suspend,
                    2 => BlockAction::Kick,
                    a => return Err(Error::Bad(format!("block action {a}"))),
                };
                Message::Block { id, requirement, action, reason: r.text("reason", MAX_NOTE)? }
            }
            5 => {
                let id = r.token("id", MAX_ID)?;
                if id != PORT_ID {
                    return Err(Error::Bad(format!("port_version for id {id:?}")));
                }
                Message::PortVersion { requirement: r.text("requirement", MAX_REQUIREMENT)?, url: r.text("url", MAX_URL)? }
            }
            _ => return Err(Error::Bad(format!("op {op}"))),
        };
        if r.pos != body.len() {
            return Err(Error::Truncated);
        }
        Ok(m)
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let out = self.b.get(self.pos..self.pos + N).ok_or(Error::Truncated)?;
        self.pos += N;
        Ok(out.try_into().unwrap_or([0; N]))
    }

    fn text(&mut self, field: &'static str, max: usize) -> Result<String, Error> {
        let rest = self.b.get(self.pos..).ok_or(Error::Truncated)?;
        let end = rest.iter().position(|&c| c == 0).ok_or(Error::Truncated)?;
        let s = std::str::from_utf8(&rest[..end]).map_err(|_| Error::Field(field, "isn't UTF-8".into()))?;
        check_text(field, s, max)?;
        self.pos += end + 1;
        Ok(s.to_owned())
    }

    fn token(&mut self, field: &'static str, max: usize) -> Result<String, Error> {
        let s = self.text(field, max)?;
        check_token(field, &s, max)?;
        Ok(s)
    }
}

/// The client's reassembly of chunked bodies.
#[derive(Default, Debug)]
pub struct Reassembler {
    /// (op, msg_id, next chunk index, body so far).
    pending: Option<(u8, u8, u8, Vec<u8>)>,
}

impl Reassembler {
    /// Takes one `TSPkg` message. `Ok(Some)` when it completes a body. A chunk that doesn't
    /// continue the body in progress drops that body; a first chunk then starts a new one.
    pub fn push(&mut self, msg: &[u8]) -> Result<Option<Message>, Error> {
        let [op, id, chunk, ..] = *msg else { return Err(Error::Bad("a chunk without its header".into())) };
        let (index, last) = (chunk & !LAST_CHUNK, chunk & LAST_CHUNK != 0);
        let part = &msg[HEADER..];
        if part.len() > CHUNK_BODY || usize::from(index) >= MAX_CHUNKS {
            self.pending = None;
            return Err(Error::Bad(format!("chunk {index} of {} bytes", part.len())));
        }
        let mut body = match self.pending.take() {
            Some((p_op, p_id, next, body)) if (p_op, p_id, next) == (op, id, index) => body,
            _ if index == 0 => Vec::new(),
            _ => return Err(Error::Bad(format!("chunk {index} of msg_id {id} without its start"))),
        };
        body.extend_from_slice(part);
        if !last {
            self.pending = Some((op, id, index + 1, body));
            return Ok(None);
        }
        Message::decode(op, &body).map(Some)
    }
}

/// What a client reports for an offer (§6.5, §13.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Have,
    Offered,
    Downloading,
    Installed,
    Loaded,
    Denied,
    Blocked,
    Unsupported,
    Failed,
    Suspended,
    Restart,
}

impl State {
    pub const ALL: [State; 11] = [
        State::Have,
        State::Offered,
        State::Downloading,
        State::Installed,
        State::Loaded,
        State::Denied,
        State::Blocked,
        State::Unsupported,
        State::Failed,
        State::Suspended,
        State::Restart,
    ];

    pub fn name(self) -> &'static str {
        match self {
            State::Have => "have",
            State::Offered => "offered",
            State::Downloading => "downloading",
            State::Installed => "installed",
            State::Loaded => "loaded",
            State::Denied => "denied",
            State::Blocked => "blocked",
            State::Unsupported => "unsupported",
            State::Failed => "failed",
            State::Suspended => "suspended",
            State::Restart => "restart",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.name() == s)
    }

    /// The answers that refuse a required offer (`sv_plugin_required_kick`, §6.1).
    pub fn refuses(self) -> bool {
        matches!(self, State::Denied | State::Unsupported | State::Failed)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The client command's name.
pub const REPLY_COMMAND: &str = "tspkg";
/// The longest client command the client sends (the spec keeps commands under 128 bytes).
pub const MAX_COMMAND: usize = 127;

/// A `tspkg` client command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `tspkg <id> <version> <state> [reason]`.
    State { id: String, version: String, state: State, reason: String },
    /// `tspkg list <id>@<version> ...`: loaded plugins, added to what earlier lines named. A
    /// `tspkg list` without any ends the list (so a client without plugins sends just that).
    List(Vec<(String, String)>),
}

impl Reply {
    /// Parses the command's arguments after `tspkg`.
    pub fn parse(args: &[&str]) -> Result<Reply, Error> {
        match args {
            ["list", items @ ..] => items
                .iter()
                .map(|item| {
                    let (id, version) = item.rsplit_once('@').ok_or_else(|| Error::Bad(format!("list item {item:?}")))?;
                    check_token("id", id, MAX_ID)?;
                    check_token("version", version, MAX_VERSION)?;
                    Ok((id.to_owned(), version.to_owned()))
                })
                .collect::<Result<_, _>>()
                .map(Reply::List),
            [id, version, state, reason @ ..] => {
                check_token("id", id, MAX_ID)?;
                check_token("version", version, MAX_VERSION)?;
                let state = State::parse(state).ok_or_else(|| Error::Bad(format!("state {state:?}")))?;
                let reason = reason.join(" ");
                check_text("reason", &reason, MAX_NOTE)?;
                Ok(Reply::State { id: id.to_string(), version: version.to_string(), state, reason })
            }
            _ => Err(Error::Bad("usage: tspkg <id> <version> <state> [reason] | tspkg list <id>@<version> ...".into())),
        }
    }

    /// The command line for a [`Reply::State`] (a [`Reply::List`] is sent with
    /// [`list_commands`]). The reason is quoted, with quotes and `;` removed and cut to
    /// [`MAX_NOTE`] bytes at a character boundary.
    pub fn command(&self) -> Result<String, Error> {
        match self {
            Reply::State { id, version, state, reason } => {
                check_token("id", id, MAX_ID)?;
                check_token("version", version, MAX_VERSION)?;
                let mut r: String = reason.chars().filter(|c| !c.is_control() && *c != '"' && *c != ';').collect();
                let mut n = r.len().min(MAX_NOTE);
                while !r.is_char_boundary(n) {
                    n -= 1;
                }
                r.truncate(n);
                let r = r.trim();
                Ok(if r.is_empty() {
                    format!("{REPLY_COMMAND} {id} {version} {state}")
                } else {
                    format!("{REPLY_COMMAND} {id} {version} {state} \"{r}\"")
                })
            }
            Reply::List(_) => Err(Error::Bad("send a list with list_commands".into())),
        }
    }
}

/// The `tspkg list` commands for every loaded plugin: each under [`MAX_COMMAND`] bytes, the last
/// a bare `tspkg list` that ends the list.
pub fn list_commands(plugins: &[(String, String)]) -> Result<Vec<String>, Error> {
    let head = format!("{REPLY_COMMAND} list");
    let mut out = Vec::new();
    let mut line = head.clone();
    for (id, version) in plugins {
        check_token("id", id, MAX_ID)?;
        check_token("version", version, MAX_VERSION)?;
        let item = format!(" {id}@{version}");
        if line.len() + item.len() > MAX_COMMAND {
            out.push(std::mem::replace(&mut line, head.clone()));
        }
        line.push_str(&item);
    }
    if line != head {
        out.push(line);
    }
    out.push(head);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(url: &str, note: &str) -> Message {
        Message::Offer(Offer {
            id: "webview".into(),
            version: "0.1.0".into(),
            size: 131_072_000,
            sha256: std::array::from_fn(|i| i as u8),
            flags: FLAG_REQUIRED | FLAG_FASTDL,
            url: url.into(),
            note: note.into(),
        })
    }

    fn round_trip(m: &Message, msg_id: u8) -> Vec<Vec<u8>> {
        let chunks = m.encode(msg_id).unwrap();
        let mut r = Reassembler::default();
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.len() <= MAX_MESSAGE, "chunk {i}: {} bytes", c.len());
            let got = r.push(c).unwrap();
            if i + 1 < chunks.len() {
                assert_eq!(got, None);
            } else {
                assert_eq!(got.as_ref(), Some(m));
            }
        }
        chunks
    }

    #[test]
    fn every_op_round_trips() {
        let msgs = [
            offer("http://fastdl.example.org/ts/plugins/webview-0.1.0.tspkg", "Needed for the stats and vote pages."),
            offer("https://x.org/a.tspkg", ""),
            Message::Withdraw { id: "webview".into(), version: "0.1.0".into() },
            Message::Require { id: "webview".into(), requirement: ">=0.2.0, <0.4".into(), flags: FLAG_REQUIRED },
            Message::Block {
                id: "manual:*".into(),
                requirement: "*".into(),
                action: BlockAction::Kick,
                reason: "no hand-made plugins".into(),
            },
            Message::Block { id: "webview".into(), requirement: "<0.2.0".into(), action: BlockAction::Suspend, reason: "".into() },
            Message::PortVersion { requirement: "=3.0.9".into(), url: "tsmod.org".into() },
        ];
        for (i, m) in msgs.iter().enumerate() {
            let chunks = round_trip(m, (i as u8).wrapping_add(250));
            assert_eq!(chunks[0][0], m.op());
            assert_eq!(chunks.last().unwrap()[2] & LAST_CHUNK, LAST_CHUNK);
        }
        let short = Message::Withdraw { id: "a".into(), version: "1.0.0".into() }.encode(7).unwrap();
        assert_eq!(short, vec![b"\x02\x07\x80a\x001.0.0\x00".to_vec()]);
    }

    #[test]
    fn the_largest_offer_fits_in_the_chunks() {
        let big = Message::Offer(Offer {
            id: "x".repeat(MAX_ID),
            version: "1".repeat(MAX_VERSION),
            size: u32::MAX,
            sha256: [0xff; 32],
            flags: 0xff,
            url: "u".repeat(MAX_URL),
            note: "é".repeat(MAX_NOTE / 2),
        });
        let chunks = round_trip(&big, 0);
        assert!(chunks.len() <= MAX_CHUNKS);
        assert!(chunks[..chunks.len() - 1].iter().all(|c| c.len() == MAX_MESSAGE), "full chunks but the last");
        // Every op's largest body fits too.
        let req = "r".repeat(MAX_REQUIREMENT);
        for m in [
            Message::Block { id: "x".repeat(MAX_ID), requirement: req.clone(), action: BlockAction::Kick, reason: "n".repeat(MAX_NOTE) },
            Message::PortVersion { requirement: req.clone(), url: "u".repeat(MAX_URL) },
            Message::Require { id: "x".repeat(MAX_ID), requirement: req, flags: 1 },
        ] {
            round_trip(&m, 1);
        }
    }

    #[test]
    fn limits_and_bad_text_are_refused() {
        let o = |f: &dyn Fn(&mut Offer)| {
            let Message::Offer(mut x) = offer("https://x.org/a.tspkg", "") else { unreachable!() };
            f(&mut x);
            Message::Offer(x).encode(0)
        };
        assert!(o(&|x| x.id = "x".repeat(MAX_ID + 1)).is_err());
        assert!(o(&|x| x.version = "1".repeat(MAX_VERSION + 1)).is_err());
        assert!(o(&|x| x.url = "u".repeat(MAX_URL + 1)).is_err());
        assert!(o(&|x| x.note = "n".repeat(MAX_NOTE + 1)).is_err());
        assert!(o(&|x| x.note = "a\nb".into()).is_err(), "control characters");
        assert!(o(&|x| x.note = "a\u{202e}b".into()).is_err(), "bidi override");
        assert!(o(&|x| x.note = "a\0b".into()).is_err(), "NUL");
        assert!(o(&|x| x.id = "a b".into()).is_err(), "ids are tokens");
        assert!(o(&|x| x.url = "https://x.org/a\".tspkg".into()).is_err(), "quotes in the url");
        assert!(o(&|x| x.id = String::new()).is_err());
        assert!(o(&|x| x.note = "n".repeat(MAX_NOTE)).is_ok());
    }

    #[test]
    fn bad_input_is_refused() {
        let good = offer("https://x.org/a.tspkg", "hi").body().unwrap();
        assert!(Message::decode(1, &good).is_ok());
        assert_eq!(Message::decode(1, &good[..good.len() - 1]), Err(Error::Truncated));
        let mut extra = good.clone();
        extra.push(0);
        assert_eq!(Message::decode(1, &extra), Err(Error::Truncated));
        assert!(matches!(Message::decode(9, &good), Err(Error::Bad(_))));
        assert!(matches!(Message::decode(5, b"webview\0=1.0.0\0x\0"), Err(Error::Bad(_))), "op 5 is for tsrs");
        assert!(matches!(Message::decode(4, b"a\0*\0\x03r\0"), Err(Error::Bad(_))), "action 3");
        assert!(matches!(Message::decode(2, b"\xff\x001.0\0"), Err(Error::Field("id", _))));

        let mut r = Reassembler::default();
        assert!(r.push(&[1, 0]).is_err(), "no header");
        assert!(r.push(&[2, 0, 1 | LAST_CHUNK, b'a', 0]).is_err(), "a middle chunk without its start");
        let two = offer(&format!("https://x.org/{}.tspkg", "a".repeat(150)), "").encode(9).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(r.push(&two[0]), Ok(None));
        // A new body drops the unfinished one.
        let w = Message::Withdraw { id: "a".into(), version: "1".into() };
        assert_eq!(r.push(&w.encode(10).unwrap()[0]), Ok(Some(w)));
        assert!(r.push(&two[1]).is_err(), "its start is gone");
        // Too many chunks.
        let mut r = Reassembler::default();
        for i in 0..MAX_CHUNKS as u8 {
            let _ = r.push(&[1, 3, i]);
        }
        assert!(r.push(&[1, 3, MAX_CHUNKS as u8 | LAST_CHUNK]).is_err());
        assert!(r.push(&[1, 3, 0].iter().copied().chain([0u8; CHUNK_BODY + 1]).collect::<Vec<_>>()).is_err(), "an oversized chunk");
    }

    #[test]
    fn replies() {
        let p = |s: &str| Reply::parse(&s.split_whitespace().collect::<Vec<_>>());
        assert_eq!(
            p("webview 0.1.0 offered"),
            Ok(Reply::State { id: "webview".into(), version: "0.1.0".into(), state: State::Offered, reason: String::new() })
        );
        assert_eq!(
            p("webview 0.1.0 failed bad hash"),
            Ok(Reply::State { id: "webview".into(), version: "0.1.0".into(), state: State::Failed, reason: "bad hash".into() })
        );
        assert!(p("webview 0.1.0 approved").is_err(), "unknown state");
        assert!(p("webview 0.1.0").is_err());
        assert!(p("").is_err());
        assert_eq!(p("list"), Ok(Reply::List(vec![])));
        assert_eq!(
            p("list webview@0.1.0 manual:foo@0.0.0"),
            Ok(Reply::List(vec![("webview".into(), "0.1.0".into()), ("manual:foo".into(), "0.0.0".into())]))
        );
        assert!(p("list webview").is_err());
        for st in State::ALL {
            assert_eq!(State::parse(st.name()), Some(st));
            let r = Reply::State { id: "a".into(), version: "1.0.0".into(), state: st, reason: "x \"y\"; z".into() };
            let cmd = r.command().unwrap();
            assert!(!cmd.contains(';'), "{cmd}");
            // The engine's tokenizer strips the quotes.
            let args = tokenize(&cmd);
            assert_eq!(args[0], REPLY_COMMAND);
            let back = Reply::parse(&args[1..].iter().map(String::as_str).collect::<Vec<_>>()).unwrap();
            assert_eq!(back, Reply::State { id: "a".into(), version: "1.0.0".into(), state: st, reason: "x y z".into() });
        }
        let long = Reply::State { id: "a".into(), version: "1".into(), state: State::Failed, reason: "é".repeat(100) };
        assert!(long.command().unwrap().len() <= MAX_COMMAND);
        assert!(State::Denied.refuses() && State::Unsupported.refuses() && State::Failed.refuses());
        assert!(!State::Have.refuses() && !State::Loaded.refuses() && !State::Offered.refuses());
    }

    #[test]
    fn lists_are_chunked() {
        assert_eq!(list_commands(&[]).unwrap(), vec!["tspkg list"]);
        let many: Vec<(String, String)> = (0..20).map(|i| (format!("plugin{i:02}"), format!("1.{i}.0"))).collect();
        let cmds = list_commands(&many).unwrap();
        assert!(cmds.len() > 2);
        assert_eq!(cmds.last().unwrap(), "tspkg list");
        let mut got = Vec::new();
        for c in &cmds {
            assert!(c.len() <= MAX_COMMAND, "{c}");
            let args: Vec<&str> = c.split(' ').collect();
            let Ok(Reply::List(items)) = Reply::parse(&args[1..]) else { panic!("{c}") };
            got.extend(items);
        }
        assert_eq!(got, many);
        assert!(list_commands(&[("a b".into(), "1".into())]).is_err());
    }

    /// The engines' `Cmd_TokenizeString` for these lines: spaces split, quotes group.
    pub(super) fn tokenize(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut it = s.chars().peekable();
        while let Some(&c) = it.peek() {
            if c == ' ' {
                it.next();
                continue;
            }
            let mut t = String::new();
            if c == '"' {
                it.next();
                for c in it.by_ref() {
                    if c == '"' {
                        break;
                    }
                    t.push(c);
                }
            } else {
                while let Some(&c) = it.peek() {
                    if c == ' ' {
                        break;
                    }
                    t.push(c);
                    it.next();
                }
            }
            out.push(t);
        }
        out
    }
}
