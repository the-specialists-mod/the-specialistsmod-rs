//! User message registration (TS order) and send helpers. Payload layouts per message are in the
//! decomp docs; each subsystem writes its own messages with these helpers.

use goldsrc::Global;
use goldsrc::server::{MsgWriter, engine};
use goldsrc::sys::*;
use shared::protocol::MESSAGES;
pub use shared::protocol::Msg;
use std::cell::Cell;

static IDS: Global<[Cell<i32>; MESSAGES.len()]> = Global([const { Cell::new(0) }; MESSAGES.len()]);

/// Registers every message in TS order, then the port's own (`TSExt`, after TS's 64). Called once from `GameInit`. From then on every message
/// the game sends passes the active gameplay plugin's `ServerRules::outgoing_message`.
pub fn register_all() {
    for (m, name, size) in MESSAGES {
        IDS.0[*m as usize].set(engine().reg_user_msg(name, *size));
    }
    goldsrc::server::set_outgoing_filter(Some(outgoing));
}

pub fn id(m: Msg) -> i32 {
    IDS.0[m as usize].get()
}

/// The message registered as `id` (a user message id the engine handed out), if any.
pub fn msg_of(id: i32) -> Option<Msg> {
    if id == 0 {
        return None;
    }
    MESSAGES.iter().map(|&(m, _, _)| m).find(|&m| IDS.0[m as usize].get() == id)
}

/// The outgoing filter: a user message goes to the gameplay plugin, anything else (temp entities,
/// `svc_*`) straight out.
fn outgoing(w: MsgWriter, send: &mut dyn FnMut(MsgWriter)) {
    match msg_of(w.msg_type()) {
        Some(m) => crate::gameplay::active().server.outgoing_message(m, w, send),
        None => send(w),
    }
}

/// Reliable message to one client.
pub fn one(m: Msg, to: *mut edict_t) -> MsgWriter {
    MsgWriter::begin(MSG_ONE as i32, id(m), None, to)
}

/// Unreliable message to one client (dropped if the channel is full).
pub fn one_unreliable(m: Msg, to: *mut edict_t) -> MsgWriter {
    MsgWriter::begin(MSG_ONE_UNRELIABLE as i32, id(m), None, to)
}

pub fn all(m: Msg) -> MsgWriter {
    MsgWriter::begin(MSG_ALL as i32, id(m), None, std::ptr::null_mut())
}

pub fn pvs(m: Msg, origin: [f32; 3]) -> MsgWriter {
    MsgWriter::begin(MSG_PVS as i32, id(m), Some(origin), std::ptr::null_mut())
}

pub fn pas(m: Msg, origin: [f32; 3]) -> MsgWriter {
    MsgWriter::begin(MSG_PAS as i32, id(m), Some(origin), std::ptr::null_mut())
}
