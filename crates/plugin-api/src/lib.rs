//! The C ABI the server library offers server plugins: a port addition, TS has no such API.
//!
//! TS 3.0 kept player state in its C++ `CBasePlayer`, and AMX Mod X's TS module (`tsx`) read and
//! wrote it at fixed offsets. The port keeps that state in Rust objects (an edict's private data
//! is only a slab handle, see `goldsrc::edict`), so plugins reach it through this table instead.
//! Two kinds of plugin use it (docs/implementation/plugin-api.md, bot-plugins.md):
//! * **metamod plugins** such as the AMX Mod X modules in `the-specialists-mod/tsmod-amxx-rs`,
//!   which find the table through `CreateInterface`, and
//! * **`tsmod_*` libraries** that the server itself loads from its `dlls/` directory at
//!   `GameDLLInit` (bot backends such as `tsmod_rcbot`), which get the table from
//!   [`LOAD_SYMBOL`] and hand back a [`Plugin`] descriptor with their callbacks.
//!
//! # Getting the table (metamod plugins)
//! The server library exports tier1's `CreateInterface(name, return_code)` (`mp.dll` always had
//! it; the port adds it to `ts_i386.so`). Called with [`INTERFACE_NAME`] it returns a pointer to a
//! static [`PluginApi`] and writes 0 (`IFACE_OK`) to a non-NULL `return_code`; any other name
//! (older versions' names included) returns NULL and 1 (`IFACE_FAILED`). Look the export up in
//! the already loaded game library (metamod's `GINFO_REALDLL_FULLPATH`, then
//! `dlopen(RTLD_NOLOAD)` / `GetModuleHandle`). Such a plugin has no [`PluginId`] of its own: it
//! passes [`NO_PLUGIN`] where one is asked for, or takes one with [`PluginApi::attach_plugin`]
//! (version 7) and from then on is a plugin like a loaded one for its data directory, log prefix,
//! commands, listeners and bot backend, until [`PluginApi::detach_plugin`].
//!
//! # Loading (`tsmod_*` libraries)
//! At `GameDLLInit` the server loads every library in its own `dlls/` directory named
//! `tsmod_<name>` plus the server library's platform suffix ([`library_suffix`]) and calls its
//! exported [`LOAD_SYMBOL`] ([`LoadFn`]) once. The plugin checks the table's [`PluginApi::size`]
//! and [`PluginApi::version`], fills the [`Plugin`] it is given (its `size` and `api_version` are
//! checked after the call), and returns [`OK`]. Anything else (a library for another platform or
//! architecture, a missing export, a wrong size or version, a non-`OK` return) skips the library
//! with a warning. Libraries stay loaded for the life of the process.
//!
//! # Rules
//! * Call only on the engine's thread, from inside an engine or game callback (metamod hooks,
//!   AMX Mod X natives and forwards, and every [`Plugin`] callback qualify).
//! * Players are entity indices, 1 to `maxClients`. A slot without a connected player gives
//!   [`NOT_PLAYER`].
//! * A plugin can run while the server is inside one of that player's own callbacks (a message
//!   hook during its think, a death forward during its damage). The player object is then out of
//!   reach: [`PluginApi::player_info`] returns the latest snapshot (refreshed at least once a
//!   frame), and setters queue the change until the callback returns and give [`DEFERRED`]. The
//!   server calls the [`Plugin`] player callbacks only once the player is back in reach.
//! * There is no backward compatibility between versions: a plugin built for another
//!   [`VERSION`] doesn't find the table ([`INTERFACE_NAME`] carries the version) and isn't loaded
//!   ([`Plugin::api_version`]). Version 3 redesigned the table (docs/implementation/plugin-api.md
//!   §8 maps version 2 to it); version 4 added team scores, objectives, entity creation and more
//!   entity hooks (§9); version 5 added listeners for metamod plugins, the stunt, power-up,
//!   melee-attack and scored-kill callbacks, and the private-data mirror (§10, §4); version 6
//!   added what bot backends need to see and steer without guessing: [`PluginApi::run_cmd`]'s
//!   view handling, server-side `msec` and [`BotCmdResult`], the per-frame [`PlayerSnapshot`]s,
//!   [`ItemInfo`]s and [`FrameInfo`], the map keys the port doesn't keep
//!   ([`PluginApi::entity_key`]), and the touch, damage, shot and pickup callbacks (§11); version 7
//!   added plugin identities for metamod plugins ([`PluginApi::attach_plugin`],
//!   [`PluginApi::detach_plugin`], listeners they own), the kung-fu and superjump
//!   [`player_flag`]s and [`FrameInfo::team_scores`] (§12); version 8 added the `TSExt` channel
//!   messages to the port client's extensions ([`PluginApi::ext_message`]), the port client's
//!   version ([`PluginApi::client_port_version`]), and the client plugin offers
//!   ([`PluginApi::offer_client_plugin`], [`PluginApi::client_plugin_version`],
//!   [`Plugin::client_plugin_state`], [`Plugin::client_plugin_list`]) (§13).
//!
//! # Private data
//! An edict's private data is the server's block, not a C++ object. Since version 5 the server
//! mirrors a few TS 3.0 fields into it at TS's offsets (the `pev` pointer, a player's
//! `m_pActiveItem`, the `weapon_tsgun`'s ammo and weapon records), so fakemeta's `get_pdata_*` /
//! `set_pdata_*` and hamsandwich's `get_pdata_cbase` read them; some writes are taken back
//! (docs/implementation/plugin-api.md §4 lists them). [`PluginApi::private_size`] gives the
//! block's size.
//!
//! # Entity hooks
//! AMX Mod X's hamsandwich hooks C++ virtual functions by patching vtables. The port's entities
//! are Rust objects without vtables, so the server calls a plugin hook around each "virtual" it
//! has instead, and runs a virtual on request ([`PluginApi::ham_execute`]).
//! * Functions are numbered by hamsandwich's `Ham` enum (`ham_const.inc`, AMX Mod X 1.10:
//!   `Ham_Spawn` = 0, ...). Their arguments are hamsandwich's for that function
//!   (`hook_native.cpp`'s signature, e.g. `takedamage` = `Int_Entvar_Entvar_Float_Int`), in order,
//!   in [`HamCall::args`].
//! * Hooks are per class, like a vtable: [`PluginApi::class_id`] gives the class of a classname,
//!   and every call carries the class of `this`. Classnames sharing one Rust type share a class.
//! * The server calls the hook only for functions turned on with [`PluginApi::ham_watch`].
//! * The hook runs before the function (`post` = 0) and after it (`post` = 1). `this` is in
//!   reach during both (not taken out), so plugins may call the API and `ham_execute` on it.
//!   Before: argument changes in the [`HamCall`] are used, and [`ham::action::SUPERCEDE`] skips
//!   the function.
//! * Every call before is followed by exactly one call after, also when the function was
//!   superseded (or `this` was removed meanwhile: the API then answers [`NOT_PLAYER`] for it).
//!   After: [`HamCall::orig_ret`] is what the function returned, the zero value of its return
//!   kind if it didn't run. **The post call's action decides the return value**:
//!   [`ham::action::OVERRIDE`] returns [`HamCall::ret`], anything else returns `orig_ret`. A
//!   pre call's `OVERRIDE` or `SUPERCEDE` value in `ret` is still there for the post call, which
//!   must return `OVERRIDE` itself to keep it.
//! * Hooks may nest: a hook may call `ham_execute`, deal damage or remove entities, and the
//!   functions that run call the hook again. Hooks nested 8 deep are not called (the function
//!   runs without them), and `ham_execute` at that depth gives [`BUSY`].
//! * `Ham_Keyvalue`'s argument (hamsandwich's int) is kind [`ham::kind::INT`] with `p` the
//!   `KeyValueData *` (valid for the call, strings read-only; `i` is the pointer's low 32 bits).
//!   A pre hook that supersedes it decides `fHandled`.
//! * One hook per server ([`PluginApi::ham_set_hook`]): the AMX Mod X module dispatches to its
//!   plugins.
//! * TS's player weapons are one `weapon_tsgun` per player holding up to six guns. The `Item_*` and
//!   `Weapon_*` functions fire on that entity's class with `this` the `weapon_tsgun` (its
//!   `pev->owner` the player); the gun in hand is the context ([`PluginApi::entity_info`]'s
//!   `weapon`, [`PluginApi::player_info`]).
//! * Query functions (`ObjectCaps`, `IsPlayer`, `Center`, ...) fire where the game asks them. When
//!   the game asks about an entity inside its own call (an entity's think asking its own time
//!   scale), the entity is out of reach for the hook, as for a player inside its own callback.
//! * Port deviation: `Ham_TS_GiveSlowMul` returns its value, kind [`ham::kind::FLOAT`]
//!   (hamsandwich declares it `Void_Void`).
//!
//! # Bots
//! A plugin offers bots by registering a [`BotBackend`] under a name
//! ([`PluginApi::register_bot_backend`]). The server cvar `ts_bot_backend` (`native`, TS's own
//! bots, or a registered name) picks the backend when a map starts. The server owns the bot quota
//! (`ts_bot_quota`) and `addbot`/`addcustombot`, and asks the active backend to add a bot
//! ([`BotBackend::add_bot`]); the backend makes the player with [`PluginApi::create_bot`] and
//! drives it every frame with [`PluginApi::run_cmd`] (from [`Plugin::start_frame`]), reading the
//! world from [`PluginApi::players`], [`PluginApi::items`] and [`PluginApi::frame_info`], which
//! the server builds once a frame before the plugins' `start_frame`.

#![no_std]

use core::ffi::{CStr, c_char, c_int, c_void};

/// The `CreateInterface` name of the table. It carries the version, so a metamod plugin built for
/// another version gets NULL.
pub const INTERFACE_NAME: &CStr = c"TSRS_PluginApi008";
/// [`PluginApi::version`] of this definition (and what [`Plugin::api_version`] must be).
pub const VERSION: u32 = 8;
/// The entry point every `tsmod_*` library exports ([`LoadFn`]).
pub const LOAD_SYMBOL: &CStr = c"TSRS_PluginLoad";
/// The prefix of the libraries the server loads.
pub const LIBRARY_PREFIX: &str = "tsmod_";

