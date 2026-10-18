//! Game rules: modes (DM, TDM, LMS, The One), spawning, scoring, power-ups, slow motion
//! propagation, chat, cvars. Owner: rules work package.
//! Reference: decomp docs/server/gamerules-entities.md, architecture.md §2, §4.
//!
//! Layout:
//! * `state`: the `GameRules` singleton (`g_pGameRules` + `CTSGameRules`), mode decision, teams.
//! * `cvars`: TS cvar registration. `commands`: client and server commands. `msg`: payload send.
//! * `lifecycle`: connect, put-in-server, InitHUD, spawn, observer/respawn, think, disconnect.
//! * `scoring`: `PlayerKilled` (points, sprees, awards, cash, DeathMsg, ScoreInfo).
//! * `spawnpoint`: `EntSelectSpawnPoint`/`TSSelectSpawnSpot`.
//! * `powerup`: held/active power-ups, slow-motion flow and easing.
//! * `frame`: `StartFrame` (`CHalfLifeMultiplay::Think` + `CTSGameRules::Think`).
//! * `teams`: teamplay assignment and messages.
//! * `votes`: chat map votes (`vote map/stats/reset`, `CheckVotes`).
//! * `voice`: `CVoiceGameMgr` (who hears whom, `vban`, `VoiceMask`/`ReqState`).
//! * `client_version`: the port client's version check at connect (port addition).

pub mod client_version;
pub mod commands;
pub mod cvars;
pub mod frame;
pub mod lifecycle;
pub mod mapcycle;
pub mod msg;
pub mod powerup;
pub mod scoring;
pub mod spawnpoint;
pub mod state;
pub mod teams;
pub mod voice;
pub mod votes;

use crate::commands::Args;
use crate::player::Player;
use crate::world::{Cx, DamageInfo};
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::borrow::Cow;
use std::ffi::CStr;

/// Per-player rules state (`CBasePlayer` TS fields; offsets from player-movement.md §8).
#[derive(Default)]
pub struct PlayerRules {
    // ---- HUD / join ------------------------------------------------------------------------
    /// `m_fInitHUD`: send ResetHUD (and InitHUD the first time) on the next think.
    pub init_hud: bool,
    /// `m_fGameHUDInitialized`.
    pub game_hud_initialized: bool,
    /// Put in server (cash given) once.
    pub put_in_server: bool,
    /// Throttle for the respawn countdown HUD text (`+0x754`).
    pub next_hud_msg: f32,
    /// Buttons last frame (`m_afButtonLast`) and the edges this frame.
    pub buttons_last: i32,
    pub buttons_pressed: i32,

    // ---- life / respawn ----------------------------------------------------------------------
    /// LMS: deadline to join this round (`+0x7bc`), respawns used (`+0x7ba`).
    pub join_deadline: f32,
    pub respawns_used: u8,
    /// Death time + 3 s: become an observer (`+0x768`).
    pub observer_at: f32,
    /// Delayed suicide (3.0's `kill`, +1 s, `+0x84c`).
    pub suicide_at: f32,
    /// `m_fNextSuicideTime`: the SDK's `kill` cooldown, for a ruleset whose `kill` is immediate
    /// (2.1 `+0x7a8`). Nothing else reads or writes it, so it survives a respawn.
    pub next_suicide: f32,
    /// Last model accepted (HLDM model check) and the team name (`m_szTeamName`, bytes: #188).
    pub team_name: Vec<u8>,
    /// Spawn protection end (`+0x71c`).
    pub protect_until: f32,
    /// Observer: next allowed button action (`+0x760`), next `Observer_Idle` check (`+0x764`),
    /// saved mode (`+0x76c`).
    pub obs_next_input: f32,
    pub obs_next_idle: f32,
    pub obs_mode: i32,
    /// `m_hObserverTarget` (`+0x75c`), a player index (0 = none). Set by `Killed` and
    /// `Observer_FindNextPlayer`; survives a respawn like TS's `EHANDLE`.
    pub obs_target: i32,
    /// Chat flood control (`+0x7c0`; 2.1 `+0x738`): `Host_Say` sets it 0.5 s ahead, `TSInit`
    /// zeroes it at every spawn (0x92a4c; 2.1 0x100552f0).
    pub next_say: f32,
    /// `m_iFOV` (`+0x9f0`): set only by the `fov` cheat, zeroed by `Spawn`, `Killed` and
    /// `StartObserver`; the `fov` reply prints it.
    pub ifov: i32,

