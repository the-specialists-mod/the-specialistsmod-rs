//! The server side of the plugin API (`ts_plugin_api::PluginApi`): a port addition, TS has no
//! such API. AMX Mod X's TS module (`tsx`) read and wrote TS's `CBasePlayer` at fixed offsets; the
//! port keeps that state in [`Player`], so plugins reach it through this table, which `ts-server-dll`
//! hands out from `CreateInterface` (see docs/implementation/plugin-api.md).
//!
//! # Reaching the player
//! A plugin runs inside engine callbacks, possibly while the server is inside one of that very
//! player's callbacks (a message hook during its think): the player object is then taken out of
//! the slab (`world.rs`). So:
//! * [`player_info`] reads the object when it can be reached (in its slot, or lent back), and
//!   otherwise returns the last [`Snapshots`] entry, which [`refresh`] keeps current: at the end of
//!   every PostThink, every StartFrame, and after every change made here.
//! * Setters validate their arguments first (`INVALID`, nothing changes), then run on the object
//!   now, or queue with `world::defer` until the dispatch that holds it returns (`DEFERRED`).
//!
//! Nothing here sends a message while the plugin's call is running, except `set_award`'s
//! `TSPAward` and `run_powerup`'s `PwUpTime` / `TSMBlur`, as TS's own code paths do (`PlayerKilled`,
//! `UsePowerUp`); the rest goes out with the player's next `UpdateClientData` (`player::hud`).
//!
//! Version 3 (issue #104) added the host services ([`crate::plugins::host`]), the bot functions
//! ([`bots`]), and the entity and game state ([`world_info`]). Version 6 added the frame's world
//! ([`frame`]: every player, the items, the frame), what the new callbacks report ([`events`]) and
//! the map keys the classes don't keep ([`keys`]). Version 8 added the port client's extensions and
//! version ([`ext`]) and the client plugin offers ([`client_plugins`]).

pub mod bots;
pub mod client_plugins;
pub mod events;
pub mod ext;
pub mod frame;
pub mod keys;
pub mod world_info;

pub use events::source_player;

use crate::player::Player;
use crate::rules::{self, powerup};
use crate::world::{self, Cx};
use goldsrc::Global;
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::cell::RefCell;
use std::ffi::{CStr, c_int};
use ts_plugin_api::{DEFERRED, INVALID, NOT_PLAYER, OK, PlayerInfo, UNSUPPORTED, WeaponEntry};

/// `CreateInterface`'s test: `name` is this server's table.
pub fn interface(name: &CStr) -> bool {
    name == ts_plugin_api::INTERFACE_NAME
}

// ---- changes --------------------------------------------------------------------------------

/// The most money TS lets a player have (`CBasePlayer::GiveCash`, `lifecycle::give_cash`).
pub const MAX_CASH: i32 = 30000;
/// The most time scale `set_speed` and `set_physics_speed` take: `globalslow`'s ceiling
/// (`cmd_globalslow` 0xc5580 clamps to [0, 2]).
pub const MAX_SCALE: f32 = 2.0;

/// A validated setter call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Cash(i32),
    FreeSlots(i32),
    Award(u8),
    /// `bits` 0 stops the running power-up.
    Powerup {
        bits: u16,
        secs: f32,
    },
    /// `secs` 0 ends the running one.
    Speed {
        amount: f32,
        aura: f32,
        secs: f32,
    },
    /// `None`: back to TS's own `TSSlowMo` amount.
    PhysicsSpeed(Option<f32>),
    WeaponAmmo {
        id: u8,
        clip: Option<i32>,
        ammo: Option<i32>,
    },
}

impl Change {
    /// `set_cash`: `GiveCash`'s range, 0 to 30000.
    pub fn cash(cash: c_int) -> Result<Self, c_int> {
        (0..=MAX_CASH).contains(&cash).then_some(Change::Cash(cash)).ok_or(INVALID)
    }