/// `TSRS_PluginLoad(api, api_size, info, out, out_size)`: called once, at `GameDLLInit`. `api`
/// is the table (`api_size` = its [`PluginApi::size`]); `info` says who the plugin is (valid for
/// the call; keep [`LoadInfo::id`]); `out` is a zeroed [`Plugin`] of `out_size` bytes whose
/// `size` the server has set. Fill it and return [`OK`]; any other value refuses to load.
pub type LoadFn =
    unsafe extern "C" fn(api: *const PluginApi, api_size: u32, info: *const LoadInfo, out: *mut Plugin, out_size: u32) -> c_int;

/// The file name suffix of a `tsmod_*` library for a platform and architecture: the server
/// library's own naming (`mp.dll`, `mp_amd64.dll`, `ts_i386.so`, `ts_amd64.so`). `None` for a
/// platform the port doesn't build.
pub const fn library_suffix(windows: bool, x86_64: bool) -> &'static str {
    match (windows, x86_64) {
        (true, false) => ".dll",
        (true, true) => "_amd64.dll",
        (false, false) => "_i386.so",
        (false, true) => "_amd64.so",
    }
}

/// [`library_suffix`] of the platform this crate is built for.
pub const LIBRARY_SUFFIX: &str = library_suffix(cfg!(windows), cfg!(target_arch = "x86_64"));

/// Done.
pub const OK: c_int = 0;
/// A setter's player was inside its own callback; the change applies when that returns.
pub const DEFERRED: c_int = 1;
/// Not an entity index of a connected player (or, for the entity functions, not a game entity).
pub const NOT_PLAYER: c_int = -1;
/// An argument is out of range; nothing changed.
pub const INVALID: c_int = -2;
/// TS 3.0 has no such state; nothing changed.
pub const UNSUPPORTED: c_int = -3;
/// The player or entity is inside one of its own callbacks and out of reach for something that
/// must run now ([`PluginApi::run_cmd`], [`PluginApi::ham_execute`]), or hooks are nested too deep;
/// nothing ran.
pub const BUSY: c_int = -4;
/// [`PluginApi::create_bot`]: the server has no free client slot.
pub const FULL: c_int = -5;
/// A name is already taken (a command, cvar or bot backend); nothing changed.
pub const EXISTS: c_int = -6;

/// A plugin, as [`LoadInfo::id`] or [`PluginApi::attach_plugin`] gives it: passed back to the
/// functions that act for the plugin (its data directory, log prefix, commands, listeners, bot
/// backend). Ids are never handed out twice in a process.
pub type PluginId = c_int;
/// The [`PluginId`] of a plugin without an identity (a metamod plugin using `CreateInterface` that
/// didn't [`attach`](PluginApi::attach_plugin)).
pub const NO_PLUGIN: PluginId = 0;

/// Who the plugin is, given to [`LoadFn`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoadInfo {
    /// This plugin's id (> 0).
    pub id: PluginId,
    /// `<name>` of `tsmod_<name>`, NUL-terminated.
    pub name: *const c_char,
    /// The full path of the library, NUL-terminated.
    pub path: *const c_char,
}

/// A plugin's description and callbacks, filled by [`LoadFn`]. Every callback is optional and
/// runs on the engine's thread; player indices follow the module's rules.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Plugin {
    /// `size_of::<Plugin>()`: the server sets it before the call and checks it after.
    pub size: u32,
    /// The [`VERSION`] the plugin was built for: must be the server's.
    pub api_version: u32,
    /// Display name and version (NUL-terminated, static): for the log.
    pub name: *const c_char,
    pub version: *const c_char,

    /// `GameDLLInit`, after every plugin has loaded (cvars and commands may be registered here or
    /// in [`LoadFn`]).
    pub game_init: Option<extern "C" fn()>,
    /// The map's precache (`CWorld::Precache`): precache models and sounds here.
    pub precache: Option<extern "C" fn()>,
    /// `ServerActivate`: the map's entities exist. `map` is the map name.
    pub server_activate: Option<unsafe extern "C" fn(map: *const c_char)>,
    /// `ServerDeactivate`.
    pub server_deactivate: Option<extern "C" fn()>,
    /// `StartFrame`, after the server's own bots and before the rules think: bot backends run
    /// their bots ([`PluginApi::run_cmd`]) here.
    pub start_frame: Option<extern "C" fn()>,
    /// `GameShutdown` (the engines that call it).
    pub shutdown: Option<extern "C" fn()>,
    /// A client connected (`ClientConnect`, accepted). Bots made by any backend aren't
    /// "connected"; they get [`Self::client_put_in_server`] only.
    pub client_connect: Option<extern "C" fn(player: c_int, is_bot: c_int)>,
    /// A client (or bot) entered the game and spawned for the first time.
    pub client_put_in_server: Option<extern "C" fn(player: c_int, is_bot: c_int)>,
    /// A client (or bot) is leaving; still a player during the call.
    pub client_disconnect: Option<extern "C" fn(player: c_int, is_bot: c_int)>,
    /// A client command (`argv[0]` the command). Return non-zero to swallow it: the game then
    /// doesn't run it. Called before the game's commands, plugin by plugin in load order.
    pub client_command: Option<unsafe extern "C" fn(player: c_int, argc: c_int, argv: *const *const c_char) -> c_int>,
    /// The player spawned (a first spawn or a respawn).
    pub player_spawned: Option<extern "C" fn(player: c_int)>,
    /// Version 6 (replaces version 5's `player_damaged`). A player's `TakeDamage` ran: the
    /// [`DamageEvent`] (valid for the call) says who, with what, where and what is left. Deferred
    /// until the victim and the attacker are back in their slots.
    pub damage: Option<unsafe extern "C" fn(event: *const DamageEvent)>,
    /// The player died: `attacker` / `inflictor` as above.
    pub player_killed: Option<extern "C" fn(victim: c_int, attacker: c_int, inflictor: c_int)>,
    /// `killer` (a player) scored a kill of `victim`: `points` the frags it gave (-1 a team
    /// kill, 0 none).
    pub on_kill: Option<extern "C" fn(killer: c_int, victim: c_int, points: c_int)>,
    /// A round started (last man standing) / ended (its winner known, the restart pending).
    pub round_start: Option<extern "C" fn()>,
    pub round_end: Option<extern "C" fn()>,
    /// The game played a sound at `entity` (an index; 0 the world) and `origin` (`pfnEmitSound`,
    /// `pfnEmitAmbientSound`): for bots' hearing. `volume` 0..1, `attenuation` 0..4. Version 6
    /// adds `channel` (`CHAN_*`: 0 auto .. 7; ambient sounds are `CHAN_STATIC`, 6) and
    /// `source_player`, the player responsible: `entity` itself when it is a player, else the
    /// thrower or holder [`PluginApi::projectile_owner`] names, 0 for none.
    pub sound: Option<
        unsafe extern "C" fn(
            entity: c_int,
            origin: *const f32,
            sample: *const c_char,
            volume: f32,
            attenuation: f32,
            channel: c_int,
            source_player: c_int,
        ),
    >,
    /// The player's weapon in hand changed (`weapon` its TS id, 0 kung fu), or its clip or
    /// reserve did: once a frame at most, from [`PluginApi::player_info`]'s fields.
    pub weapon_changed: Option<extern "C" fn(player: c_int, weapon: c_int)>,
    pub ammo_changed: Option<extern "C" fn(player: c_int, weapon: c_int, clip: c_int, ammo: c_int)>,
    /// Version 5. The player started a stunt (a [`stunt`] kind), once the game has begun it.
    pub player_stunt: Option<extern "C" fn(player: c_int, stunt: c_int)>,
    /// Version 5. A power-up took effect ([`powerup`] bits): the held one was used
    /// (`usepowerup`), or one that works at once was picked up (health, armour, kung fu,
    /// superjump, grenade, and infinite clip where the rules apply it at once). Not for
    /// [`PluginApi::run_powerup`].
    pub player_powerup: Option<extern "C" fn(player: c_int, powerup: c_int)>,
    /// Version 5. A kung-fu move is about to land (`CTSGun::CCombatAttack`): `time` is the
    /// seconds until the next attack, `damage` what a hit deals (kung-fu power and the Kung Fu
    /// power-up included). Change them through the pointers (valid for the call; a value that
    /// isn't finite or is below 0 is ignored), and return non-zero to block the move (no hit,
    /// animation or event; `time` still applies). Called before the move with the player lent to
    /// its slot, so the API reaches it. Callbacks run in order; the first to block stops it. In
    /// TS 2.1 mode `time` is the move's delay before the player's time scale stretches it.
    pub melee_attack: Option<unsafe extern "C" fn(player: c_int, time: *mut f32, damage: *mut f32) -> c_int>,
    /// Version 5. The rules scored a player's death (`CHalfLifeMultiplay::PlayerKilled`): a kill,
    /// a suicide or a death by the world, after the frags changed and the `DeathMsg` went out.
    /// `kill` is valid for the call. Deferred like [`on_kill`](Self::on_kill), so it comes after
    /// the victim's `TSHealth` 0; deaths the rules don't score (a teamplay team change) have none.
    /// In TS 2.1 mode it is 2.1's `PlayerKilled` with 2.1's points, sprees and tags (it never
    /// sets [`kill::VICTIMS_WEAPON`] or [`kill::HEADSHOT`], which 2.1 lacks).
    pub kill_scored: Option<unsafe extern "C" fn(kill: *const KillInfo)>,
    /// Version 6. A bot (`FL_FAKECLIENT`, any backend) touched `other` or was touched by it
    /// (`DispatchTouch` either way round, after the entity's `Touch` ran): a trigger, a pickup,
    /// a door, another player. Solid contacts can come twice in a frame (both entities' touch);
    /// triggers once. Deferred until the bot is back in its slot.
    pub entity_touch: Option<extern "C" fn(player: c_int, other: c_int)>,
    /// Version 6. `player` fired a shot of the TS weapon `weapon` (each `ShootBullets`: every
    /// round of a burst, one call for a shotgun's pellets): `origin` the gun position, `dir` the
    /// unit aim (`v_angle`'s forward, before spread), `silenced` non-zero with a silencer on.
    /// Both pointers are three floats valid for the call. Melee, throws and grenades don't fire
    /// it. Deferred until the player is back in its slot.
    pub weapon_fired: Option<unsafe extern "C" fn(player: c_int, weapon: c_int, origin: *const f32, dir: *const f32, silenced: c_int)>,
    /// Version 6. `player` picked up `entity`: `kind` [`entity::GROUND_WEAPON`] (by `use`, its
    /// inventory changed; `id` the TS weapon id), [`entity::POWERUP`] (`id` its [`powerup`] bits)
    /// or [`entity::GUN`] (a dropped gun; `id` its TS id). A temporary power-up or dropped gun is
    /// removed by then, so `entity` may be free. Deferred until the player is back in its slot.
    pub item_pickup: Option<extern "C" fn(player: c_int, entity: c_int, kind: c_int, id: c_int)>,

    // ---- version 8: client plugins ------------------------------------------------------------------
    /// A `tspkg <id> <version> <state> [reason]` answer the server took from a client
    /// (docs/implementation/client-plugins.md §6.5): to an offer (the API's
    /// [`PluginApi::offer_client_plugin`], `sv_plugin_offer` or the offers at `ClientPutInServer`),
    /// to a `block` (`suspended`) or to op 5 (`id` "tsrs"). `state` is a [`plugin_state`] value;
    /// `reason` is "" when the client gave none. The strings live for the call. Answers the server
    /// ignores (ids it didn't send, over the rate limit) aren't reported. Runs at once, from the
    /// client's command (after [`Plugin::client_command`], which sees `tspkg` first).
    pub client_plugin_state:
        Option<unsafe extern "C" fn(player: c_int, id: *const c_char, version: *const c_char, state: c_int, reason: *const c_char)>,
    /// A client's `tspkg list` is complete (its closing line came, client-plugins.md §13.2): the
    /// `count` plugins it has loaded, `plugins[..count]` (a plugin id or `manual:<name>`, and the
    /// version), valid for the call. Once per connection (so again at each map: the map change's
    /// reconnect starts the list again); a client with no plugins gives `count` 0. A list still
    /// incomplete when the grace period ends isn't reported ([`PluginApi::client_plugin_version`]
    /// answers from what came).
    pub client_plugin_list: Option<unsafe extern "C" fn(player: c_int, plugins: *const ClientPlugin, count: c_int)>,
}