    // ---- scoring -------------------------------------------------------------------------------
    /// Kill spree (`+0x744`), time of the last kill (`+0x748`).
    pub spree: u8,
    pub last_kill: f32,
    /// `TSPAward` status (`+0x74c`).
    pub award: u8,
    /// Team-kill window and count (`+0x7c4`/`+0x7c8`).
    pub tk_until: f32,
    pub tk_count: u8,
    pub is_the_one: bool,

    // ---- damage ----------------------------------------------------------------------------------
    pub last_hitgroup: i32,
    pub last_damage: i32,
    /// `PTakeDam` throttle (`+0x750`).
    pub next_ptakedam: f32,
    /// Bullet dodge window end (`+0x524`, set by the stunt package through `dodge_until`).
    pub dodge_until: f32,

    // ---- power-ups / slow motion -----------------------------------------------------------------
    /// Held (stored) power-up and its seconds (`+0x728`/`+0x730`).
    pub held: u16,
    pub held_secs: i32,
    /// Health decay / The One regen timer (`+0x73c`).
    pub health_timer: f32,
    /// Displayed slow amount (`+0x170`) and bullet-time flag (`+0x175`).
    pub slow_display: f32,
    pub btime: bool,
    /// Activator amounts: self (`+0x17c`), others (`+0x180`); 0 = not an activator.
    pub slow_self: f32,
    pub slow_others: f32,
    /// Next `SlowMotionFlow` (`+0x178`).
    pub next_flow: f32,
    /// Players this activator slowed (bit per player index) and projectiles.
    pub slowed: u64,
    pub slowed_ents: Vec<*mut edict_t>,

    // ---- server plugins (port additions, `crate::plugin_api`) ------------------------------------
    /// The running power-up (`Player::active_powerup`) was started by a plugin
    /// (`powerup::run_powerup`), not by `UsePowerUp`: it isn't the held one, so its end leaves
    /// [`held`](Self::held) alone and drops nothing.
    pub plugin_powerup: bool,
    /// A plugin's time scale for the player and the players around it (`powerup::plugin_speed_think`).
    pub plugin_speed: Option<powerup::PluginSpeed>,
    /// A plugin's override of the `TSSlowMo` amount (`powerup::hud_slow_amount`): the time scale of
    /// the player's client-side effects.
    pub physics_speed: Option<f32>,

    // ---- capture the briefcase ---------------------------------------------------------------------
    /// The case this player carries (`s_flag_status` at `+0x7d0`).
    pub flag: Option<crate::entities::ts::ctb::FlagStatus>,

    // ---- objectives ----------------------------------------------------------------------------
    /// The bomb wires the HUD shows (`+0x78c..+0x78e`) and the last sent (`+0x78f..+0x791`);
    /// `TSInit` zeroes both.
    pub wires: [u8; 3],
    pub wires_sent: [u8; 3],
    /// The next `CheckForWires` (`+0x794`).
    pub next_wire_check: f32,
}

/// The rules' part of `GameDLLInit` (0xc5aa0): the cvars, the server commands, and the
/// `exec skill.cfg` it queues last (TS registers no `sk_*` cvars, but the command still goes out).
pub fn game_init() {
    cvars::register();
    commands::register_server_commands();
    crate::plugin_offers::register_server_commands();
    engine().server_command("exec skill.cfg\n");
}

/// `InstallGameRules` (0xc6050), from `CWorld::Precache` (worldspawn's keyvalues are parsed by
/// then): `exec game.cfg` run at once, the rules decided and built. The multiplayer rules'
/// constructor (0xcedd0) runs `CVoiceGameMgr::Init`, which precaches the voice icon, and queues
/// the server config; `CHalfLifeRules` (0xde6fc) does neither.
pub fn install() {
    let e = engine();
    e.server_command("exec game.cfg\n");
    e.server_execute();
    // Port addition: the gameplay plugin `ts_gameplay` selects decides the new game's rules.
    crate::gameplay::map_select();
    // Port addition: `ts_bot_backend` picks the bot backend for the map (game.cfg has run).
    crate::bot_backend::map_start();
    start_rules();
    if !state::single_player() {
        voice::precache();
    }
    exec_server_config();
    // Port addition: the selected gameplay plugin takes over the new game.
    crate::gameplay::map_start();
    // Port addition: what a reload asked for is what this map runs (or a map start set it back).
    crate::reload::map_start();
    // Port addition: `plugin_offers.toml`, read and checked for the map (issue #298).
    crate::plugin_offers::map_start();
}

