//! `TSExt`, the port's server-to-client-extension message (issue #294, port addition; the webview
//! specification §4.2, docs/implementation/client-ext-api.md §10).
//!
//! Body: `string channel` (1 to [`CHANNEL_MAX`] bytes), then the payload's bytes to the end of
//! the message. The whole message is at most [`MESSAGE_MAX`] bytes, the Steam engines' user
//! message limit (`MAX_USER_MSG_DATA`; FWGS allows 2048), so the payload is at most
//! [`max_payload`] of the channel. One message is one delivery: anything longer is the channel's
//! own protocol to split (the webview channel numbers its chunks, its spec §8.1).
//!
//! TS 3.0's client doesn't hook `TSExt`, so the server sends it only to port clients (those whose
//! userinfo has [`super::port::USERINFO_KEY`], client-plugins.md §12). The port client hands each
//! message to the extension that claimed its channel and drops the rest.

/// The longest channel name, in bytes (the NUL not counted).
pub const CHANNEL_MAX: usize = 15;
/// The longest user message the engines all carry: the Steam engines' `MAX_USER_MSG_DATA`.
pub const MESSAGE_MAX: usize = 192;

/// A channel name: 1 to [`CHANNEL_MAX`] bytes of ASCII letters, digits, `_`, `-` or `.`.
pub fn valid_channel(name: &[u8]) -> bool {
    (1..=CHANNEL_MAX).contains(&name.len()) && name.iter().all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The most payload bytes a message on a channel of `channel_len` bytes can carry.
pub const fn max_payload(channel_len: usize) -> usize {
    MESSAGE_MAX.saturating_sub(channel_len + 1)
}

/// A message's body, or `None` for a bad channel or a payload over [`max_payload`].
pub fn encode(channel: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
    if !valid_channel(channel) || payload.len() > max_payload(channel.len()) {
        return None;
    }
    let mut b = Vec::with_capacity(channel.len() + 1 + payload.len());
    b.extend_from_slice(channel);
    b.push(0);
    b.extend_from_slice(payload);
    Some(b)
}

/// The channel and payload of a message, or `None` if it has no terminated, valid channel.
pub fn decode(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let nul = buf.iter().position(|&b| b == 0)?;
    let (channel, rest) = (&buf[..nul], &buf[nul + 1..]);
    valid_channel(channel).then_some((channel, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        assert!(valid_channel(b"webview"));
        assert!(valid_channel(b"a.b-c_9"));
        assert!(valid_channel(&[b'x'; CHANNEL_MAX]));
        assert!(!valid_channel(&[b'x'; CHANNEL_MAX + 1]));
        for bad in [&b""[..], b"a b", b"a;b", b"a\"b", b"\xe9"] {
            assert!(!valid_channel(bad), "{bad:?}");
        }
    }

    #[test]
    fn round_trip_and_limits() {
        let b = encode(b"webview", b"\x01\x00\xffhi").unwrap();
        assert_eq!(b, b"webview\0\x01\x00\xffhi");
        assert_eq!(decode(&b), Some((&b"webview"[..], &b"\x01\x00\xffhi"[..])));
        // The webview spec's figure: a 15-byte channel leaves 176 bytes.
        assert_eq!(max_payload(CHANNEL_MAX), 176);
        let full = vec![7u8; max_payload(7)];
        assert_eq!(encode(b"webview", &full).unwrap().len(), MESSAGE_MAX);
        assert_eq!(encode(b"webview", &[0; 185]), None, "one byte over");
        assert_eq!(encode(b"bad channel", b""), None);
        assert_eq!(decode(b"webview"), None, "no terminator");
        assert_eq!(decode(b"\0x"), None, "an empty channel");
        assert_eq!(decode(b"test\0"), Some((&b"test"[..], &b""[..])));
    }
}