/// One loaded plugin of [`Plugin::client_plugin_list`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ClientPlugin {
    /// A plugin id (`[a-z0-9_-]{1,32}`) or `manual:<name>` (a hand-installed library).
    pub id: *const c_char,
    /// Its version as the client reported it (`0.0.0` for a hand-installed library).
    pub version: *const c_char,
}

/// [`Plugin::client_plugin_state`]'s `state`: the `tspkg` states (client-plugins.md §6.5, §13.3),
/// in that order.
pub mod plugin_state {
    use core::ffi::c_int;
    /// Installed and approved already (the same manifest).
    pub const HAVE: c_int = 0;
    /// The card is shown.
    pub const OFFERED: c_int = 1;
    pub const DOWNLOADING: c_int = 2;
    /// Downloaded and approved, not loaded yet.
    pub const INSTALLED: c_int = 3;
    pub const LOADED: c_int = 4;
    /// The player said no ("Not now" or "Never"; the server isn't told which).
    pub const DENIED: c_int = 5;
    /// The player's client blocks it (the plugin or its publisher).
    pub const BLOCKED: c_int = 6;
    /// This client can't run it (another platform, a newer API).
    pub const UNSUPPORTED: c_int = 7;
    /// The download or a check failed.
    pub const FAILED: c_int = 8;
    /// Suspended on this server, as a `block` asked.
    pub const SUSPENDED: c_int = 9;
    /// It needs a restart (an update of a loaded plugin, or op 5's port update).
    pub const RESTART: c_int = 10;
}

/// A player's damage as [`Plugin::damage`] reports it (version 6).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DamageEvent {
    /// The damaged player, and the entities that did it: indices, 0 the world, -1 none.
    pub victim: c_int,
    pub attacker: c_int,
    pub inflictor: c_int,
    /// What `TakeDamage` was asked for (a shotgun shot's pellets summed), and its `DMG_*` bits.
    pub damage: f32,
    pub bits: c_int,
    /// The victim's health and armour after it (health ≤ 0: it died).
    pub health: f32,
    pub armor: f32,
    /// The hitgroup of the `TraceAttack` that led to it (1 head, 2 chest, 3 stomach, 4/5 arms,
    /// 6/7 legs), 0 (generic) for damage that didn't come through one (falls, explosions without
    /// a trace, triggers).
    pub hitgroup: c_int,
    /// The TS weapon id of the blow, as [`KillInfo::weapon`] counts it: by the attacker's own
    /// hand the bullet's weapon, kung fu (0) in close combat, else the gun in hand; a thrown
    /// blade's id; 24 a grenade; -1 anything else.
    pub weapon: c_int,
    /// That `TraceAttack`'s direction of travel (unit) and hit position, zero without one.
    pub dir: [f32; 3],
    pub pos: [f32; 3],
}

/// Bytes of [`KillInfo::weapon_name`], its NUL included.
pub const WEAPON_NAME_LEN: usize = 32;

/// A death as `PlayerKilled` scored it ([`Plugin::kill_scored`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KillInfo {
    /// The killing player; the victim for a suicide; 0 when no player killed (the world, a
    /// trigger, a monster).
    pub killer: c_int,
    pub victim: c_int,
    /// The inflictor's index, -1 for none.
    pub inflictor: c_int,
    /// The frags the killing player got (`PlayerKilled`'s points: double kill, specialist and
    /// spree included): -1 for a team kill, 0 for none (The One mode). 0 for a suicide or a death
    /// by no player.
    pub points: c_int,
    /// The TS weapon id of the blow: by the killer's own hand the simulated bullet's weapon, else
    /// 0 (kung fu) under `glast_closecombat`, else the gun in hand (0 for kung fu); a thrown
    /// blade's id; 24 for a grenade; -1 for anything else.
    pub weapon: c_int,
    /// [`kill`] bits.
    pub tags: c_int,
    /// The killer's kills since its last death, this one included (0 without a killing player);
    /// the victim's before it died.
    pub killer_spree: c_int,
    pub victim_spree: c_int,
    /// The `DeathMsg` weapon (`PlayerKilled`'s name: "Kung Fu", "world", "Glock-18 Stock",
    /// "Throwing Knife", ...), NUL-terminated, cut to fit.
    pub weapon_name: [c_char; WEAPON_NAME_LEN],
}

impl Default for KillInfo {
    fn default() -> Self {
        KillInfo {
            killer: 0,
            victim: 0,
            inflictor: -1,
            points: 0,
            weapon: -1,
            tags: 0,
            killer_spree: 0,
            victim_spree: 0,
            weapon_name: [0; WEAPON_NAME_LEN],
        }
    }
}

/// [`KillInfo::tags`]: `PlayerKilled`'s log tags (0x89f0d..0x89fa4) and how the kill scored.
pub mod kill {
    use core::ffi::c_int;
    /// ` (victims_weapon)`: killed with a gun the victim owned first.
    pub const VICTIMS_WEAPON: c_int = 0x1;
    /// ` (stopped_fury)`: the victim was on a spree of three or more.
    pub const STOPPED_FURY: c_int = 0x2;
    /// ` (killed_specialist)`: the victim's spree was over 8.
    pub const KILLED_SPECIALIST: c_int = 0x4;
    /// ` (double_kill)`: under 2 s after the killer's last scoring kill.
    pub const DOUBLE_KILL: c_int = 0x8;
    /// ` (diving)` / ` (sliding)`: `TSGetPointsForFrag`'s stunt test.
    pub const DIVING: c_int = 0x10;
    pub const SLIDING: c_int = 0x20;
    /// ` (killed_the_one)`.
    pub const KILLED_THE_ONE: c_int = 0x40;
    /// The spree award this kill reached: ` (is_killer)` (3), ` (is_demolition)` (6),
    /// ` (is_specialist)` (9), ` (is_unstoppable)` (10 on).
    pub const IS_KILLER: c_int = 0x80;
    pub const IS_DEMOLITION: c_int = 0x100;
    pub const IS_SPECIALIST: c_int = 0x200;
    pub const IS_UNSTOPPABLE: c_int = 0x400;
    /// `TSGetPointsForFrag` scored a kung-fu frag.
    pub const KUNG_FU: c_int = 0x800;
    /// TS's damage globals for the kill: `glast_headshot`, `glast_closecombat`, `glast_coldcock`.
    pub const HEADSHOT: c_int = 0x1000;
    pub const CLOSE_COMBAT: c_int = 0x2000;
    pub const COLDCOCK: c_int = 0x4000;
}

impl Plugin {
    /// A descriptor with no name and no callbacks, `size` and `api_version` set.
    pub const EMPTY: Plugin = Plugin {
        size: size_of::<Plugin>() as u32,
        api_version: VERSION,
        name: core::ptr::null(),
        version: core::ptr::null(),
        game_init: None,
        precache: None,
        server_activate: None,
        server_deactivate: None,
        start_frame: None,
        shutdown: None,
        client_connect: None,
        client_put_in_server: None,
        client_disconnect: None,
        client_command: None,
        player_spawned: None,
        damage: None,
        player_killed: None,
        on_kill: None,
        round_start: None,
        round_end: None,
        sound: None,
        weapon_changed: None,
        ammo_changed: None,
        player_stunt: None,
        player_powerup: None,
        melee_attack: None,
        kill_scored: None,
        entity_touch: None,
        weapon_fired: None,
        item_pickup: None,
        client_plugin_state: None,
        client_plugin_list: None,
    };
}