/// The rules of a new game: the rules decided and built, and `CTSGameRules::Init`'s node graph
/// and bot texts.
fn start_rules() {
    state::with(|g| g.install());
    // mp.dll's `CTSGameRules::Init` (from the rules' constructor) also loads the node graph and
    // the bot texts.
    crate::bot_backend::map_init();
    crate::entities::ts::map_init();
}

/// The tail of the `CHalfLifeMultiplay` constructor (0xcedd0): queue `exec <servercfgfile>` on a
/// dedicated server (`lservercfgfile` on a listen server) on every map. It isn't executed here,
/// so it runs once the map has loaded and overrides what `CWorld::Precache` forced
/// (`sv_gravity`, `mp_defaultteam`, ...). `InstallGameRules` builds the multiplayer rules only
/// when `deathmatch` is set.
fn exec_server_config() {
    let e = engine();
    if e.globals().deathmatch == 0.0 {
        return;
    }
    let (var, what) = if e.is_dedicated() { (c"servercfgfile", "dedicated") } else { (c"lservercfgfile", "listen") };
    let file = e.cvar_string(var).to_string_lossy().into_owned();
    if file.is_empty() {
        return;
    }
    e.alert(ALERT_TYPE::at_console, &format!("Executing {what} server config file\n"));
    // A `ts_gameplay` or `ts_bot_backend` the config changes reloads the map (`crate::reload`).
    e.server_command(&format!("exec {file}\n"));
}

/// `ServerActivate` (0xae928): `Activate` on the map's entities (`entities::server_activate`),
/// then the port's bot manager.
pub fn server_activate(edict_count: i32, client_max: i32) {
    crate::entities::server_activate(edict_count, client_max);
    crate::bot_backend::server_activate();
    crate::plugins::server_activate();
}

pub fn server_deactivate() {
    crate::bot_backend::server_deactivate();
    crate::plugins::server_deactivate();
}

/// Called every server frame (`pfnStartFrame`).
pub fn start_frame() {
    // Port addition: a `ts_gameplay` or `ts_bot_backend` change reloads the map.
    if state::with(|g| g.installed) {
        crate::reload::frame();
    }
    frame::think();
    // Port addition: the op 5 kicks (`plugin_offers`).
    crate::plugin_offers::start_frame();
}

/// "The Specialists v<version>", then ` (<mode>)` if given, as a `&'static CStr` constant. The
/// version is the calling package's, which in this workspace is the workspace's and follows the
/// release tags (`v3.0.2`), so each release names itself. Exported for the gameplay plugins'
/// [`ServerRules::game_description`](crate::gameplay::ServerRules::game_description).
#[macro_export]
macro_rules! game_description {
    ($($mode:literal)?) => {
        match ::std::ffi::CStr::from_bytes_with_nul(
            concat!("The Specialists v", env!("CARGO_PKG_VERSION"), $(" (", $mode, ")",)? "\0").as_bytes(),
        ) {
            Ok(c) => c,
            Err(_) => panic!("the package version has a NUL"),
        }
    };
}

/// Which description [`game_description`] asks the active ruleset for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptionMode {
    /// `CHalfLifeRules`: `deathmatch` 0 when the rules were installed.
    Single,
    Dm,
    Lms,
    Teamplay,
    TheOne,
}

