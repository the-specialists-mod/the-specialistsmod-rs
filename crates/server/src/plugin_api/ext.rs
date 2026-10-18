//! Version 8 (port addition): `PluginApi::ext_message`, the `TSExt` message to the port client's
//! extensions (issue #294, `shared::protocol::ext`, docs/implementation/client-ext-api.md §10),
//! and `PluginApi::client_port_version` (issue #304, client-plugins.md §13.1).
//!
//! TS 3.0's client doesn't hook `TSExt` and a GoldSrc client can fail on a message it hasn't
//! hooked, so only clients that hook it get it: a `_tsrs` version at or above
//! `port::TSPKG_SINCE`, the first release whose client hooks the port's messages (`TSExt` and
//! `TSPkg` came together), the same gate as `TSPkg`'s (`plugin_offers::hooks_tspkg`, with its
//! `ts_dev_tspkg` dev aid; client-plugins.md §12). Port clients up to 3.0.8 carry `_tsrs` but hook
//! neither. The userinfo is read at each send, as `ClientConnect` reads it
//! (`gameplay::with_connecting_client`).

use super::Game;
use crate::gameplay::{ConnectingClient, with_connecting_client};
use crate::messages::{self, Msg};
use goldsrc::server::engine;
use goldsrc::sys::edict_t;
use shared::protocol::{ext, port};
use std::ffi::{CString, c_int};
use ts_plugin_api::{INVALID, NOT_PLAYER, UNSUPPORTED, ext_flag};

/// What a client's `ClientConnect` classification says about its port version: the version
/// (`major.minor.patch`), "" for a client without the port (TS 3.0's, a bot, HLTV), or
/// `UNSUPPORTED` for a `_tsrs` value that isn't a version.
pub fn version_of(c: ConnectingClient) -> Result<String, c_int> {
    match c {
        ConnectingClient::Port(v) => port::parse_userinfo_version(v).map(|[a, b, c]| format!("{a}.{b}.{c}")).ok_or(UNSUPPORTED),
        ConnectingClient::Bot | ConnectingClient::Hltv | ConnectingClient::Other => Ok(String::new()),
    }
}

/// True for a connected player whose client hooks `TSExt`: the port's, from `port::TSPKG_SINCE`.
fn is_port_client(e: *mut edict_t) -> bool {
    crate::plugin_offers::hooks_tspkg(e)
}

/// `PluginApi::client_port_version`.
pub fn client_port_version(player: c_int) -> Result<String, c_int> {
    let e = Game::edict(player);
    if e.is_null() {
        return Err(NOT_PLAYER);
    }
    with_connecting_client(e, version_of)
}

/// `PluginApi::ext_message`'s checks, in the API's order: the player (`NOT_PLAYER`), the
/// arguments (`INVALID`), then whether that client can take it (`UNSUPPORTED`). The body to send.
pub fn check(player_ok: bool, channel: &[u8], payload: &[u8], flags: c_int, port: bool) -> Result<Vec<u8>, c_int> {
    if !player_ok {
        return Err(NOT_PLAYER);
    }
    if flags & !ext_flag::UNRELIABLE != 0 {
        return Err(INVALID);
    }
    let body = ext::encode(channel, payload).ok_or(INVALID)?;
    if !port {
        return Err(UNSUPPORTED);
    }
    Ok(body)
}

/// `PluginApi::ext_message`: the number of clients the message went to, or a status.
pub fn ext_message(player: c_int, channel: &[u8], payload: &[u8], flags: c_int) -> c_int {
    let targets: Vec<*mut edict_t> = if player == 0 {
        if let Err(code) = check(true, channel, payload, flags, true) {
            return code;
        }
        (1..=engine().max_clients()).map(Game::edict).filter(|e| !e.is_null() && is_port_client(*e)).collect()
    } else {
        let e = Game::edict(player);
        if let Err(code) = check(!e.is_null(), channel, payload, flags, !e.is_null() && is_port_client(e)) {
            return code;
        }
        vec![e]
    };
    let name = CString::new(channel).unwrap_or_default();
    for &e in &targets {
        let w = if flags & ext_flag::UNRELIABLE != 0 { messages::one_unreliable(Msg::TsExt, e) } else { messages::one(Msg::TsExt, e) };
        payload.iter().fold(w.string(&name), |w, &b| w.byte(b as i32)).end();
    }
    targets.len() as c_int
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_plugin_api::{EXT_CHANNEL_MAX, EXT_MESSAGE_MAX, OK};

    #[test]
    fn the_api_limits_are_the_wire_limits() {
        assert_eq!(EXT_CHANNEL_MAX, ext::CHANNEL_MAX);
        assert_eq!(EXT_MESSAGE_MAX, ext::MESSAGE_MAX);
        assert_eq!(Msg::TsExt.name(), c"TSExt");
    }

    #[test]
    fn checks_come_in_the_api_order() {
        let fits = vec![0u8; ext::max_payload(4)];
        assert_eq!(check(true, b"test", &fits, 0, true).map(|b| b.len()), Ok(ext::MESSAGE_MAX));
        assert_eq!(check(true, b"test", b"x", ext_flag::UNRELIABLE, true).map(|_| OK), Ok(OK));
        assert_eq!(check(false, b"bad name", b"", 0, false), Err(NOT_PLAYER));
        assert_eq!(check(true, b"test", &[0; 188], 0, true), Err(INVALID), "one byte too many");
        assert_eq!(check(true, b"", b"", 0, true), Err(INVALID));
        assert_eq!(check(true, b"test", b"", 4, true), Err(INVALID), "an unknown flag");
        assert_eq!(check(true, b"test", b"", 0, false), Err(UNSUPPORTED), "not a port client");
        assert_eq!(check(true, b"bad name", b"", 0, false), Err(INVALID), "arguments before the client");
    }

    #[test]
    fn versions_by_client() {
        assert_eq!(version_of(ConnectingClient::Port("3.0.8")), Ok("3.0.8".into()));
        assert_eq!(version_of(ConnectingClient::Port("3000009")), Ok("3.0.9".into()), "the first connect's number");
        assert_eq!(version_of(ConnectingClient::Port("1")), Err(UNSUPPORTED), "the placeholder of clients from before #303");
        assert_eq!(version_of(ConnectingClient::Other), Ok(String::new()), "TS 3.0's client");
        assert_eq!(version_of(ConnectingClient::Bot), Ok(String::new()));
        assert_eq!(version_of(ConnectingClient::Hltv), Ok(String::new()));
    }
}