/// A bot backend, registered with [`PluginApi::register_bot_backend`] (the server copies it).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BotBackend {
    /// `size_of::<BotBackend>()`.
    pub size: u32,
    /// Add one bot: `addbot` / `addcustombot` / the quota. `name` and `model` may be empty (the
    /// backend picks; in teamplay the model is the team); `skill` ≤ 0 means the backend picks.
    /// They are the command's argument bytes as typed, not UTF-8: give them to
    /// [`PluginApi::create_bot`] unconverted.
    /// Make the player with [`PluginApi::create_bot`] and return its index, or ≤ 0 for none.
    pub add_bot: Option<unsafe extern "C" fn(name: *const c_char, model: *const c_char, skill: f32) -> c_int>,
    /// The backend became the active one (a map start selected it).
    pub activate: Option<extern "C" fn()>,
    /// It no longer is (the next map selected another; its bots are being kicked).
    pub deactivate: Option<extern "C" fn()>,
    /// A quota bot of this backend is still connected but its player object is new (the engine
    /// kept the client across a level change): pick it up again. Called once.
    pub adopt_bot: Option<extern "C" fn(player: c_int)>,
}

/// Entity hook types: see the module docs.
pub mod ham {
    use core::ffi::c_int;

    /// [`HamArg::kind`](super::HamArg::kind).
    pub mod kind {
        use core::ffi::c_int;
        /// No value (a void return).
        pub const VOID: c_int = 0;
        /// `i`: int, bool, short.
        pub const INT: c_int = 1;
        /// `f`.
        pub const FLOAT: c_int = 2;
        /// `i`: an entity index (hamsandwich's Entvar, Cbase and Edict); -1 for none.
        pub const ENTITY: c_int = 3;
        /// `v`.
        pub const VECTOR: c_int = 4;
        /// `p`: a `TraceResult *` valid for the call; changes before the function are used.
        pub const TRACE: c_int = 5;
        /// `p`: a NUL-terminated string valid for the call (read-only).
        pub const STRING: c_int = 6;
        /// `p`: an `ItemInfo *` valid for the call.
        pub const ITEMINFO: c_int = 7;
        /// `v` / `f` passed by pointer (hamsandwich's pVector, pFloat): the function's writes
        /// come back in them.
        pub const VECTOR_OUT: c_int = 8;
        pub const FLOAT_OUT: c_int = 9;
    }

    /// What a hook returns.
    pub mod action {
        use core::ffi::c_int;
        /// Carry on; the function's own result stands.
        pub const CONTINUE: c_int = 0;
        /// Before the function only: skip it. The call after still follows, with a zero
        /// `orig_ret`; its action decides the return value.
        pub const SUPERCEDE: c_int = 1;
        /// After the function: return [`HamCall::ret`](super::super::HamCall::ret) instead of
        /// the function's result. Before it, only the `ret` it leaves counts (for the call after).
        pub const OVERRIDE: c_int = 2;
    }

    /// Most arguments any hamsandwich function takes.
    pub const MAX_ARGS: usize = 8;

    /// The plugin's hook: `post` is 0 before the function, 1 after. Returns an [`action`].
    pub type Hook = unsafe extern "C" fn(call: *mut super::HamCall, post: c_int) -> c_int;
}

/// One argument or return value of a hooked function. Only the field its `kind` names is used.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HamArg {
    pub kind: c_int,
    pub i: c_int,
    pub f: f32,
    pub v: [f32; 3],
    pub p: *const c_void,
}

impl Default for HamArg {
    fn default() -> Self {
        HamArg { kind: ham::kind::VOID, i: 0, f: 0.0, v: [0.0; 3], p: core::ptr::null() }
    }
}

/// A call of an entity function, as hooks see it and [`PluginApi::ham_execute`] takes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HamCall {
    /// hamsandwich's `Ham` number.
    pub ham: c_int,
    /// The entity the function runs on, and its [`PluginApi::class_id`].
    pub this: c_int,
    pub class: c_int,
    /// The arguments, `args[..argc]`.
    pub argc: c_int,
    pub args: [HamArg; ham::MAX_ARGS],
    /// The return value to use ([`ham::action`]); its kind is the function's return type.
    pub ret: HamArg,
    /// After the function: what it returned.
    pub orig_ret: HamArg,
}

/// Power-up bits (`PwUp` message, `tsconst.inc`'s `TSPWUP_*`).
pub mod powerup {
    pub const SLOWMO: i32 = 1;
    pub const INFAMMO: i32 = 2;
    pub const KUNGFU: i32 = 4;
    pub const SLOWPAUSE: i32 = 8;
    pub const DFIRERATE: i32 = 16;
    pub const GRENADE: i32 = 32;
    pub const HEALTH: i32 = 64;
    pub const ARMOR: i32 = 128;
    pub const SUPERJUMP: i32 = 256;
}

/// [`Plugin::player_stunt`] kinds. 2 to 5 are `tsconst.inc`'s `STUNT_*` values (TS 2.x's
/// `TSState`, which TS 3.0 cut to stand / crouch / prone); 6 and 7 number TS 3.0 stunts 2.x
/// lacked.
pub mod stunt {
    use core::ffi::c_int;
    /// A roll: `CTSStunt::GoRoll` (0x9cfe8), rolling out of a dive's landing (`Landing`
    /// 0x9bbf4) or sideways out of a slide (`HandleSlide` 0x9b9f8).
    pub const ROLL: c_int = 2;
    /// A dive (`GoDive` 0x9b1a0, wall dives included) or the slow-motion dodge (`GoBulletTime`
    /// 0x9a5b4).
    pub const DIVE: c_int = 3;
    /// Getting up from lying after a dive (`HandleSlide`).
    pub const GETUP: c_int = 4;
    /// A flip or cartwheel (`GoWheel` 0x9c9bc).
    pub const FLIP: c_int = 5;
    /// Kicking off a wall (`GoWallJump` 0x9a7d4).
    pub const WALL_JUMP: c_int = 6;
    /// Running up a wall (`GoWallClimb` 0x9aa1c).
    pub const WALL_CLIMB: c_int = 7;
}

/// Loadout items for [`PluginApi::buy_item`] (`tki`, `CBasePlayer::AddItem`).
pub mod item {
    pub const KEVLAR: i32 = 0x10;
    pub const STEALTH_SHOES: i32 = 0x20;
}

/// [`PluginApi::log`] levels.
pub mod log {
    use core::ffi::c_int;
    pub const INFO: c_int = 0;
    pub const WARNING: c_int = 1;
    pub const ERROR: c_int = 2;
    /// Printed only with `developer` on.
    pub const DEBUG: c_int = 3;
}

/// A player's TS state, as [`PluginApi::player_info`] reports it.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct PlayerInfo {
    /// The player has joined the game (past the team/MOTD menus) and isn't spectating.
    pub joined: c_int,
    /// Alive and not observing.
    pub alive: c_int,
    /// A bot (of any backend: TS's `m_bIsBot`).
    pub is_bot: c_int,
    /// The rules team: 1.. in teamplay (the order of `mp_teamlist` / the map's teams; the CTB
    /// and team-win entities' `team` keys count the same way, from 1), 0 for none or outside
    /// teamplay.
    pub team: c_int,
    /// Carries the capture-the-briefcase case (a carried case has no entity of its own).
    pub has_flag: c_int,
    /// Money (`TSCash`).
    pub cash: c_int,
    /// Free inventory slots (`TSSpace`).
    pub free_slots: c_int,
    /// The award/status byte TS broadcasts with `TSPAward` (`pl+0x74c`): 1 out until the next
    /// round, 2 Killer, 3 Demolition, 4 Specialist, 10 The One, 11 respawn pending, 0 none.
    pub award: c_int,
    /// Kills since the last death (`pl+0x744`).
    pub kill_streak: c_int,
    /// The power-up running now ([`powerup`] bits), 0 for none.
    pub active_powerup: c_int,
    /// Server time (`gpGlobals->time`) at which `active_powerup` ends, 0 for none.
    pub powerup_until: f32,
    /// The power-up held but not yet used, and its value (the `PwUp` message's two fields).
    pub stored_powerup: c_int,
    pub stored_powerup_value: c_int,
    /// Permanent kung fu and superjump picked up this life.
    pub kungfu: c_int,
    pub superjump: c_int,
    /// The player's time scale (`pev->fuser1`): 1 normal, below 1 slowed.
    pub time_scale: f32,
    /// The stunt state TS sends with `TSState`.
    pub stunt_state: c_int,
    /// The active weapon's TS id (`WeaponInfo`; 0 for kung fu) and its clip, reserve ammo,
    /// fire mode and attachment bits (`tsconst.inc`'s `TSA_*`).
    pub weapon: c_int,
    pub clip: c_int,
    pub ammo: c_int,
    pub fire_mode: c_int,
    pub attachments: c_int,
}

/// One weapon a player owns, as [`PluginApi::player_inventory`] lists it.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct WeaponEntry {
    /// TS weapon id (1..37).
    pub weapon: c_int,
    /// Rounds in the clip (a stack's count for grenades and blades).
    pub clip: c_int,
    /// Reserve of the weapon's ammo type (shared by every weapon of that type), 0 if it has none.
    pub ammo: c_int,
    /// Fire mode and owned attachment bits (`TSA_*`).
    pub fire_mode: c_int,
    pub attachments: c_int,
}

/// What a game entity is, for [`EntityInfo::kind`].
pub mod entity {
    use core::ffi::c_int;
    /// Anything else (check its classname).
    pub const OTHER: c_int = 0;
    /// A player.
    pub const PLAYER: c_int = 1;
    /// `ts_groundweapon` or an HL weapon name it stands for: `weapon` is its TS id.
    pub const GROUND_WEAPON: c_int = 2;
    /// `ts_powerup`: `powerup` is its bits.
    pub const POWERUP: c_int = 3;
    /// A thrown grenade, knife or gun: `owner` is the thrower.
    pub const PROJECTILE: c_int = 4;
    /// A gun a player holds or dropped (`weapon_tsgun`, the dropped pack): `owner` its player,
    /// `weapon` its TS id (a held `weapon_tsgun`: the weapon in hand).
    pub const GUN: c_int = 5;
    /// The capture-the-briefcase case lying in the map (`ts_flag`): `team` the team it belongs to.
    /// A carried case has no entity: see [`PlayerInfo::has_flag`](super::PlayerInfo::has_flag).
    pub const CTB_FLAG: c_int = 6;
    /// A capture point (`ts_briefcase`): `team` the team that may capture there (0 any).
    pub const CTB_BASE: c_int = 7;
    /// A team or score objective (`ts_wingiver`, `ts_scoregiver`).
    pub const OBJECTIVE: c_int = 8;
}