/// `GetGameDescription` (0xaee84): `"Half-Life"` before the rules exist, `"The Specialists"`
/// for `CHalfLifeRules` (`deathmatch` 0 when they were installed), otherwise the multiplayer
/// rules' text for the mode (`CHalfLifeMultiplay` 0xd0c68, `CHalfLifeTeamplay` 0xe5a04). The
/// active ruleset words it ([`ServerRules::game_description`](crate::gameplay::ServerRules::game_description)).
pub fn game_description() -> &'static CStr {
    let (installed, single) = state::with(|g| (g.installed, g.single_player));
    if !installed {
        return c"Half-Life";
    }
    let mode = if single {
        DescriptionMode::Single
    } else {
        let (_, teamplay, lms, the_one) = state::mode();
        match (teamplay, lms, the_one) {
            (_, _, true) => DescriptionMode::TheOne,
            (_, true, _) => DescriptionMode::Lms,
            (true, _, _) => DescriptionMode::Teamplay,
            _ => DescriptionMode::Dm,
        }
    };
    crate::gameplay::active().server.game_description(mode)
}

/// worldspawn keyvalues the rules care about (`gamemode`, `mapteams`, `teammodels`). Return true
/// if handled. `defaultteam` is a worldspawn spawnflag that `CWorld::Precache` turns into
/// `mp_defaultteam` (entities::stock `World`).
pub fn world_key_value(key: &CStr, value: &CStr) -> bool {
    let v = value.to_str().unwrap_or("").trim().to_owned();
    // `mapteams` and `teammodels` are kept as the map's bytes (`CWorld::KeyValue` 0xf4a18
    // `ALLOC_STRING`s / `strcpy`s them; #188).
    let raw = value.to_bytes().to_vec();
    match key.to_bytes() {
        b"gamemode" => state::with(|g| g.forced_gamemode = v.parse().unwrap_or(0)),
        b"mapteams" => state::with(|g| g.map_teams = raw),
        b"teammodels" => state::with(|g| g.map_team_models = raw),
        _ => return false,
    }
    true
}

/// `ClientConnect`: `Err(reason)` refuses the client, for the engine's reject buffer. Port
/// additions: the gameplay ruleset may refuse a client first (`gameplay::refuse_client`), with
/// its own reason; then the port client's version check (`client_version`, issue #300); both
/// before TS's part counts it.
pub fn client_connect(e: *mut edict_t, name: &CStr, addr: &CStr) -> Result<(), Cow<'static, str>> {
    if let Some(reason) = crate::gameplay::refuse_client(e) {
        return Err(reason.into());
    }
    let notice = match check_client_version(e, name) {
        client_version::Verdict::Admit => None,
        client_version::Verdict::Refuse(reason) => return Err(reason.into()),
        client_version::Verdict::Notify { requirement, reason } => {
            let url = client_version::notice_url(&cvars::s(client_version::CVAR_URL));
            Some(crate::plugin_offers::clients::PortNotice { requirement, url, reason, kick_at: None })
        }
    };
    if !lifecycle::client_connect(e, name, addr) {
        return Err("Connection rejected by game".into());
    }
    // Port addition: offers of client plugins and op 5 (`plugin_offers`, issue #298).
    crate::plugin_offers::client_connect(e, notice);
    crate::plugins::client_connect(e);
    Ok(())
}
/// The port client's version check (`client_version::decide`) with the server's cvars; logs a
/// refusal, a client let in to be told by op 5, and a `sv_client_version` it can't read (which
/// then admits every version).
fn check_client_version(e: *mut edict_t, name: &CStr) -> client_version::Verdict {
    use client_version as cv;
    let eng = engine();
    let policy = cv::Policy::parse(&cvars::s(cv::CVAR_VERSION)).unwrap_or_else(|bad| {
        eng.print(&format!(
            "[ts] version: sv_client_version \"{bad}\" isn't any, same_minor, exact or a semver requirement; every version may join\n"
        ));
        cv::Policy::Any
    });
    let settings = cv::Settings { policy, require_port: cvars::on(cv::CVAR_REQUIRE_PORT), url: cvars::s(cv::CVAR_URL) };
    let server = shared::protocol::port::version_numbers();
    crate::gameplay::with_connecting_client(e, |client| {
        let verdict = cv::decide(&settings, server, client, crate::plugin_offers::version_hooks_tspkg);
        match &verdict {
            cv::Verdict::Admit => {}
            cv::Verdict::Refuse(reason) => eng.print(&format!("[ts] version: refused {} ({client:?}): {reason}\n", name.to_string_lossy())),
            cv::Verdict::Notify { reason, .. } => eng
                .print(&format!("[ts] version: let in {} ({client:?}) to tell it by TSPkg, then kick: {reason}\n", name.to_string_lossy())),
        }
        verdict
    })
}

