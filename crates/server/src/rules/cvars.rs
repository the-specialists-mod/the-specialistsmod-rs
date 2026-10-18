//! TS 3.0 server cvars (decomp server/architecture.md §2.1), registered in `GameDLLInit` order
//! with the TS names, defaults and flags. Values are read by name, so a cvar another package or
//! the engine registered first still works.

use goldsrc::server::engine;
use goldsrc::sys::cvar_t;
use shared::pm::Arith;
use std::cell::Cell;
use std::ffi::CStr;

const SERVER: i32 = 0x4; // FCVAR_SERVER
const UNLOGGED: i32 = 0x100; // FCVAR_UNLOGGED

/// (name, default, flags), in TS registration order. All of these are registered by TS's
/// `GameDLLInit` except the voice pair (`CVoiceGameMgr::Init`) and the port's (`ts_bot_*`, `sv_client_*`, `sv_plugin_*`, `ts_dev_tspkg`).
/// The port's `ts_pm_arith` is registered after them ([`pm_arith`]).
pub const CVARS: &[(&str, &str, i32)] = &[
    ("displaysoundlist", "0", 0),
    ("mp_teamplay", "0", SERVER),
    ("mp_fraglimit", "0", SERVER),
    ("mp_timelimit", "0", SERVER),
    ("mp_fragsleft", "0", SERVER | UNLOGGED),
    ("mp_timeleft", "0", SERVER | UNLOGGED),
    ("mp_friendlyfire", "0", SERVER),
    ("mp_falldamage", "0", SERVER),
    ("mp_weaponstay", "0", SERVER),
    ("mp_forcerespawn", "1", SERVER),
    ("mp_autocrosshair", "1", SERVER),
    ("decalfrequency", "30", SERVER),
    ("mp_teamlist", "hitman;gordon", SERVER),
    ("mp_teammodels", "", SERVER),
    ("mp_teamoverride", "1", 0),
    ("mp_defaultteam", "0", 0),
    ("mp_allowmonsters", "0", SERVER),
    ("mp_chattime", "10", SERVER),
    ("roundtime", "150", SERVER),
    ("respawntime", "3", SERVER),
    ("damagemult", "0.0", SERVER),
    ("slowmatch", "1.0", SERVER),
    ("botlearn", "0.0", SERVER),
    ("nopowerup", "0.0", SERVER),
    ("realbullet", "0.0", SERVER),
    ("usecash", "0.0", SERVER),
    ("ammocount", "0", SERVER),
    ("bottalk", "1", SERVER),
    ("weaponstay", "15", SERVER),
    ("mp_theonemode", "0", SERVER),
    ("mp_theone", "gordon", SERVER),
    ("mp_theothers", "seal", SERVER),
    ("lastmanstanding", "0", SERVER),
    ("spectacularness", "1", SERVER),
    ("customweapons", "", SERVER),
    ("banfortk", "0", SERVER),
    ("botarmor", "0", SERVER),
    // CVoiceGameMgr::Init
    ("voice_serverdebug", "0", 0),
    ("sv_alltalk", "0", 0),
    // Port addition: fake clients for unattended server tests (bots/mod.rs).
    ("ts_bot_quota", "0", 0),
    // Port addition: 1 = bots stand still and hold fire (they still respawn), for scripted tests.
    ("ts_bot_stop", "0", 0),
    // Port addition: the bot backend, read at map start (`bot_backend::map_start`): `native` (TS's
    // bots) or a name a `tsmod_*` plugin registered. A change mid-map reloads the map (`reload`).
    ("ts_bot_backend", "native", 0),
    // Port additions: the port client's version check at connect (`client_version`, issue #300).
    // `FCVAR_SERVER`, so the launcher's server browser reads them from the A2S rules.
    ("sv_client_version", "any", SERVER),
    ("sv_client_require_port", "0", SERVER),
    ("sv_client_version_url", "", 0),
    // Port additions: client plugin offers (`plugin_offers`, issue #298).
    ("sv_plugin_required_kick", "0", 0),
    ("sv_plugin_check_time", "15", 0),
    // Port addition: the required-offer kick's wait for downloads (`plugin_offers::enforce`, #302).
    ("sv_plugin_download_time", "300", 0),
    // Port dev aid (sv_cheats 1): every port client counts as hooking TSPkg.
    ("ts_dev_tspkg", "0", 0),
];