/// TS's teams: `CTSGameRules` keeps four ([`GameState::team_scores`]).
pub const MAX_TEAMS: usize = 4;

/// Bytes of [`ObjectiveInfo::name`], its NUL included: TS cuts the `Objective` message's text to
/// 63 bytes.
pub const OBJECTIVE_NAME_LEN: usize = 64;

/// An objective of the HUD list, as [`PluginApi::objectives`] reports it: what TS's `Objective`
/// message carries (`SendObjectiveToHud`), plus the entity and whether it is active.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectiveInfo {
    /// The list index the `Objective` message carries (its `idx` byte).
    pub id: c_int,
    /// The objective's entity (`ts_bomb`, `ts_hack`, `ts_teamescape`, ...).
    pub entity: c_int,
    /// The message's position: the entity's centre (`Center()`).
    pub origin: [f32; 3],
    /// The objective is enabled (not frozen): it can be completed now.
    pub active: c_int,
    /// The message's text for the asking player's team, NUL-terminated (empty for none).
    pub name: [c_char; OBJECTIVE_NAME_LEN],
}

impl Default for ObjectiveInfo {
    fn default() -> Self {
        ObjectiveInfo { id: 0, entity: 0, origin: [0.0; 3], active: 0, name: [0; OBJECTIVE_NAME_LEN] }
    }
}

/// A game entity as [`PluginApi::entity_info`] reports it.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct EntityInfo {
    /// [`entity`] kind.
    pub kind: c_int,
    /// TS weapon id: of a [`entity::GROUND_WEAPON`] or a dropped [`entity::GUN`], and for a held
    /// `weapon_tsgun` the holder's weapon in hand (0 kung fu); else 0.
    pub weapon: c_int,
    /// [`powerup`] bits ([`entity::POWERUP`]), else 0.
    pub powerup: c_int,
    /// The responsible player (thrower, holder, carrier), 0 for none.
    pub owner: c_int,
    /// Team as in [`PlayerInfo::team`], 0 for none or any.
    pub team: c_int,
    /// It can be picked up or used now (visible and not waiting to respawn).
    pub available: c_int,
}

/// `m_mode` values of [`GameState::mode`] (`CTSGameRules::m_mode`).
pub mod mode {
    use core::ffi::c_int;
    pub const DEATHMATCH: c_int = 0;
    pub const TEAM_DEATHMATCH: c_int = 1;
    /// Unreachable in TS 3.0, listed for completeness.
    pub const ROUND_TEAM_DEATHMATCH: c_int = 2;
    pub const OBJECTIVE: c_int = 3;
}

/// [`GameState::round`] values.
pub mod round {
    use core::ffi::c_int;
    /// The game has no rounds (deathmatch, team deathmatch).
    pub const NONE: c_int = 0;
    /// A round is running ([`GameState::round_ends_at`] tells when its time runs out).
    pub const RUNNING: c_int = 1;
    /// The round is over and the next starts at [`GameState::round_restart_at`].
    pub const OVER: c_int = 2;
}

/// The game's rules and state, as [`PluginApi::game_state`] reports it.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct GameState {
    /// [`mode`].
    pub mode: c_int,
    pub teamplay: c_int,
    /// Last man standing, and The One modes.
    pub last_man_standing: c_int,
    pub the_one: c_int,
    /// The map has a capture-the-briefcase case.
    pub ctb: c_int,
    /// The teams in play (teamplay), else 0.
    pub teams: c_int,
    /// [`round`].
    pub round: c_int,
    pub round_ends_at: f32,
    pub round_restart_at: f32,
    /// The game is over (intermission).
    pub game_over: c_int,
    /// `gpGlobals->time`, `maxClients`.
    pub time: f32,
    pub max_clients: c_int,
    /// Each team's score as the `TeamScore` message sends it (`CTSGameRules`' team scores: round
    /// wins, captures), by [`PlayerInfo::team`] - 1. 0 for a team not in play.
    pub team_scores: [c_int; MAX_TEAMS],
}

/// A bot's usercmd for [`PluginApi::run_cmd`]: `pfnRunPlayerMove`'s arguments, and how the
/// server is to run it.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct BotCmd {
    /// View angles (pitch, yaw, roll), as `pev->v_angle`.
    pub angles: [f32; 3],
    pub forward: f32,
    pub side: f32,
    pub up: f32,
    /// `IN_*` bits.
    pub buttons: c_int,
    pub impulse: c_int,
    /// The command's duration in milliseconds (1..255), or 0 for the server's choice (as
    /// [`cmd::AUTO_MSEC`]).
    pub msec: c_int,
    /// Version 6: [`cmd`] bits.
    pub flags: c_int,
}

/// [`BotCmd::flags`] (version 6).
pub mod cmd {
    use core::ffi::c_int;
    /// Turn the bot as the native bots do (`CBotBrain::UpdateView`): before the move, write
    /// `angles` to `pev->v_angle` and `pev->angles` (pitch × -1/3, roll 0) and clear
    /// `pev->fixangle`. Without it a bot the game turned (`fixangle`: a spawn, a teleport,
    /// `ts_tp`) keeps ignoring `angles`, because only a real client's network update clears
    /// `fixangle`. When the game had set `fixangle`, the game's angles win for this command
    /// (`angles` is ignored) and the result says [`super::cmd_event::FIXANGLE`]: adopt
    /// [`BotCmdResult::v_angle`](super::BotCmdResult::v_angle).
    pub const SET_VIEW: c_int = 0x1;
    /// The server picks `msec`: the frame time (`gpGlobals->frametime`) in whole milliseconds,
    /// with the fraction carried over to the bot's next command, so its commands add up to the
    /// server's time. `msec` 0 means the same.
    pub const AUTO_MSEC: c_int = 0x2;
}

/// [`BotCmdResult::events`] (version 6).
pub mod cmd_event {
    use core::ffi::c_int;
    /// The bot was moved further than it could have walked since its last command: a teleport,
    /// a respawn, `ts_tp`. Drop path state.
    pub const TELEPORTED: c_int = 0x1;
    /// The game set the bot's view (`fixangle`), before the command or during it: its angles are
    /// [`BotCmdResult::v_angle`](super::BotCmdResult::v_angle).
    pub const FIXANGLE: c_int = 0x2;
    /// The bot was in the air before the command and on the ground after it.
    pub const LANDED: c_int = 0x4;
}

/// What a [`PluginApi::run_cmd`] did to the bot (version 6), read after `pfnRunPlayerMove`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct BotCmdResult {
    /// The bot's view angles now (the game's after [`cmd_event::FIXANGLE`]), origin and velocity.
    pub v_angle: [f32; 3],
    pub origin: [f32; 3],
    pub velocity: [f32; 3],
    /// [`cmd_event`] bits.
    pub events: c_int,
    /// The `msec` the command ran with (the server's choice under [`cmd::AUTO_MSEC`]).
    pub msec: c_int,
}

/// [`PlayerSnapshot::flags`] (version 6).
pub mod player_flag {
    use core::ffi::c_int;
    /// Past the team/MOTD menus and not spectating ([`PlayerInfo::joined`](super::PlayerInfo::joined)).
    pub const JOINED: c_int = 0x1;
    /// Alive and not observing.
    pub const ALIVE: c_int = 0x2;
    /// A bot of any backend.
    pub const BOT: c_int = 0x4;
    /// `FL_ONGROUND`, `FL_DUCKING`.
    pub const ON_GROUND: c_int = 0x8;
    pub const DUCKING: c_int = 0x10;
    /// On a ladder (`MOVETYPE_FLY`, which the player movement sets there).
    pub const ON_LADDER: c_int = 0x20;
    /// Waist deep in water or more (`waterlevel` ≥ 2).
    pub const IN_WATER: c_int = 0x40;
    /// Carries the capture-the-briefcase case.
    pub const HAS_FLAG: c_int = 0x80;
    /// Observing (`pev->iuser1`, an observer mode).
    pub const SPECTATOR: c_int = 0x100;
    /// Lying after a dive or slide ([`stunt_bits::PRONE`]).
    pub const PRONE: c_int = 0x200;
    /// In a stunt: diving, flipping, rolling, on a wall, or ducked for one
    /// ([`stunt_bits::STUNTING`]).
    pub const STUNTING: c_int = 0x400;
    /// Version 7: permanent kung fu and superjump picked up this life
    /// ([`PlayerInfo::kungfu`](super::PlayerInfo::kungfu), [`PlayerInfo::superjump`](super::PlayerInfo::superjump)).
    /// Kung fu in hand is [`PlayerSnapshot::weapon`](super::PlayerSnapshot::weapon) 0.
    pub const KUNG_FU: c_int = 0x800;
    pub const SUPERJUMP: c_int = 0x1000;
}

/// TS's stunt bits (`pev->iuser4`, [`PlayerSnapshot::stunt_bits`]; version 6). They are the
/// game's state, sent to the client in `clientdata`.
pub mod stunt_bits {
    use core::ffi::c_int;
    /// Half speed while getting up (`BlockPlayer` too).
    pub const RECOVER: c_int = 0x1;
    /// Set on death.
    pub const DEAD_LOCK: c_int = 0x2;
    /// A stunt is in progress: duck hull, no jump or duck.
    pub const GO_DUCK: c_int = 0x4;
    pub const STAND_UP: c_int = 0x8;
    pub const DIVING: c_int = 0x10;
    /// Lying after a dive or slide.
    pub const PRONE: c_int = 0x20;
    /// Stealth shoes (silent footsteps).
    pub const STEALTH_SHOES: c_int = 0x200;
    pub const FLIP_HIGH: c_int = 0x400;
    pub const FLIP_LOW: c_int = 0x800;
    pub const WALLJUMPING: c_int = 0x10000;
    pub const WALLCLIMBING: c_int = 0x20000;
    pub const ROLLING: c_int = 0x40000;
    pub const SUPERJUMP_GLIDE: c_int = 0x100000;
    pub const WALLDIVE: c_int = 0x800000;
    /// What [`player_flag::STUNTING`](super::player_flag::STUNTING) tests.
    pub const STUNTING: c_int = GO_DUCK | DIVING | FLIP_HIGH | FLIP_LOW | WALLJUMPING | WALLCLIMBING | ROLLING | WALLDIVE;
}