    /// `set_free_slots`: 0 to 81, the free weight of an empty inventory (`TSInit`'s `pl+0x548`).
    pub fn free_slots(slots: c_int) -> Result<Self, c_int> {
        (0..=shared::ts::SLOTS_TOTAL).contains(&slots).then_some(Change::FreeSlots(slots)).ok_or(INVALID)
    }

    /// `set_award`: any value of the byte `TSPAward` carries.
    pub fn award(award: c_int) -> Result<Self, c_int> {
        u8::try_from(award).map(Change::Award).map_err(|_| INVALID)
    }

    /// `run_powerup`: 0, or one power-up that runs for a time (`powerup::RUNNABLE`) for a positive
    /// number of seconds (negative: its normal length, `powerup::normal_secs`).
    pub fn powerup(bits: c_int, secs: f32) -> Result<Self, c_int> {
        if bits == 0 {
            return Ok(Change::Powerup { bits: 0, secs: 0.0 });
        }
        let bits = u16::try_from(bits).ok().filter(|&b| powerup::runnable(b)).ok_or(INVALID)?;
        let secs = if secs < 0.0 { powerup::normal_secs(bits).ok_or(INVALID)? } else { secs };
        (secs.is_finite() && secs > 0.0).then_some(Change::Powerup { bits, secs }).ok_or(INVALID)
    }

    /// `set_speed`: a time scale of 0 (frozen, TS's floor of 0.01) to 2, an aura and a duration
    /// that aren't negative.
    pub fn speed(amount: f32, aura: f32, secs: f32) -> Result<Self, c_int> {
        let ok = (0.0..=MAX_SCALE).contains(&amount) && aura >= 0.0 && aura.is_finite() && secs >= 0.0 && secs.is_finite();
        ok.then_some(Change::Speed { amount, aura, secs }).ok_or(INVALID)
    }

    /// `set_physics_speed`: up to 2; 0 or below pauses (the client's slowest, 0.01: it reads a
    /// `TSSlowMo` of 0 as 1), and 1 hands the amount back to TS.
    pub fn physics_speed(speed: f32) -> Result<Self, c_int> {
        if speed.is_nan() || speed > MAX_SCALE {
            return Err(INVALID);
        }
        Ok(Change::PhysicsSpeed(if speed == 1.0 { None } else { Some(speed.max(0.01)) }))
    }

    /// `set_weapon_ammo`: a TS weapon id (1 to 37, not kung fu), a clip of 0 to 255 (the byte of
    /// `WStatus` / `WeaponInfo`) and a reserve of 0 to 32767 (`WeaponInfo`'s short); negative
    /// leaves that count alone. Whether the player owns it is checked when it runs.
    pub fn weapon_ammo(weapon: c_int, clip: c_int, ammo: c_int) -> Result<Self, c_int> {
        let id = u8::try_from(weapon).ok().filter(|&id| id != 0 && shared::weapons::info(id).is_some()).ok_or(INVALID)?;
        let count = |v: c_int, max: c_int| {
            if v < 0 {
                Ok(None)
            } else if v <= max {
                Ok(Some(v))
            } else {
                Err(INVALID)
            }
        };
        Ok(Change::WeaponAmmo { id, clip: count(clip, 255)?, ammo: count(ammo, 32767)? })
    }
}

