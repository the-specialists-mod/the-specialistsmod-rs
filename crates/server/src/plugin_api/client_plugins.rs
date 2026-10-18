//! Version 8 (port addition, issue #321): the client plugin offers in the plugin API
//! (docs/implementation/client-plugins.md §6.1, §13.3). `PluginApi::offer_client_plugin` offers a
//! checked `plugin_offers.toml` entry as `sv_plugin_offer` does, and
//! `PluginApi::client_plugin_version` reads what `crate::plugin_offers` keeps of a client's answers
//! and `tspkg list`. The events (`Plugin::client_plugin_state`, `client_plugin_list`) are fired
//! from `plugin_offers::client_command` through `crate::plugins`.
//!
//! Only clients that hook `TSPkg` (`plugin_offers::hooks_tspkg`: a port client from
//! `port::TSPKG_SINCE` on, or with the `ts_dev_tspkg` dev aid) can be offered anything or run
//! client plugins the server can learn of; every other connected player is `UNSUPPORTED`, as for
//! `ext_message`.

use super::Game;
use crate::plugin_offers;
use goldsrc::server::engine;
use goldsrc::sys::edict_t;
use shared::protocol::tspkg::State;
use std::ffi::c_int;
use ts_plugin_api::{INVALID, NOT_PLAYER, UNSUPPORTED, plugin_state};

/// The API's order of checks: the player (`NOT_PLAYER`), the arguments (`INVALID`), then whether
/// that client takes part in offers (`UNSUPPORTED`).
pub fn check(player_ok: bool, args_ok: bool, hooks: bool) -> Result<(), c_int> {
    if !player_ok {
        return Err(NOT_PLAYER);
    }
    if !args_ok {
        return Err(INVALID);
    }
    if !hooks {
        return Err(UNSUPPORTED);
    }
    Ok(())
}

/// A `tspkg` state as `ts_plugin_api::plugin_state`.
pub fn state_code(s: State) -> c_int {
    match s {
        State::Have => plugin_state::HAVE,
        State::Offered => plugin_state::OFFERED,
        State::Downloading => plugin_state::DOWNLOADING,
        State::Installed => plugin_state::INSTALLED,
        State::Loaded => plugin_state::LOADED,
        State::Denied => plugin_state::DENIED,
        State::Blocked => plugin_state::BLOCKED,
        State::Unsupported => plugin_state::UNSUPPORTED,
        State::Failed => plugin_state::FAILED,
        State::Suspended => plugin_state::SUSPENDED,
        State::Restart => plugin_state::RESTART,
    }
}

/// `PluginApi::offer_client_plugin` (`id` `None` for NULL): the number of clients the offer went
/// to, or a status.
pub fn offer_client_plugin(player: c_int, id: Option<&str>) -> c_int {
    let index = id.and_then(plugin_offers::ready_index);
    if player == 0 {
        if let Err(code) = check(true, index.is_some(), true) {
            return code;
        }
        let Some(index) = index else { return INVALID };
        let targets: Vec<*mut edict_t> =
            (1..=engine().max_clients()).map(Game::edict).filter(|e| !e.is_null() && plugin_offers::hooks_tspkg(*e)).collect();
        return targets.into_iter().filter(|&e| plugin_offers::offer(e, index)).count() as c_int;
    }
    let e = Game::edict(player);
    if let Err(code) = check(!e.is_null(), index.is_some(), !e.is_null() && plugin_offers::hooks_tspkg(e)) {
        return code;
    }
    match index {
        Some(index) => plugin_offers::offer(e, index) as c_int,
        None => INVALID,
    }
}

/// `PluginApi::client_plugin_version` (`id` `None` for NULL): the version, "" when the server
/// doesn't know it loaded, or a status.
pub fn client_plugin_version(player: c_int, id: Option<&str>) -> Result<String, c_int> {
    let e = Game::edict(player);
    check(!e.is_null(), id.is_some(), !e.is_null() && plugin_offers::hooks_tspkg(e))?;
    Ok(id.and_then(|id| plugin_offers::loaded_version(e, id)).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_plugin_api::OK;

    #[test]
    fn checks_come_in_the_api_order() {
        assert_eq!(check(true, true, true).map(|_| OK), Ok(OK));
        assert_eq!(check(false, false, false), Err(NOT_PLAYER));
        assert_eq!(check(true, false, false), Err(INVALID), "arguments before the client");
        assert_eq!(check(true, true, false), Err(UNSUPPORTED), "TS 3.0's client, an older port client, a bot");
    }

    #[test]
    fn states_are_numbered_in_the_protocols_order() {
        for (i, s) in State::ALL.into_iter().enumerate() {
            assert_eq!(state_code(s), i as c_int, "{s}");
        }
        assert_eq!((plugin_state::HAVE, plugin_state::RESTART), (0, 10));
    }
}