/// `ClientDisconnect`: plugins hear of it first, while the client is still a player.
pub fn client_disconnect(e: *mut edict_t) {
    crate::plugins::client_disconnect(e);
    crate::plugin_offers::client_disconnect(e);
    crate::bot_backend::client_disconnect(e);
    lifecycle::client_disconnect(e)
}
pub fn client_kill(e: *mut edict_t) {
    lifecycle::client_kill(e)
}
pub fn userinfo_changed(e: *mut edict_t) {
    lifecycle::userinfo_changed(e)
}

/// Spawn: TS player init, spawn point, power-up/HUD state.
pub fn on_spawn(p: &mut Player, cx: &mut Cx) {
    lifecycle::on_spawn(p, cx)
}

/// Button edges and `PlayerThink`; true while the game is over (PreThink stops there).
pub fn player_think(p: &mut Player, cx: &mut Cx) -> bool {
    lifecycle::player_think(p, cx)
}

/// `tail` is false on the frame a train is lost (TS's PreThink returns before its tail). True
/// when [`pre_think_end`] is due this frame (alive, not observing, `tail`).
pub fn pre_think(p: &mut Player, cx: &mut Cx, tail: bool) -> bool {
    lifecycle::pre_think(p, cx, tail)
}

/// PreThink's last steps: the end of spawn protection and the delayed suicide (issue #155).
pub fn pre_think_end(p: &mut Player, cx: &mut Cx) {
    lifecycle::pre_think_end(p, cx)
}

pub fn post_think(p: &mut Player, cx: &mut Cx) {
    lifecycle::post_think(p, cx)
}

/// The player died (health ≤ 0 in `player::damage`): `CBasePlayer::Killed` + `PlayerKilled`.
pub fn on_killed(p: &mut Player, cx: &mut Cx, dmg: &DamageInfo) {
    crate::ham::killed(p, cx, dmg, lifecycle::killed)
}

pub fn client_command(p: &mut Player, cx: &mut Cx, cmd: &str, args: &Args) -> bool {
    commands::client_command(p, cx, cmd, args)
}

pub fn fill_client_data(p: &mut Player, cx: &mut Cx, cd: &mut clientdata_t) {
    // vuser1[1]: active power-up bits (architecture.md §7.2).
    cd.vuser1[1] = p.active_powerup as f32;
    let _ = cx;
}

// ---- helpers used across the rules modules ------------------------------------------------------

/// `IsAlive()`.
pub fn alive(v: &entvars_t) -> bool {
    v.deadflag == DEAD_NO as i32 && v.health > 0.0
}

/// Runs `f` on another player's Rust object (not the one being dispatched). No engine calls
/// that re-enter the game inside `f`.
pub fn other<R>(e: *mut edict_t, f: impl FnOnce(&mut Player) -> R) -> Option<R> {
    crate::world::with::<Player, R>(e, f)
}

/// Connected client edicts that have a `Player` object (the port's liveness test, `FL_CLIENT`).
/// TS has no such loop: its player loops are [`slot_players`]. Only port additions (bots, dev
/// commands) use this.
pub fn players() -> Vec<*mut edict_t> {
    engine().clients().filter(|&e| is_player(e)).collect()
}

/// `UTIL_PlayerByIndex` (0xec694) over slots 1..=maxClients: every client edict with a `Player`,
/// including players who left. The engine never frees a dropped client's edict or its private
/// data before the slot's next client spawns, so TS's loops still find them (with what
/// `TSPlayerDisconnect` left: no team, no frags, dead, `EF_NODRAW`, `FL_DORMANT`).
pub fn slot_players() -> Vec<*mut edict_t> {
    let eng = engine();
    (1..=eng.max_clients()).map(|i| eng.player(i)).filter(|&e| has_player(e)).collect()
}