/// Makes `c` on the player being dispatched, then refreshes its snapshot. False when the change
/// turned out impossible (a weapon the player doesn't own).
fn apply(p: &mut Player, cx: &mut Cx, c: Change) -> bool {
    // A plugin's private-data writes came before this call: take them back first.
    crate::pdata::sync(p, cx.edict);
    let done = match c {
        Change::Cash(cash) => {
            p.cash = cash;
            true
        }
        Change::FreeSlots(slots) => {
            crate::weapons::set_free_slots(p, cx, slots);
            true
        }
        Change::Award(award) => {
            p.ts.award = award;
            // `PlayerKilled`'s broadcast; the cache keeps `UpdateClientData` from repeating it.
            p.hud.award = award;
            rules::lifecycle::send_award(cx.index(), award);
            true
        }
        Change::Powerup { bits, secs } => {
            powerup::run_powerup(p, cx, bits, secs);
            true
        }
        Change::Speed { amount, aura, secs } => {
            powerup::set_plugin_speed(p, cx.time(), amount, aura, secs);
            true
        }
        Change::PhysicsSpeed(v) => {
            p.ts.physics_speed = v;
            true
        }
        Change::WeaponAmmo { id, clip, ammo } => crate::weapons::set_weapon_ammo(&mut p.inv, id, clip, ammo),
    };
    crate::pdata::sync(p, cx.edict);
    refresh(p, cx.edict);
    done
}

// ---- reaching players -----------------------------------------------------------------------

/// How a setter reached its player.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reach {
    /// Changed now.
    Now,
    /// Can't be done (a weapon the player doesn't own), or the player went away.
    Refused,
    /// Queued until the player's own callback returns.
    Later,
}

/// What the status logic needs from the game, so it can be tested without an engine.
trait Host {
    /// The user id of the connected player at entity index `player`, `None` for anything else.
    fn user_id(&self, player: c_int) -> Option<i32>;
    /// The player's state, if its object can be reached now.
    fn read(&self, player: c_int) -> Option<PlayerInfo>;
    /// What can be told from the engine alone (the time scale), for a player that can't be
    /// reached and has no snapshot yet.
    fn bare(&self, player: c_int) -> PlayerInfo;
    fn apply(&self, player: c_int, c: Change) -> Reach;
}

/// The setters' status: `NOT_PLAYER` first, then `INVALID` for the arguments, then how it went.
fn set(host: &impl Host, player: c_int, c: Result<Change, c_int>) -> c_int {
    if host.user_id(player).is_none() {
        return NOT_PLAYER;
    }
    let c = match c {
        Ok(c) => c,
        Err(e) => return e,
    };
    match host.apply(player, c) {
        Reach::Now => OK,
        Reach::Later => DEFERRED,
        Reach::Refused => INVALID,
    }
}

/// `player_info`: the live state, else the snapshot, else what the engine knows.
fn info(host: &impl Host, snaps: &RefCell<Snapshots>, player: c_int) -> Result<PlayerInfo, c_int> {
    let uid = host.user_id(player).ok_or(NOT_PLAYER)?;
    if let Some(i) = host.read(player) {
        snaps.borrow_mut().put(player, uid, i);
        return Ok(i);
    }
    let snap = snaps.borrow().get(player, uid);
    Ok(snap.unwrap_or_else(|| host.bare(player)))
}

/// The engine and the slab.
struct Game;

impl Game {
    fn edict(player: c_int) -> *mut edict_t {
        let eng = engine();
        if player < 1 || player > eng.max_clients() {
            return std::ptr::null_mut();
        }
        let e = eng.player(player);
        if rules::is_player(e) { e } else { std::ptr::null_mut() }
    }
}

impl Host for Game {
    fn user_id(&self, player: c_int) -> Option<i32> {
        let e = Self::edict(player);
        (!e.is_null()).then(|| engine().user_id(e))
    }

    fn read(&self, player: c_int) -> Option<PlayerInfo> {
        let e = Self::edict(player);
        world::with_lent::<Player, _>(e, |p| snapshot(p, e.v()))
    }

    fn bare(&self, player: c_int) -> PlayerInfo {
        PlayerInfo { time_scale: Self::edict(player).v().fuser1, ..PlayerInfo::default() }
    }

    fn apply(&self, player: c_int, c: Change) -> Reach {
        let e = Self::edict(player);
        let job = move || crate::player::with_player(e, |p, cx| apply(p, cx, c));
        match world::run_now(e, job) {
            Some(Some(true)) => Reach::Now,
            Some(_) => Reach::Refused,
            None => {
                world::defer(e, move || {
                    // The player may have left meanwhile; the slot's next client isn't theirs.
                    if rules::is_player(e) {
                        job();
                    }
                });
                Reach::Later
            }
        }
    }
}

