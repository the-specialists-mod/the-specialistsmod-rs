//! Port addition: the client's side of version matching (`docs/implementation/client-plugins.md`
//! §13.1, §13.4, issue #300). The server's version comes in `SrvSett` keys 5..7
//! (`shared::protocol::port`); a difference from this client's is printed, and with
//! `cl_version_strict 1` the client leaves the server.

use goldsrc::client::Cvar;
use shared::protocol::port;
use std::ffi::CStr;

/// `cl_version_strict` (port addition): 1 disconnects from a server whose port version isn't
/// this client's (the server's `exact`).
pub const STRICT_CVAR: &CStr = c"cl_version_strict";

pub fn register() {
    Cvar::register(STRICT_CVAR, c"0", 1); // FCVAR_ARCHIVE
}

/// What to do about the server's version: `None` when it is this client's; otherwise the console
/// line, and whether to disconnect (`strict`).
pub fn on_server_version(server: [i16; 3], own: [i16; 3], strict: bool) -> Option<(String, bool)> {
    if server == own {
        return None;
    }
    let v = |[a, b, c]: [i16; 3]| format!("{a}.{b}.{c}");
    let (server, own) = (v(server), v(own));
    Some(if strict {
        (
            format!(
                "[ts] version: this server runs The Specialists port {server}, this client is {own}; cl_version_strict 1 disconnects\n"
            ),
            true,
        )
    } else {
        (format!("[ts] version: this server runs The Specialists port {server}, this client is {own}\n"), false)
    })
}

/// The HUD has the server's full version (`SrvSett` 7 after 5 and 6).
pub fn apply(server: [i16; 3]) {
    crate::plugins::offers::new_game(server);
    let e = goldsrc::client::engine();
    let strict = Cvar::find(STRICT_CVAR).value() != 0.0;
    if let Some((line, disconnect)) = on_server_version(server, port::version_numbers(), strict) {
        e.print(&line);
        if disconnect {
            e.client_cmd("disconnect\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_differences_only() {
        assert_eq!(on_server_version([3, 0, 8], [3, 0, 8], true), None);
        let (line, leave) = on_server_version([3, 0, 8], [3, 0, 7], false).unwrap();
        assert!(!leave && line.contains("port 3.0.8, this client is 3.0.7\n"), "{line}");
        let (line, leave) = on_server_version([3, 1, 0], [3, 0, 7], true).unwrap();
        assert!(leave && line.contains("cl_version_strict 1"), "{line}");
    }
}