/// TS defines these `cvar_t`s but never registers them, so they're constants (architecture.md
/// §2.1): `initial_cash` "1200", `cashforwin` "10000", `cashforlose` "1200", `respawnnumber`
/// (value 0, clamped to 1), `dmgreport` 0.
pub const INITIAL_CASH: i32 = 1200;
pub const CASH_FOR_WIN: i32 = 10000;
pub const CASH_FOR_LOSE: i32 = 1200;
pub const RESPAWN_NUMBER: f32 = 1.0;

pub fn register() {
    register_extra(CVARS);
    // Port addition. Not in `CVARS`: `pm_arith` keeps the pointer, as it reads it every command.
    let e = engine();
    PM_ARITH.0.set(Some(e.register_cvar("ts_pm_arith", "msvc", SERVER)));
    // Port addition, kept like `ts_pm_arith`: read every frame (`crate::reload::frame`).
    // Its default is the default ruleset's id: the server library registers the rulesets first.
    GAMEPLAY.0.set(Some(e.register_cvar(ts_gameplay::CVAR, crate::gameplay::default_id(), SERVER)));
    crate::gameplay::register_cvars();
}

/// Registers `list` in order, skipping a name already registered: TS 3.0's (`CVARS`) at
/// `GameDLLInit`, then each gameplay plugin's own (`ServerRules::cvars`, `gameplay::register_cvars`).
pub fn register_extra(list: &[(&str, &str, i32)]) {
    // Skip cvars the engine already defines (FWGS and newer engines have sv_alltalk; old HLDS
    // leaves it to the game DLL, like TS's CVoiceGameMgr), so nothing is registered twice.
    let e = engine();
    for &(name, value, flags) in list {
        let cname = goldsrc::util::to_cstring(name);
        if !e.cvar_lookup(&cname).is_null() {
            continue;
        }
        e.register_cvar(name, value, flags);
    }
}

/// `ts_gameplay` (port addition; TS has no such cvar).
static GAMEPLAY: goldsrc::Global<Cell<Option<&'static cvar_t>>> = goldsrc::Global(Cell::new(None));

/// `ts_gameplay` (port addition): the id of the gameplay plugin the server runs
/// (`crate::gameplay`), read on every use so a change applies from the next frame.
pub fn gameplay() -> String {
    match GAMEPLAY.0.get() {
        Some(c) => goldsrc::server::cvar_string(c).to_string_lossy().into_owned(),
        None => crate::gameplay::default_id().to_owned(),
    }
}

/// `ts_pm_arith` (port addition; TS has no such cvar).
static PM_ARITH: goldsrc::Global<Cell<Option<&'static cvar_t>>> = goldsrc::Global(Cell::new(None));

/// The arithmetic the server's player movement, stunt maths and shared weapon maths
/// (`GunHost::arith`) run in, from `ts_pm_arith` (port addition), read on every call so a change
/// applies from the next command. `msvc`, the default, is `mp.dll`'s: TS's `client.dll` and this
/// port's client predict with it, so server and prediction agree exactly on every platform.
/// `gcc` is `ts_i386.so`'s (TS's own Linux server; a frame different in places, e.g. the duck
/// completes at 590 ms instead of 600). See `docs/implementation/movement.md` and `weapons.md` §5.
pub fn pm_arith() -> Arith {
    match PM_ARITH.0.get() {
        Some(c) => Arith::from_cvar(goldsrc::server::cvar_string(c).to_bytes()),
        None => Arith::SERVER_DEFAULT,
    }
}