/// A player's close-combat and stunt timers ([`PlayerSnapshot::combat`], version 6), as game
/// times (`gpGlobals->time`) at which they run out, 0 for one that isn't running. TS counts most of
/// them down in the player's own time (`pev->fuser1`): a slowed player's run out later than
/// they say.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CombatState {
    /// Kung-fu power, 0..100 (`pl+0x774`): the bar a move costs and blocks drain.
    pub melee_power: f32,
    /// No attack before (`m_flNextAttack`, `pl+0x12c`; the gun's primary delay when that is
    /// later).
    pub next_attack_at: f32,
    /// No stunt before (`CTSStunt`'s cooldown, the `NextStunt` message).
    pub next_stunt_at: f32,
    /// Getting up (`CTSStunt`'s recovery lock, or `BlockPlayer`'s, the later): half speed.
    pub recover_until: f32,
    /// Can't get up from lying before (`CTSStunt+0x30`).
    pub prone_lock: f32,
    /// The slow-motion dodge: bullets do ×0.7 until (`CTSStunt+0x34`).
    pub dodge_until: f32,
    /// Block windows (`pl+0x7a8` kung fu, `+0x7ac` knife, `+0x7b0` katana): blows of that kind
    /// are blocked until.
    pub block_kungfu: f32,
    pub block_knife: f32,
    pub block_katana: f32,
    /// A delayed melee hit lands until (`pl+0x79c`).
    pub melee_window: f32,
}

/// One player slot as [`PluginApi::players`] reports it (version 6): built once a frame, at
/// `StartFrame` before the plugins' [`Plugin::start_frame`].
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct PlayerSnapshot {
    /// The engine's user id of the client in the slot (a serial that is new for every
    /// connection), 0 for an empty slot (then every other field is 0).
    pub user_id: c_int,
    /// [`player_flag`] bits.
    pub flags: c_int,
    /// As [`PlayerInfo::team`].
    pub team: c_int,
    /// `pev`'s `origin`, `velocity`, `v_angle`, `view_ofs`, `absmin` and `absmax`.
    pub origin: [f32; 3],
    pub velocity: [f32; 3],
    pub v_angle: [f32; 3],
    pub view_ofs: [f32; 3],
    pub absmin: [f32; 3],
    pub absmax: [f32; 3],
    /// `pev->health`, `pev->armorvalue`.
    pub health: f32,
    pub armor: f32,
    /// `pev->button` (`IN_*`) of the player's last command.
    pub buttons: c_int,
    /// As [`PlayerInfo`]'s weapon fields: the TS id in hand (0 kung fu), its clip (the kung-fu bar
    /// with kung fu), reserve, fire mode and attachment bits.
    pub weapon: c_int,
    pub clip: c_int,
    pub ammo: c_int,
    pub fire_mode: c_int,
    pub attachments: c_int,
    /// As [`PlayerInfo::stunt_state`] (`TSState`: 0 stand, 1 crouch, 2 prone or diving), and the
    /// [`stunt_bits`].
    pub stance: c_int,
    pub stunt_bits: c_int,
    /// As [`PlayerInfo`]'s power-up fields.
    pub active_powerup: c_int,
    pub stored_powerup: c_int,
    pub powerup_until: f32,
    /// As [`PlayerInfo::time_scale`].
    pub time_scale: f32,
    /// The scoreboard's frags (`pev->frags`) and deaths.
    pub frags: c_int,
    pub deaths: c_int,
    /// As [`PlayerInfo::free_slots`].
    pub free_slots: c_int,
    /// Changes (by 1) whenever [`PluginApi::player_inventory`]'s list does (a weapon got or lost,
    /// a clip, reserve, mode or attachment changed): read the inventory again only then. Starts at
    /// 1 for a new client.
    pub inventory_serial: c_int,
    pub combat: CombatState,
}

/// An item lying in the map, as [`PluginApi::items`] lists it (version 6).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct ItemInfo {
    /// Its entity index and the engine's serial number of the slot (an index with another
    /// serial is another entity).
    pub index: c_int,
    pub serial: c_int,
    /// [`entity::GROUND_WEAPON`], [`entity::POWERUP`] or [`entity::GUN`] (a dropped or thrown gun),
    /// with its TS weapon id or [`powerup`] bits, as [`EntityInfo`] has them.
    pub kind: c_int,
    pub weapon: c_int,
    pub powerup: c_int,
    pub origin: [f32; 3],
    /// It can be picked up now (as [`EntityInfo::available`]).
    pub available: c_int,
    /// Taken and coming back: the game time it shows again; 0 when available, or when it waits
    /// for the next round (a round mode's weapon without a respawn time).
    pub respawn_at: f32,
}

/// The frame, as [`PluginApi::frame_info`] reports it (version 6).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct FrameInfo {
    /// `gpGlobals->time` and `frametime` at `StartFrame`.
    pub time: f32,
    pub frametime: f32,
    /// The highest entity index in use this frame: scan entities up to it.
    pub highest_edict: c_int,
    /// As [`GameState`]'s `teamplay`, `ctb` (the map had a capture point when it started),
    /// [`mode`] and [`round`].
    pub teamplay: c_int,
    pub ctb: c_int,
    pub mode: c_int,
    pub round: c_int,
    /// Version 7: as [`GameState::team_scores`].
    pub team_scores: [c_int; MAX_TEAMS],
}

/// A server command a plugin registered: `argv[0]` is the command name.
pub type CommandFn = unsafe extern "C" fn(argc: c_int, argv: *const *const c_char);

/// The table [`INTERFACE_NAME`] and [`LOAD_SYMBOL`] hand out. Every function follows the module's
/// rules. Strings are NUL-terminated and only read during the call; buffers get a NUL-terminated
/// string cut to fit, and the function returns the full length (or a status below 0).
#[repr(C)]
pub struct PluginApi {
    /// `size_of::<PluginApi>()` in the server that built it.
    pub size: u32,
    /// [`VERSION`] of the server that built it.
    pub version: u32,

    // ---- the host -----------------------------------------------------------------------------
    /// The engine's function table (`enginefuncs_t *`, the FWGS headers' layout, as the server
    /// got it) and globals (`globalvars_t *`). Don't write to the table.
    pub engine_funcs: *const c_void,
    pub globals: *mut c_void,
    /// Prints `msg` to the server console as `[tsmod_<name>] msg` ([`log`] level); an attached
    /// plugin's lines (version 7) as `[<name>] msg`, and [`NO_PLUGIN`]'s as `[plugin] msg`.
    pub log: unsafe extern "C" fn(plugin: PluginId, level: c_int, msg: *const c_char),
    /// The plugin's own data directory, `<gamedir>/tsmod/<name>/` with a trailing slash (created
    /// on first use): waypoints, learned data, configs. Loaded and attached plugins alike;
    /// [`INVALID`] for [`NO_PLUGIN`] or an id that isn't (or no longer is) a plugin's.
    pub data_dir: unsafe extern "C" fn(plugin: PluginId, buf: *mut c_char, len: c_int) -> c_int,
    /// The current map's name.
    pub map_name: unsafe extern "C" fn(buf: *mut c_char, len: c_int) -> c_int,
    /// Registers a server command (console, configs, rcon) for the process. [`EXISTS`] if the
    /// server or a plugin already has the name.
    pub register_command: unsafe extern "C" fn(plugin: PluginId, name: *const c_char, f: CommandFn) -> c_int,
    /// Registers a cvar (`pfnCVarRegister`) and returns the engine's `cvar_t *` (read its
    /// `value`/`string`; set it with the engine's `pfnCvar_DirectSet`), or the existing one if
    /// the name is taken. NULL for a bad name.
    pub register_cvar: unsafe extern "C" fn(plugin: PluginId, name: *const c_char, value: *const c_char, flags: c_int) -> *mut c_void,
    /// Registers a bot backend under `name` (not `native`): [`OK`], [`EXISTS`] or [`INVALID`].
    /// Only plugins with an id can: loaded ones and (version 7) attached ones. A backend that
    /// registers after the map start chose the map's backend, while `ts_bot_backend` names it and
    /// native runs, takes over at once (as a map start would switch to it).
    pub register_bot_backend: unsafe extern "C" fn(plugin: PluginId, name: *const c_char, backend: *const BotBackend) -> c_int,

    // ---- players ------------------------------------------------------------------------------
    /// Fills `out` with `player`'s state: [`OK`] or [`NOT_PLAYER`]. `out` must be valid for
    /// writes; it is left untouched on failure.
    pub player_info: unsafe extern "C" fn(player: c_int, out: *mut PlayerInfo) -> c_int,
    /// Fills `out[..max]` with the weapons the player owns (kung fu not included), by id, and
    /// returns how many it owns (which may be more than `max`), or [`NOT_PLAYER`].
    pub player_inventory: unsafe extern "C" fn(player: c_int, out: *mut WeaponEntry, max: c_int) -> c_int,
    /// Sets the money and updates the client's HUD (`TSCash`).
    pub set_cash: extern "C" fn(player: c_int, cash: c_int) -> c_int,
    /// Sets the free inventory slots (and the walking speed that depends on them).
    pub set_free_slots: extern "C" fn(player: c_int, slots: c_int) -> c_int,
    /// Sets the award/status byte and broadcasts it (`TSPAward`).
    pub set_award: extern "C" fn(player: c_int, award: c_int) -> c_int,
    /// Runs `powerup` (one [`powerup`] bit) now for `duration` seconds, without taking it from
    /// the player's stored power-up. 0 stops the running power-up. A negative duration uses the
    /// power-up's normal length.
    pub run_powerup: extern "C" fn(player: c_int, powerup: c_int, duration: f32) -> c_int,
    /// Slows (or speeds up) the player: time scale `speed` (1 normal, 0 frozen) for `duration`
    /// seconds, and everyone within `aura` units of them too.
    pub set_speed: extern "C" fn(player: c_int, speed: f32, aura: f32, duration: f32) -> c_int,
    /// Time scale of what the player sees of the world (sparks, sounds); negative pauses it.
    pub set_physics_speed: extern "C" fn(player: c_int, speed: f32) -> c_int,
    /// Shows (non-zero) or hides bullet trails for this player only.
    pub set_bullet_trail: extern "C" fn(player: c_int, on: c_int) -> c_int,
    /// Sets the clip and reserve ammo of the player's weapon with TS id `weapon` (a negative
    /// value leaves that count alone) and updates the HUD.
    pub set_weapon_ammo: extern "C" fn(player: c_int, weapon: c_int, clip: c_int, ammo: c_int) -> c_int,