// ---- snapshots ------------------------------------------------------------------------------

/// The last state of each player slot, with the user id it belongs to (a slot's next client
/// doesn't see the last one's).
pub struct Snapshots<T = PlayerInfo>(Vec<Option<(i32, T)>>);

impl<T> Default for Snapshots<T> {
    fn default() -> Self {
        Snapshots(Vec::new())
    }
}

impl<T: Clone> Snapshots<T> {
    fn put(&mut self, player: c_int, uid: i32, info: T) {
        let Ok(i) = usize::try_from(player) else { return };
        if self.0.len() <= i {
            self.0.resize(i + 1, None);
        }
        self.0[i] = Some((uid, info));
    }

    fn get(&self, player: c_int, uid: i32) -> Option<T> {
        let i = usize::try_from(player).ok()?;
        self.0.get(i).cloned().flatten().filter(|(u, _)| *u == uid).map(|(_, info)| info)
    }
}

static SNAPSHOTS: Global<RefCell<Snapshots>> = Global(RefCell::new(Snapshots(Vec::new())));
static INVENTORIES: Global<RefCell<Snapshots<Vec<WeaponEntry>>>> = Global(RefCell::new(Snapshots(Vec::new())));
/// What plugins were last told of each player's weapon: (weapon, clip, ammo).
static TOLD: Global<RefCell<Snapshots<(c_int, c_int, c_int)>>> = Global(RefCell::new(Snapshots(Vec::new())));

/// The weapons a player owns, by id, as `player_inventory` lists them.
pub fn inventory(p: &Player) -> Vec<WeaponEntry> {
    let gun = &p.inv.gun;
    (1..shared::weapons::MAX_WEAPONS as u8)
        .filter(|&id| gun.status[id as usize].owned)
        .map(|id| {
            let s = &gun.status[id as usize];
            let ammo = shared::weapons::info(id).map_or(0, |w| w.ammo);
            WeaponEntry {
                weapon: id as c_int,
                clip: s.clip,
                ammo: if ammo != 0 { gun.reserve.get(ammo as usize).copied().unwrap_or(0) } else { 0 },
                fire_mode: s.mode as c_int,
                attachments: s.items as c_int,
            }
        })
        .collect()
}

/// The player's state as the API reports it. `v` is the player's `entvars_t`.
pub fn snapshot(p: &Player, v: &entvars_t) -> PlayerInfo {
    let gun = &p.inv.gun;
    // What `WeaponInfo` carries (`CTSGun::UpdateClientData` 0x858c8): with kung fu, the bar.
    let (clip, ammo, fire_mode, attachments) = match gun.slot() {
        Some(s) => (s.clip, gun.current_reserve(), s.mode as c_int, s.items as c_int),
        None => (gun.melee_bar.round_ties_even().clamp(0.0, 255.0) as c_int, 0, 0, 0),
    };
    let team = if rules::state::teamplay() { rules::state::with(|g| g.team_index(&p.ts.team_name)) + 1 } else { 0 };
    PlayerInfo {
        joined: p.joined as c_int,
        alive: (rules::alive(v) && v.iuser1 == 0) as c_int,
        is_bot: (p.is_bot != 0) as c_int,
        team: team.max(0),
        has_flag: p.ts.flag.is_some() as c_int,
        cash: p.cash,
        free_slots: p.inv.sv.free_slots,
        award: p.ts.award as c_int,
        kill_streak: p.ts.spree as c_int,
        active_powerup: p.active_powerup as c_int,
        powerup_until: if p.active_powerup != 0 { p.powerup_until } else { 0.0 },
        stored_powerup: p.ts.held as c_int,
        stored_powerup_value: p.ts.held_secs,
        kungfu: p.kungfu as c_int,
        superjump: p.superjump as c_int,
        time_scale: v.fuser1,
        stunt_state: p.stunt.stance as c_int,
        weapon: gun.current as c_int,
        clip,
        ammo,
        fire_mode,
        attachments,
    }
}