/// A non-free edict with a `Player` attached, connected or not. This is TS's player test in all
/// its forms: the `IsPlayer()` and `IsNetClient()` virtuals (true for any `CBasePlayer`),
/// `Classify() == CLASS_PLAYER`, a classname compare with `"player"`, and `FL_CLIENT` (see
/// [`ts_flags`]). A player who left still passes all of them until the slot's next client spawns.
///
/// It stands for TS's `IsPlayer()` call sites, so it is hookable (`Ham_IsPlayer`, `crate::ham`): a
/// plugin's hook can make a player not count as one. Use [`is_player_entity`] where the port
/// itself needs the answer (the plugin API, bookkeeping).
pub fn has_player(e: *mut edict_t) -> bool {
    let real = is_player_entity(e);
    if e.is_null() || e.is_free() {
        return real;
    }
    crate::ham::is_player(e, real)
}

/// [`has_player`] without the plugin hook.
pub fn is_player_entity(e: *mut edict_t) -> bool {
    !e.is_null() && !e.is_free() && e.has_private_data() && engine().string(e.v().classname) == c"player"
}

/// `IsNetClient()` (vtable slot 49): true for any `CBasePlayer` but TS's bots, whose `CBot`
/// (mp.dll vtable 0x10094a9c) returns 0. A player being dispatched further up the stack counts
/// as a net client (callers skip the player they run for). Hookable (`Ham_IsNetClient`).
pub fn is_net_client(e: *mut edict_t) -> bool {
    let real = is_player_entity(e) && !other(e, |p| p.cbot).unwrap_or(false);
    if e.is_null() || e.is_free() {
        return real;
    }
    crate::ham::is_net_client(e, real)
}

/// `pEntity->IsAlive()` (`deadflag == DEAD_NO && health > 0`), hookable (`Ham_IsAlive`): TS's
/// virtual call sites. [`alive`] is the plain test on entvars.
pub fn is_alive(e: *mut edict_t) -> bool {
    crate::ham::is_alive(e)
}

/// `pev->flags` as TS sees them. `CBasePlayer::Spawn` (0x8f604) sets `FL_CLIENT` and nothing in
/// TS or the engines clears it, so it stays on a player who left; the port clears it in
/// `ClientDisconnect` (engine safety, `lifecycle::client_disconnect`) and puts it back here.
pub fn ts_flags(e: *mut edict_t) -> i32 {
    ts_flags_of(e.v().flags, has_player(e))
}

/// [`ts_flags`] from the stored flags and [`has_player`].
pub fn ts_flags_of(flags: i32, has_player: bool) -> i32 {
    if has_player { flags | FL_CLIENT as i32 } else { flags }
}

/// TS's `UTIL_EntitiesInBox(.., flagMask)` (0xec1bc): the flag test reads
/// [`ts_flags`], so a player who left is still found with `FL_CLIENT` in the mask.
pub fn entities_in_box(mins: [f32; 3], maxs: [f32; 3], flag_mask: i32, max: usize) -> Vec<*mut edict_t> {
    engine().entities_in_box_by(mins, maxs, max, |e| flag_mask == 0 || ts_flags(e) & flag_mask != 0)
}

/// `CBasePlayer::SetPlayerSpeed`'s and `Observer_SetMode`'s `pfnSetClientMaxspeed`. Guard (engine
/// crash): FWGS's `pfnSetClientMaxspeed` finds a dropped client's slot and writes through its
/// NULL `cl->edict`, so for a player who left (`FL_CLIENT` cleared by the port) only
/// `pev->maxspeed` is set, which is all the Steam engines' version does.
pub fn set_client_maxspeed(e: *mut edict_t, speed: f32) {
    if e.v().flags & FL_CLIENT as i32 != 0 {
        engine().set_client_maxspeed(e, speed);
    } else {
        e.v().maxspeed = speed;
    }
}

/// True if `e` is a connected client edict with a Player attached (the port's liveness test:
/// `FL_CLIENT`, cleared on disconnect). Not a TS test; see [`has_player`].
pub fn is_player(e: *mut edict_t) -> bool {
    if e.is_null() {
        return false;
    }
    let v = e.v();
    v.flags & FL_CLIENT as i32 != 0 && e.has_private_data() && engine().string(v.classname) == c"player"
}