    // ---- entities and the game -----------------------------------------------------------------
    /// The player responsible for game entity `entity` (a thrown knife, a grenade, a placed
    /// C4): their index, 0 if none, or [`NOT_PLAYER`] if `entity` isn't a game entity.
    pub projectile_owner: extern "C" fn(entity: c_int) -> c_int,
    /// Fills `out` with what game entity `entity` is: [`OK`] or [`NOT_PLAYER`] (not a game
    /// entity). Other entities are [`entity::OTHER`].
    pub entity_info: unsafe extern "C" fn(entity: c_int, out: *mut EntityInfo) -> c_int,
    /// `UTIL_IsMasterTriggered(master, activator)`: 1 if the entities named `master` (a `master`
    /// keyvalue: a `multisource` or `game_team_master`) let `activator` (a player, or 0) through,
    /// else 0. An empty name is 1. As in TS, a name that isn't a master counts as triggered.
    pub master_triggered: unsafe extern "C" fn(master: *const c_char, activator: c_int) -> c_int,
    /// Fills `out` with the rules and round state: [`OK`].
    pub game_state: unsafe extern "C" fn(out: *mut GameState) -> c_int,
    /// Fills `out[..max]` with the objectives on the HUD list (TS's `Objective` messages), in list
    /// order, and returns how many there are (which may be more than `max`). The names are those
    /// `player` would read (its team's text); `player` 0 asks for no team. [`NOT_PLAYER`] for
    /// any other index that isn't a connected player.
    pub objectives: unsafe extern "C" fn(player: c_int, out: *mut ObjectiveInfo, max: c_int) -> c_int,
    /// Creates a game entity of class `classname` (`CREATE_NAMED_ENTITY`), not spawned yet: its
    /// index, or [`INVALID`] (no such class, or no free edict). Set its keys with
    /// [`entity_key_value`](Self::entity_key_value), then [`spawn_entity`](Self::spawn_entity).
    pub create_entity: unsafe extern "C" fn(classname: *const c_char) -> c_int,
    /// `DispatchKeyValue(entity, key, value)` as the engine passes a map entity's keys: the
    /// entvars fields first, then the class. 1 if handled, 0 if not, [`NOT_PLAYER`] (not a game
    /// entity) or [`INVALID`] (a NULL string).
    pub entity_key_value: unsafe extern "C" fn(entity: c_int, key: *const c_char, value: *const c_char) -> c_int,
    /// `DispatchSpawn(entity)` (its `Spawn`, with the hooks). [`OK`], [`NOT_PLAYER`], or
    /// [`INVALID`] when the spawn failed: the entity is then removed, as the engine removes a map
    /// entity whose spawn fails.
    pub spawn_entity: extern "C" fn(entity: c_int) -> c_int,

    // ---- bots ---------------------------------------------------------------------------------
    /// Makes a bot player as TS's `BotSetup` does (a fake client, joined and spawned, in
    /// teamplay on the team `model` names) for the active backend: its index, [`FULL`] or
    /// [`INVALID`] (an empty name, or the caller's backend isn't the active one). `skill` is
    /// only reported. `name` and `model` are NUL-terminated byte strings (not UTF-8), used as
    /// they are for the netname, the `model`/`team` keys and the team, as TS's `strcpy`s.
    pub create_bot: unsafe extern "C" fn(plugin: PluginId, name: *const c_char, model: *const c_char, skill: f32) -> c_int,
    /// Kicks a bot (queued as the server's `kick #<userid>`): [`OK`], [`NOT_PLAYER`], or
    /// [`INVALID`] for a human.
    pub kick_bot: extern "C" fn(player: c_int) -> c_int,
    /// Runs one usercmd for the bot now (`pfnRunPlayerMove`: its PreThink, movement and
    /// PostThink run), as [`BotCmd::flags`] say, and fills `out` (may be NULL) with what it did.
    /// With the port's `ts_bot_stop 1` a living bot moves and presses nothing (it still turns).
    /// `gpGlobals->time` and `frametime` are put back afterwards (the engine leaves them at the
    /// command's end). [`OK`], [`NOT_PLAYER`], [`INVALID`] (a human, or a bad `msec`, impulse or
    /// angle), [`BUSY`] (inside one of the bot's own callbacks: call it from
    /// [`Plugin::start_frame`]); `out` is written only for `OK`.
    pub run_cmd: unsafe extern "C" fn(player: c_int, cmd: *const BotCmd, out: *mut BotCmdResult) -> c_int,
    /// Runs a client command as if the player typed it (`use`, `usepowerup`, `respawn`,
    /// `jointeam 1`, `say hi`): split on spaces, double quotes group. [`OK`] (also for a command
    /// the game doesn't know), [`DEFERRED`], [`NOT_PLAYER`] or [`INVALID`] (empty).
    pub bot_command: unsafe extern "C" fn(player: c_int, line: *const c_char) -> c_int,
    /// Switches to the weapon with TS id `weapon` (0 kung fu) as `weapon_<id>` does: [`OK`],
    /// [`DEFERRED`], [`NOT_PLAYER`], or [`INVALID`] (not owned, or out of range).
    pub select_weapon: extern "C" fn(player: c_int, weapon: c_int) -> c_int,
    /// Adds a weapon to the player's loadout, which TS buys at the next spawn
    /// (`CBasePlayer::BuyArmament`, under the server's weapon restrictions and free slots; TS
    /// 3.0 charges no money): with `attachments` (`TSA_*`), a second copy for an akimbo pair
    /// when `akimbo` is non-zero, and `ammo` purchases of its ammo type. [`OK`], [`DEFERRED`],
    /// [`NOT_PLAYER`] or [`INVALID`].
    pub buy: extern "C" fn(player: c_int, weapon: c_int, attachments: c_int, akimbo: c_int, ammo: c_int) -> c_int,
    /// Adds an [`item`] (kevlar, stealth shoes) to the loadout.
    pub buy_item: extern "C" fn(player: c_int, item: c_int) -> c_int,
    /// Empties the loadout (`rst_cfg`).
    pub clear_loadout: extern "C" fn(player: c_int) -> c_int,
    /// Drops the weapon with TS id `weapon`, or the one in hand for a negative id (`drop`,
    /// `drop_<id>`).
    pub drop_weapon: extern "C" fn(player: c_int, weapon: c_int) -> c_int,

    // ---- entity hooks (module docs) ------------------------------------------------------------
    /// Installs the plugin's hook (NULL removes it). One hook per server; the plugin dispatches.
    pub ham_set_hook: extern "C" fn(hook: Option<ham::Hook>),
    /// Turns calling the hook for function `ham` on (non-zero) or off: [`OK`], or
    /// [`UNSUPPORTED`] if the port has no such function for any class.
    pub ham_watch: extern "C" fn(ham: c_int, on: c_int) -> c_int,
    /// Whether class `class` has function `ham`: 1 (hooks fire, `ham_execute` runs it), 0 (it
    /// exists in TS but the port never calls it for this class: hooks never fire, `ham_execute`
    /// runs the default), or [`UNSUPPORTED`].
    pub ham_supported: extern "C" fn(ham: c_int, class: c_int) -> c_int,
    /// The class of entities linked under `classname` (> 0), or [`INVALID`] for an unknown name.
    pub class_id: unsafe extern "C" fn(classname: *const c_char) -> c_int,
    /// The class of game entity `entity` (> 0), or [`NOT_PLAYER`] if it isn't one.
    pub entity_class_id: extern "C" fn(entity: c_int) -> c_int,
    /// Runs `call.ham` on `call.this` with `call.args`, putting the result in `call.ret`. With
    /// `run_hooks` non-zero the hook runs around it as for a call by the game (hamsandwich's
    /// ExecuteHamB); otherwise it doesn't (ExecuteHam), for this call only: the calls it makes
    /// (a `Killed` from `TakeDamage`) run the hook as usual. `call.class` is ignored. [`OK`],
    /// [`NOT_PLAYER`] (not a game entity), [`INVALID`] (wrong arguments, or an `ENTITY` argument
    /// that is neither -1 nor an entity in use), [`UNSUPPORTED`] or [`BUSY`].
    pub ham_execute: unsafe extern "C" fn(call: *mut HamCall, run_hooks: c_int) -> c_int,

    // ---- version 5: listeners, private data ------------------------------------------------------
    /// Registers a [`Plugin`] descriptor's callbacks for a plugin the server didn't load (a
    /// metamod plugin; a `tsmod_*` library has its own descriptor). The server copies it and calls
    /// it from then on as it calls loaded plugins, after them, in registration order (callbacks of
    /// events already past, such as `game_init`, only when they come round again). Version 7:
    /// `plugin` is the owner, [`NO_PLUGIN`] or an attached plugin's id, whose
    /// [`detach_plugin`](Self::detach_plugin) removes it. A handle (> 0) for
    /// [`remove_listener`](Self::remove_listener), or [`INVALID`]: NULL, a `size` other than
    /// `size_of::<Plugin>()`, an `api_version` other than [`VERSION`], or a `plugin` that isn't
    /// [`NO_PLUGIN`] or attached.
    pub add_listener: unsafe extern "C" fn(plugin: PluginId, listener: *const Plugin) -> c_int,
    /// Unregisters a listener: [`OK`], or [`INVALID`] for a handle that isn't registered.
    pub remove_listener: extern "C" fn(handle: c_int) -> c_int,
    /// Bytes of game entity `entity`'s private data (`pvPrivateData`), or [`NOT_PLAYER`] (not a
    /// game entity). See the module docs ("Private data"): bytes 8..16 are the server's link to
    /// the entity, never write them.
    pub private_size: extern "C" fn(entity: c_int) -> c_int,