/// Stores the snapshot of the player `e` (from PostThink and the setters, which hold it).
pub fn refresh(p: &Player, e: *mut edict_t) {
    let eng = engine();
    let (player, uid) = (eng.index_of(e), eng.user_id(e));
    let info = snapshot(p, e.v());
    SNAPSHOTS.0.borrow_mut().put(player, uid, info);
    INVENTORIES.0.borrow_mut().put(player, uid, inventory(p));
}

/// StartFrame: the snapshot of every player that can be reached, then the plugins' weapon and
/// ammo callbacks for what changed since they were last told (port addition: TS 3.0 never sends
/// bots their weapon messages, so plugins can't sniff them).
pub fn start_frame() {
    let tell = crate::plugins::any();
    for player in 1..=engine().max_clients() {
        let Some(uid) = Game.user_id(player) else { continue };
        let e = Game::edict(player);
        // The private-data mirror (`pdata`): plugins' writes since the player's PostThink.
        crate::player::with_player(e, |p, cx| crate::pdata::sync(p, cx.edict));
        if let Some((info, inv)) = world::with_lent::<Player, _>(e, |p| (snapshot(p, e.v()), inventory(p))) {
            SNAPSHOTS.0.borrow_mut().put(player, uid, info);
            INVENTORIES.0.borrow_mut().put(player, uid, inv);
        }
        if tell && let Some(info) = SNAPSHOTS.0.borrow().get(player, uid) {
            let now = (info.weapon, info.clip, info.ammo);
            let before = TOLD.0.borrow().get(player, uid);
            if before != Some(now) {
                TOLD.0.borrow_mut().put(player, uid, now);
                if before.is_none_or(|b| b.0 != now.0) {
                    crate::plugins::weapon_changed(player, now.0);
                }
                crate::plugins::ammo_changed(player, now.0, now.1, now.2);
            }
        }
    }
}

// ---- the table's functions --------------------------------------------------------------------

/// `PluginApi::player_info`.
pub fn player_info(player: c_int) -> Result<PlayerInfo, c_int> {
    info(&Game, &SNAPSHOTS.0, player)
}

/// `PluginApi::player_inventory`: the live inventory, else the snapshot's (empty before the
/// first).
pub fn player_inventory(player: c_int) -> Result<Vec<WeaponEntry>, c_int> {
    let uid = Game.user_id(player).ok_or(NOT_PLAYER)?;
    let e = Game::edict(player);
    if let Some(inv) = world::with_lent::<Player, _>(e, inventory) {
        INVENTORIES.0.borrow_mut().put(player, uid, inv.clone());
        return Ok(inv);
    }
    Ok(INVENTORIES.0.borrow().get(player, uid).unwrap_or_default())
}

/// The connected player at index `player`, or a null pointer (for the other API modules).
pub(crate) fn player_edict(player: c_int) -> *mut edict_t {
    Game::edict(player)
}

/// Runs `job` on the connected player `player` now if it can be reached, else when the callback
/// holding it returns (as the setters do): `OK`, `DEFERRED`, `NOT_PLAYER`, or `INVALID` when `job`
/// returns false (it found the change impossible).
pub(crate) fn on_player(player: c_int, job: impl FnOnce(&mut Player, &mut Cx) -> bool + 'static) -> c_int {
    let e = Game::edict(player);
    if e.is_null() {
        return NOT_PLAYER;
    }
    let mut job = Some(job);
    let now = world::run_now(e, || crate::player::with_player(e, |p, cx| (job.take().expect("once"))(p, cx)));
    match now {
        Some(Some(true)) => OK,
        Some(Some(false)) => INVALID,
        Some(None) => NOT_PLAYER,
        None => {
            let job = job.take().expect("not run");
            world::defer(e, move || {
                // The player may have left meanwhile; the slot's next client isn't theirs.
                if rules::is_player(e) {
                    crate::player::with_player(e, job);
                }
            });
            DEFERRED
        }
    }
}

