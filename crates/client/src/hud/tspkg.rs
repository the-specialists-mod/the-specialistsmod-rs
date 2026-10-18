//! Port addition: the client's `TSPkg` hook (`docs/implementation/client-plugins.md` §6.2, §12,
//! issue #298). The server sends `TSPkg` only to port clients from `port::TSPKG_SINCE` on, so
//! every such client must hook it: a GoldSrc client can fail on a user message it hasn't hooked.
//!
//! The hook reassembles and decodes each body, prints it to the console, and hands it to the
//! offer flow (`crate::plugins::offers`, issues #299, #302): offers, withdrawals, op 5's card, and
//! ops 3 and 4.

use goldsrc::Global;
use shared::protocol::tspkg::{Message, Reassembler};
use std::cell::RefCell;

static REASSEMBLER: Global<RefCell<Option<Reassembler>>> = Global(RefCell::new(None));

/// A `TSPkg` (`shared::protocol::Msg::TsPkg`, hooked with the HUD's messages; the extensions'
/// interception hooks never see it).
pub fn on_message(buf: &[u8]) {
    let got = REASSEMBLER.0.borrow_mut().get_or_insert_with(Reassembler::default).push(buf);
    match got {
        Ok(None) => {}
        Ok(Some(m)) => {
            goldsrc::client::engine().print(&describe(&m));
            crate::plugins::offers::on_message(&m);
        }
        Err(e) => goldsrc::client::engine().print(&format!("[ts] tspkg: a TSPkg the client can't read: {e}\n")),
    }
}

/// The console line for a body.
pub fn describe(m: &Message) -> String {
    match m {
        Message::Offer(o) => format!(
            "[ts] tspkg: the server offers the client plugin {} {} ({} bytes{}, {}){}\n",
            o.id,
            o.version,
            o.size,
            if o.required() { ", required" } else { "" },
            o.url,
            if o.note.is_empty() { String::new() } else { format!(": {}", o.note) }
        ),
        Message::Withdraw { id, version } => format!("[ts] tspkg: the server withdrew its offer of {id} {version}\n"),
        Message::Require { id, requirement, .. } => format!("[ts] tspkg: the server requires {id} {requirement}\n"),
        Message::Block { id, requirement, reason, action } => format!(
            "[ts] tspkg: the server blocks {id} {requirement} ({}): {reason}\n",
            if *action == shared::protocol::tspkg::BlockAction::Kick { "kick" } else { "suspend" }
        ),
        Message::PortVersion { requirement, url } => {
            format!("[ts] version: this server needs The Specialists port {requirement}; get it via the TS launcher or {url}\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let m = Message::PortVersion { requirement: "=3.0.9".into(), url: "tsmod.org".into() };
        assert_eq!(describe(&m), "[ts] version: this server needs The Specialists port =3.0.9; get it via the TS launcher or tsmod.org\n");
        let o = Message::Offer(shared::protocol::tspkg::Offer {
            id: "demo".into(),
            version: "0.1.0".into(),
            size: 10,
            sha256: [0; 32],
            flags: 1,
            url: "http://h/p.tspkg".into(),
            note: "hi".into(),
        });
        assert!(
            describe(&o)
                .starts_with("[ts] tspkg: the server offers the client plugin demo 0.1.0 (10 bytes, required, http://h/p.tspkg): hi\n")
        );
    }
}