/// `STRING(pev->netname)`: the player's name as bytes, unconverted (TS copies it into chat, notices
/// and log lines as it is; a CP1252 name isn't UTF-8). Build text with `shared::bformat!`.
pub fn netname(e: *mut edict_t) -> Vec<u8> {
    if e.is_null() {
        return Vec::new();
    }
    engine().string(e.v().netname).to_bytes().to_vec()
}

/// A C `(int)` cast as GCC 2.95 compiles it for `ts_i386.so`: `fnstcw`, the rounding control set
/// to truncate (`movb $0xc`), `fistp`, `fldcw`. The decomp shows every such cast as `ROUND()`,
/// but it truncates toward zero; NaN and anything out of range give the x87 "integer indefinite"
/// `0x80000000`. Takes the value as the x87 register held it: sums and products of `f32`s are
/// exact in `f64` here.
pub fn x87_trunc(v: f64) -> i32 {
    if (-2_147_483_648.0..2_147_483_648.0).contains(&v) { v as i32 } else { i32::MIN }
}

pub fn eye(v: &entvars_t) -> [f32; 3] {
    [v.origin[0] + v.view_ofs[0], v.origin[1] + v.view_ofs[1], v.origin[2] + v.view_ofs[2]]
}

/// A `float` distance, for the port's plugin slow aura (`powerup::plugin_speed_think`), which has
/// no TS arithmetic to follow. TS's own lengths go through `shared::pm::FpuMath` per build.
pub fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// `pev->frags` and `Player::frags` kept in step (the engine reports `pev->frags`).
pub fn add_frags(e: *mut edict_t, p: Option<&mut Player>, n: f32) {
    let v = e.v();
    v.frags += n;
    let f = v.frags as i32;
    match p {
        Some(p) => p.frags = f,
        None => {
            other(e, |p| p.frags = f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ts_flags_of, x87_trunc};
    use goldsrc::sys::*;
    use std::ffi::CStr;

    #[test]
    fn descriptions_carry_the_release_version() {
        let v = env!("CARGO_PKG_VERSION");
        let dm: &CStr = game_description!("DM");
        assert_eq!(dm.to_str().unwrap(), format!("The Specialists v{v} (DM)"));
        let single: &CStr = game_description!();
        assert_eq!(single.to_str().unwrap(), format!("The Specialists v{v}"));
        // Steam takes 64 bytes with the NUL (`k_cbMaxGameServerGameDescription`; ReHLDS
        // `sv_steam3.cpp` passes the description to `SetGameDescription`).
        let longest: &CStr = game_description!("The One-21");
        assert!(longest.to_bytes().len() < 64);
    }

    #[test]
    fn int_casts_truncate_like_gcc() {
        // `mp_timeleft` 0.6 s before a whole second shows the lower one, as TS's (int) does.
        assert_eq!(x87_trunc(29.6), 29);
        assert_eq!(x87_trunc(-29.6), -29);
        assert_eq!(x87_trunc(2.5), 2);
        assert_eq!(x87_trunc(f64::NAN), i32::MIN);
        assert_eq!(x87_trunc(3.0e9), i32::MIN);
        // `mp_timelimit 1` at time 30.4: 29 s left.
        let left = 1.0f32 as f64 * 60.0 - 30.4f32 as f64;
        assert_eq!(x87_trunc(left), 29);
    }

    #[test]
    fn ts_flags_keep_fl_client_on_a_player_who_left() {
        let (client, fake, dormant, monster) = (FL_CLIENT as i32, FL_FAKECLIENT as i32, FL_DORMANT as i32, FL_MONSTER as i32);
        // Connected: the flags as stored.
        assert_eq!(ts_flags_of(client | fake, true), client | fake);
        // Left: `ClientDisconnect` cleared FL_CLIENT/FL_FAKECLIENT, `TSPlayerDisconnect` set
        // FL_DORMANT; TS still has FL_CLIENT (its tests with 8, 0x28 or 0x2008 still match).
        let left = ts_flags_of(dormant, true);
        assert_eq!(left, client | dormant);
        assert_ne!(left & (client | monster), 0);
        assert_ne!(left & (client | fake), 0);
        assert_eq!(left & fake, 0);
        // Anything that isn't a player keeps its own flags.
        assert_eq!(ts_flags_of(monster, false), monster);
        assert_eq!(ts_flags_of(0, false) & client, 0);
    }
}