    // ---- version 6: the frame's world ---------------------------------------------------------------
    /// Fills `out[..max]` with player slots 1..=`max` (`out[0]` is slot 1) from the frame's
    /// snapshot and returns `maxClients`; an empty slot is all zeroes. NULL `out` only counts.
    pub players: unsafe extern "C" fn(out: *mut PlayerSnapshot, max: c_int) -> c_int,
    /// Fills `out[..max]` with the items in the map (ground weapons, power-ups, dropped guns; taken
    /// ones too, with their `respawn_at`), in index order, and returns how many there are (which
    /// may be more than `max`).
    pub items: unsafe extern "C" fn(out: *mut ItemInfo, max: c_int) -> c_int,
    /// Fills `out` with the frame's time and rules: [`OK`], or [`INVALID`] for NULL.
    pub frame_info: unsafe extern "C" fn(out: *mut FrameInfo) -> c_int,
    /// The value of map key `key` that game entity `entity` was given (by the map or
    /// [`entity_key_value`](Self::entity_key_value)) and that isn't an `entvars_t` field, such as
    /// `master` (a button's or door's `multisource`), the last one if given twice: written to
    /// `buf` as the module's strings, its length returned. [`NOT_PLAYER`] (not a game entity),
    /// [`INVALID`] (a NULL key) or [`UNSUPPORTED`] (the entity wasn't given that key; read entvars
    /// fields from the edict).
    pub entity_key: unsafe extern "C" fn(entity: c_int, key: *const c_char, buf: *mut c_char, len: c_int) -> c_int,

    // ---- version 7: plugin identities ---------------------------------------------------------------
    /// Gives a plugin the server didn't load (a metamod plugin, an AMX Mod X module) an identity
    /// under `name` (1 to 32 letters, digits, `_` or `-`, as `<name>` of `tsmod_<name>`): a new
    /// [`PluginId`] (> 0, never handed out before) that works wherever a loaded plugin's does
    /// (its [`data_dir`](Self::data_dir) `<gamedir>/tsmod/<name>/`, the [`log`](Self::log)
    /// prefix, [`register_command`](Self::register_command), [`register_cvar`](Self::register_cvar),
    /// [`register_bot_backend`](Self::register_bot_backend), [`create_bot`](Self::create_bot),
    /// [`add_listener`](Self::add_listener)). [`EXISTS`] if a loaded or attached plugin has the
    /// name (letter case ignored), [`INVALID`] for NULL or a bad name. Callable any time from the
    /// engine's thread, also before `GameDLLInit` (metamod's `Meta_Attach`); a `tsmod_*` library
    /// whose name an attached plugin took is then not loaded.
    pub attach_plugin: unsafe extern "C" fn(name: *const c_char) -> PluginId,
    /// Gives an attached plugin's identity up: its server commands do nothing from then on (the
    /// engine keeps their names), its listeners are removed, and its bot backend is unregistered;
    /// if that backend was active, it is deactivated, its bots are kicked and native takes over
    /// (as a map start switching backends). Its cvars stay (the engine can't drop them), and so
    /// does an entity hook it installed ([`ham_set_hook`](Self::ham_set_hook) is the server's).
    /// The id is dead afterwards; the name is free to attach again (with a new id). [`OK`], or
    /// [`INVALID`] for an id that isn't attached (a loaded plugin's included: those stay).
    pub detach_plugin: extern "C" fn(plugin: PluginId) -> c_int,

    // ---- version 8: client extensions, the port client's version --------------------------------
    /// Sends `len` bytes of `data` on `channel` (1 to [`EXT_CHANNEL_MAX`] bytes of ASCII letters,
    /// digits, `_`, `-`, `.`) to the client extension that claimed the channel, as one `TSExt`
    /// message (the port's user message for extensions, docs/implementation/client-ext-api.md
    /// §10): to `player`, or with `player` 0 to every port client. Only port clients get it (their
    /// userinfo has `_tsrs`): TS 3.0's client doesn't hook `TSExt` (client-plugins.md §12). The
    /// payload is at most [`EXT_MESSAGE_MAX`] minus the channel's length and its NUL (176 bytes
    /// for a 15-byte channel, 184 for `webview`); a longer body is the channel's protocol to split.
    /// `flags`: [`ext_flag`] bits (0 sends reliably). Returns the number of clients it went to (1
    /// for a player; any number for 0), or [`NOT_PLAYER`] (no connected player there),
    /// [`UNSUPPORTED`] (that player isn't a port client: TS 3.0's client, a bot, an HLTV proxy) or
    /// [`INVALID`] (a bad channel, a payload that doesn't fit, NULL `data` with a length, an
    /// unknown flag). A client without an extension on the channel drops the message.
    pub ext_message: unsafe extern "C" fn(player: c_int, channel: *const c_char, data: *const u8, len: c_int, flags: c_int) -> c_int,
    /// The port version a player's client reported (its `_tsrs` userinfo, the numeric form a first
    /// connect sends included) as `major.minor.patch`, written to `buf` (NUL-terminated, cut to
    /// fit); returns its length. 0 (and "") for a client without the port: TS 3.0's, a bot, an
    /// HLTV proxy. [`UNSUPPORTED`] for a port client whose value isn't a version (the `1` clients
    /// from before #303 sent first), [`NOT_PLAYER`] for no connected player there
    /// (client-plugins.md §13.1).
    pub client_port_version: unsafe extern "C" fn(player: c_int, buf: *mut c_char, len: c_int) -> c_int,
    /// Offers client plugin `id` now, as `sv_plugin_offer` does (client-plugins.md §6.1): one
    /// `TSPkg` offer of the `plugin_offers.toml` entry `id`, which must have passed the map's
    /// package checks (`sv_plugin_offers` lists them), to `player`, or with `player` 0 to every
    /// client that hooks `TSPkg`. The client answers through [`Plugin::client_plugin_state`].
    /// Returns the number of clients it went to (1 for a player; any number for 0), 0 also when
    /// the offer can't be sent now (a relative `url` while `sv_downloadurl` is empty or isn't
    /// `http(s)://`; the server log says why); or [`NOT_PLAYER`] (no connected player there),
    /// [`INVALID`] (NULL `id`, or no checked offer of it) or [`UNSUPPORTED`] (that player's client
    /// doesn't hook `TSPkg`: TS 3.0's, a port client before 3.0.9, a bot, an HLTV proxy).
    pub offer_client_plugin: unsafe extern "C" fn(player: c_int, id: *const c_char) -> c_int,
    /// The version of client plugin `id` (a plugin id or `manual:<name>`) a player's client has
    /// loaded, as far as the server knows (client-plugins.md §13.3): its answer `loaded`, else its
    /// `tspkg list`, else its answer `have` to an offer (an approved manifest it loads at once).
    /// Written to `buf` (NUL-terminated, cut to fit); returns its length, 0 (and "") when the
    /// server doesn't know it loaded. [`NOT_PLAYER`] (no connected player there), [`INVALID`]
    /// (NULL `id`) or [`UNSUPPORTED`] (a client that doesn't hook `TSPkg`, and so runs no client
    /// plugins the server can learn of).
    pub client_plugin_version: unsafe extern "C" fn(player: c_int, id: *const c_char, buf: *mut c_char, len: c_int) -> c_int,
}

/// The longest [`PluginApi::ext_message`] channel, in bytes.
pub const EXT_CHANNEL_MAX: usize = 15;
/// The longest `TSExt` message (the channel, its NUL and the payload): the Steam engines' user
/// message limit.
pub const EXT_MESSAGE_MAX: usize = 192;

/// [`PluginApi::ext_message`]'s `flags`.
pub mod ext_flag {
    use core::ffi::c_int;
    /// Unreliable: dropped rather than queued when the client's channel is full.
    pub const UNRELIABLE: c_int = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_follow_the_server_library() {
        assert_eq!(library_suffix(true, false), ".dll"); // mp.dll
        assert_eq!(library_suffix(true, true), "_amd64.dll"); // mp_amd64.dll
        assert_eq!(library_suffix(false, false), "_i386.so"); // ts_i386.so
        assert_eq!(library_suffix(false, true), "_amd64.so"); // ts_amd64.so
    }

    #[test]
    fn plugin_states_are_the_protocols_order() {
        use plugin_state::*;
        assert_eq!(
            [HAVE, OFFERED, DOWNLOADING, INSTALLED, LOADED, DENIED, BLOCKED, UNSUPPORTED, FAILED, SUSPENDED, RESTART],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }

    #[test]
    fn the_interface_name_carries_the_version() {
        assert_eq!(VERSION, 8);
        assert!(INTERFACE_NAME.to_bytes().ends_with(b"008"));
    }

    #[test]
    fn stunting_is_the_moving_stunt_bits() {
        // `shared::ts::stunt`'s values: the airborne mask (0x10c10) plus duck, roll, climb, wall dive.
        assert_eq!(stunt_bits::STUNTING, 0x10c10 | 0x4 | 0x40000 | 0x20000 | 0x800000);
        assert_eq!(stunt_bits::STUNTING & (stunt_bits::PRONE | stunt_bits::RECOVER | stunt_bits::STEALTH_SHOES), 0);
    }

    #[test]
    fn player_flags_are_distinct_bits() {
        use player_flag::*;
        let all = [JOINED, ALIVE, BOT, ON_GROUND, DUCKING, ON_LADDER, IN_WATER, HAS_FLAG, SPECTATOR, PRONE, STUNTING, KUNG_FU, SUPERJUMP];
        let mut seen = 0;
        for f in all {
            assert_eq!(f.count_ones(), 1, "{f:#x}");
            assert_eq!(seen & f, 0, "{f:#x} twice");
            seen |= f;
        }
        // Version 7 appended its flags after version 6's.
        assert_eq!((KUNG_FU, SUPERJUMP), (0x800, 0x1000));
    }
}