/// `PluginApi::set_cash`.
pub fn set_cash(player: c_int, cash: c_int) -> c_int {
    set(&Game, player, Change::cash(cash))
}

/// `PluginApi::set_free_slots`.
pub fn set_free_slots(player: c_int, slots: c_int) -> c_int {
    set(&Game, player, Change::free_slots(slots))
}

/// `PluginApi::set_award`.
pub fn set_award(player: c_int, award: c_int) -> c_int {
    set(&Game, player, Change::award(award))
}

/// `PluginApi::run_powerup`.
pub fn run_powerup(player: c_int, bits: c_int, secs: f32) -> c_int {
    set(&Game, player, Change::powerup(bits, secs))
}

/// `PluginApi::set_speed`.
pub fn set_speed(player: c_int, speed: f32, aura: f32, secs: f32) -> c_int {
    set(&Game, player, Change::speed(speed, aura, secs))
}

/// `PluginApi::set_physics_speed`.
pub fn set_physics_speed(player: c_int, speed: f32) -> c_int {
    set(&Game, player, Change::physics_speed(speed))
}

/// `PluginApi::set_bullet_trail`: TS 3.0 has no per-player bullet trails. Its client draws trails
/// only on the client-side bullets it simulates under slow motion or with the server's
/// `mp_realbullet` (`SrvSett` key 0, a server-wide rule: `EV_FireBullets`' `0x1018d16c` test,
/// client rendering-effects-prediction.md §4.4); TS 2.x's per-player flag is gone.
pub fn set_bullet_trail(player: c_int, _on: c_int) -> c_int {
    if Game.user_id(player).is_none() { NOT_PLAYER } else { UNSUPPORTED }
}

/// `PluginApi::set_weapon_ammo`.
pub fn set_weapon_ammo(player: c_int, weapon: c_int, clip: c_int, ammo: c_int) -> c_int {
    set(&Game, player, Change::weapon_ammo(weapon, clip, ammo))
}

/// `PluginApi::projectile_owner`: the thrower of a grenade, knife or thrown gun
/// (`weapons::projectiles::thrower_of`), else the entity's `pev->owner` when that is a connected
/// player (a held `weapon_tsgun`, a placed entity's creator), else 0. `NOT_PLAYER` for an index
/// that isn't a game entity (free, out of range, or without the game's private data).
pub fn projectile_owner(entity: c_int) -> c_int {
    let eng = engine();
    if entity < 0 || entity >= eng.globals().maxEntities {
        return NOT_PLAYER;
    }
    let e = eng.entity_of_index(entity);
    if e.is_null() || e.is_free() || !e.has_private_data() {
        return NOT_PLAYER;
    }
    let owner = crate::weapons::projectiles::thrower_of(e).unwrap_or_else(|| {
        let o = e.v().owner;
        if rules::is_player(o) { o } else { std::ptr::null_mut() }
    });
    if owner.is_null() { 0 } else { eng.index_of(owner) }
}

/// `PluginApi::private_size` (version 5): the bytes of a game entity's private block, `NOT_PLAYER`
/// for an index that is out of range, free, or has no game private data (as
/// [`projectile_owner`]).
pub fn private_size(entity: c_int) -> c_int {
    let eng = engine();
    if entity < 0 || entity >= eng.globals().maxEntities {
        return NOT_PLAYER;
    }
    let e = eng.entity_of_index(entity);
    if e.is_null() || e.is_free() || !e.has_private_data() {
        return NOT_PLAYER;
    }
    c_int::try_from(goldsrc::edict::private_size(e)).unwrap_or(c_int::MAX)
}

#[cfg(test)]
mod tests;