pub fn f(name: &CStr) -> f32 {
    engine().cvar_float(name)
}

pub fn s(name: &CStr) -> String {
    engine().cvar_string(name).to_string_lossy().into_owned()
}

/// The cvar's value as its bytes (`CVAR_GET_STRING`), for text TS copies unconverted
/// (`mp_teamlist`, `mp_teammodels`).
pub fn bytes(name: &CStr) -> Vec<u8> {
    engine().cvar_string(name).to_bytes().to_vec()
}

pub fn on(name: &CStr) -> bool {
    f(name) != 0.0
}

/// `respawntime` as `CBasePlayer::Killed` (0x8ef8f) and `CTSGameRules::InitHUD` (0x79514) pass it
/// to `SetPlayerRespawnTime`: clamped to 5..30 s (`fcom 5.0; jae`, so a NaN becomes 5), then
/// `(int)` under the truncating control word, so 7.9 is 7.
pub fn respawn_time() -> f32 {
    clamp_respawn_time(f(c"respawntime"))
}

pub(crate) fn clamp_respawn_time(t: f32) -> f32 {
    let t = if t >= 5.0 { t } else { 5.0 };
    (if t > 30.0 { 30.0 } else { t }).trunc()
}

/// `roundtime`, clamped to 30..180 s as `RestartRound` (0x77941) does: `fcom 30.0; jae`, so a NaN
/// (unordered) becomes 30 too (`f32::clamp` would keep it).
pub fn round_time() -> f32 {
    let t = f(c"roundtime");
    clamp_round_time(t)
}

pub(crate) fn clamp_round_time(t: f32) -> f32 {
    let t = if t >= 30.0 { t } else { 30.0 };
    if t > 180.0 { 180.0 } else { t }
}

/// `g_flWeaponCheat`: `sv_cheats` as it was when the map's worldspawn spawned
/// (`CWorld::Spawn` 0xf4450), 0 before the first map.
static WEAPON_CHEAT: goldsrc::Global<std::cell::Cell<f32>> = goldsrc::Global(std::cell::Cell::new(0.0));

/// `CWorld::Spawn` (0xf4450): latch `g_flWeaponCheat` for the map.
pub fn latch_weapon_cheat() {
    WEAPON_CHEAT.0.set(f(c"sv_cheats"));
}

/// TS's cheat gate, `g_flWeaponCheat != 0`: the cheat commands (`fov`, `powerup`, `cashala`),
/// `CheatImpulseCommands` and the `InitHUD` cheat notice. A new `sv_cheats` takes effect on the
/// next map.
pub fn cheats() -> bool {
    WEAPON_CHEAT.0.get() != 0.0
}

/// `sv_cheats` now, for the port's own dev aids (`ts_tp`, `ts_dev_touch`, ...; not TS).
pub fn dev_cheats() -> bool {
    f(c"sv_cheats") != 0.0
}

#[cfg(test)]
mod tests {
    use super::{clamp_respawn_time, clamp_round_time};

    #[test]
    fn round_and_respawn_time_clamps() {
        // `fcom lo; jae`: a NaN takes the low bound.
        assert_eq!(clamp_round_time(f32::NAN), 30.0);
        assert_eq!(clamp_round_time(10.0), 30.0);
        assert_eq!(clamp_round_time(95.5), 95.5);
        assert_eq!(clamp_round_time(500.0), 180.0);
        assert_eq!(clamp_respawn_time(f32::NAN), 5.0);
        assert_eq!(clamp_respawn_time(1.0), 5.0);
        // Truncated by the `(int)` cast.
        assert_eq!(clamp_respawn_time(7.9), 7.0);
        assert_eq!(clamp_respawn_time(29.5), 29.0);
        assert_eq!(clamp_respawn_time(99.0), 30.0);
    }
}
