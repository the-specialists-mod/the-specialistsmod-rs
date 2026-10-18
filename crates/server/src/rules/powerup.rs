//! Player power-ups and slow motion: `PlayerAddPowerUp` (0x97bc8), `UsePowerUp` (0x95980),
//! `UpdatePowerUpState` (0x95748), `SlowMotionFlow` (0x96bb0), `DeactivateSlowMotionFlow`
//! (0x96a9c), `GoSlow` (0x929d0), `InterpolateSlowMotion` (0x9910c).
//! Reference: player-movement.md §5, gamerules-entities.md §4.

use super::msg::{self, ts_message_one};
use super::{alive, eye, other, state};
use crate::messages::Msg;
use crate::player::Player;
use crate::world::Cx;
use goldsrc::edict::EdictPtr;
use goldsrc::server::{IGNORE_MONSTERS, engine};
use goldsrc::sys::*;
use shared::pm::{Fpu, Num};
use shared::protocol::hud_wire::{PwUp, PwUpTime, TsMBlur};
use shared::ts::powerup as pw;

/// The power-up values a gameplay ruleset sets (`ServerRules::powerups`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PowerupRules {
    /// `SetupPowerUp` (0x98014): the base duration of each type, by bit (`1 << i`, the lowest
    /// set bit counts), and whether a map power-up's `pwupmult` scales it.
    pub base_secs: [i32; 9],
    pub multiplier: bool,
    /// `WaitTouch`: the types picked up whatever is held or running (`0x1e6`).
    pub instant: u16,
    /// `PlayerAddPowerUp` (0x97bc8): infinite clip is used at once (`CTSGun::MultiplyClips`);
    /// otherwise it is held and used like slow motion, and the gun reads the running bit.
    pub infinite_clip_instant: bool,
    /// `WaitTouch`: a slow motion tops up a held one with fewer seconds than this.
    pub slowmo_top_up_below: i32,
    /// `WaitTouch`: +use swaps a held slow motion for the pickup only above these seconds.
    pub slowmo_swap_above: i32,
    /// `WaitTouch`: +use swaps the held power-up for an instant one too (the held one drops,
    /// and the player keeps it).
    pub swap_on_instant: bool,
    /// `UpdatePowerUpState`: a dead player's slow motion drops only above these seconds.
    pub slowmo_drop_above: i32,
    /// `UpdatePowerUpState`: (self, others, bullet-time flag) while slow motion / slow pause runs.
    pub slowmo: (f32, f32, bool),
    pub slowpause: (f32, f32, bool),
    /// `UpdateClientData`: the `TSSlowMo` amount while slow motion / slow pause runs.
    pub hud_slowmo: f32,
    pub hud_slowpause: f32,
    /// `UpdateClientData`: normal speed (an amount of 1) goes out as `TSSlowMo` 0, the "not
    /// slowed" value a client that reads 0 as normal expects (TS 2.1's icon, issue #324).
    pub hud_normal_as_zero: bool,
    /// `PlayerAddPowerUp` (0x97bc8): the grenade power-up's `PickUpWeapon(0x18, NULL, 0, select,
    /// 0)` draws the grenades (`select` 1); otherwise only a player on kung fu draws them.
    pub grenade_selects: bool,
}

impl PowerupRules {
    /// The base duration of `kind`: its lowest known bit's, 0 for none.
    pub fn base(&self, kind: u16) -> i32 {
        (0..9).find(|i| kind & (1 << i) != 0).map_or(0, |i| self.base_secs[i])
    }
}

/// Sets a player's time scale target (`GoSlow`); a ruleset whose time scale doesn't ease writes
/// `fuser1` at once ([`ServerRules::time_scale_eases`](crate::gameplay::ServerRules::time_scale_eases)).
fn set_slow_target(p: &mut Player, e: *mut edict_t, target: f32) {
    p.slow_target = target;
    if !crate::gameplay::active().server.time_scale_eases() {
        (e.v().fuser1 = target);
    }
}

/// `InterpolateSlowMotion`: exponential approach at ~7/s, snapping within 0.05, in the server's
/// arithmetic ([`pm_arith`](super::cvars::pm_arith)).
pub fn interpolate(frametime: f32, cur: &mut f32, target: f32) {
    *cur = shared::by_arith!(super::cvars::pm_arith(), interpolate_in(frametime, *cur, target));
}

/// `InterpolateSlowMotion` (`ts_i386.so` 0x9910c, `mp.dll` 0x10067030): `d = target - cur`,
/// `d * min(ft * 7, 1)` and the sum stay on the x87 and are rounded once, when `cur` is stored
/// (issue #229). The 0.05 and 7 are `float`s. GCC keeps `ft * 7` unless it is above 1 (a NaN
/// stays); MSVC takes 1 when `1 < ft * 7` (a NaN takes 1).
fn interpolate_in<F: Fpu>(frametime: f32, cur: f32, target: f32) -> f32 {
    let c = F::x(cur);
    let mut d = F::x(target) - c;
    if !F::cmp_le(d.abs(), F::x(0.05)) {
        let k = F::x(frametime) * F::x(7.0);
        let one = F::x(1.0);
        let k = if F::MSVC {
            if F::cmp_lt(one, k) { one } else { k }
        } else if F::cmp_le(k, one) {
            k
        } else {
            one
        };
        d = d * k;
    }
    (d + c).f()
}

/// `CBasePlayer::GoSlow` on the dispatched player, a virtual call the plugins' hook sees
/// ([`crate::ham::player_go_slow`]).
pub fn go_slow_self(p: &mut Player, cx: &mut Cx, amount: f32, btime: bool) {
    crate::ham::player_go_slow(p, cx, amount, btime, go_slow_self_plain);
}

/// [`go_slow_self`] without the plugins' hooks.
fn go_slow_self_plain(p: &mut Player, cx: &mut Cx, amount: f32, btime: bool) {
    let target = if cx.pev().iuser1 != 0 { 1.0 } else { amount };
    set_slow_target(p, cx.edict, target);
    p.ts.slow_display = amount;
    p.ts.btime = btime;
    crate::gameplay::active().server.on_go_slow(p, amount);
}

/// `GoSlow` on any entity: players through their Rust state, others directly (`fuser1`).
pub fn go_slow(e: *mut edict_t, amount: f32, btime: bool) {
    if e.is_null() {
        return;
    }
    crate::ham::go_slow(e, amount, btime, go_slow_plain);
}

/// [`go_slow`] without the plugins' hooks: `CBasePlayer::GoSlow` (0x929d0) or `CBaseEntity`'s
/// (0xac5e0).
pub fn go_slow_plain(e: *mut edict_t, amount: f32, btime: bool) {
    let observer = e.v().iuser1 != 0;
    let done = other(e, |p| {
        set_slow_target(p, e, if observer { 1.0 } else { amount });
        p.ts.slow_display = amount;
        p.ts.btime = btime;
        crate::gameplay::active().server.on_go_slow(p, amount);
    });
    if done.is_none() && !super::has_player(e) {
        (e.v().fuser1 = amount);
    }
}

/// `InSlow` for players: target or displayed amount differs from 1.
pub fn in_slow(p: &Player) -> bool {
    p.slow_target != 1.0 || p.ts.slow_display != 1.0
}

/// `InSlow` for any entity.
pub fn entity_in_slow(e: *mut edict_t) -> bool {
    other(e, |p| in_slow(p)).unwrap_or_else(|| e.v().fuser1 != 1.0)
}

/// `UpdatePowerUpState`: activator amounts, expiry, dropping on death.
///
/// Port addition: a power-up a plugin runs ([`run_powerup`], `PlayerRules::plugin_powerup`) runs
/// through the same code although nothing is held, and its end clears only the running one: the
/// held power-up and its seconds stay, and nothing drops.
pub fn update_state(p: &mut Player, cx: &mut Cx) {
    // Whatever cleared the running power-up (a spawn, an LMS round) ended the plugin's run too.
    p.ts.plugin_powerup &= p.active_powerup != 0;
    if p.ts.held == 0 && !p.ts.plugin_powerup {
        return;
    }
    let now = cx.time();
    let is_alive = alive(cx.pev());
    let continuous = matches!(state::with(|g| g.mode), state::Mode::Dm | state::Mode::TeamDm);
    let rules = crate::gameplay::active().server.powerups();
    let drop_worthy = |bits: u16, secs: i32| bits & pw::SLOWMO == 0 || secs > rules.slowmo_drop_above;
    if p.active_powerup == 0 {
        if is_alive {
            return;
        }
        if continuous && drop_worthy(p.ts.held, p.ts.held_secs) {
            drop_powerup(cx.pev().origin, p.ts.held);
        }
    } else {
        let active = p.active_powerup;
        if active & (pw::SLOWMO | pw::SLOWPAUSE) == 0 {
            if active & pw::SUPERJUMP != 0 {
                cx.pev().gravity = 0.15;
            }
        } else if p.powerup_until < now || !is_alive {
            deactivate_flow(p, cx);
        } else if active & pw::SLOWPAUSE != 0 {
            (p.ts.slow_self, p.ts.slow_others, p.ts.btime) = rules.slowpause;
        } else {
            (p.ts.slow_self, p.ts.slow_others, p.ts.btime) = rules.slowmo;
        }
        if now <= p.powerup_until && is_alive {
            return;
        }
        if !is_alive && continuous && !p.ts.plugin_powerup && drop_worthy(active, p.ts.held_secs) {
            drop_powerup(cx.pev().origin, active);
        }
        if active & pw::SUPERJUMP != 0 {
            cx.pev().gravity = 1.0;
        }
        if std::mem::take(&mut p.ts.plugin_powerup) {
            p.powerup_until = 0.0;
            p.active_powerup = 0;
            return;
        }
    }
    p.powerup_until = 0.0;
    p.ts.held_secs = 0;
    p.active_powerup = 0;
    p.ts.held = 0;
}

/// The seconds a dead player's power-up drops with: `UpdatePowerUpState`'s
/// `CreatePowerUp(type, -1)` (3.0 0x858a8 and 0x8592b; 2.1 0x100569db and 0x10056a4a), so
/// `SetupPowerUp` gives it the type's base duration whatever the player held (issue #245).
const DEATH_DROP_SECS: i32 = -1;

/// Drops a dead player's power-up as a temporary `ts_powerup` (60 s life) with the base duration.
pub fn drop_powerup(origin: [f32; 3], bits: u16) {
    crate::entities::ts::create_powerup(bits, DEATH_DROP_SECS, origin);
}

/// `SlowMotionFlow`'s line-of-sight traces (0x86d4c, 0x86f63, 0x870c2, 0x87221; 2.1 the same)
/// call the glass overload `UTIL_TraceLine(.., ignore_monsters, ignore_glass, ..)` with both
/// set: `IGNORE_MONSTERS | 0x100`, so players and projectiles behind glass are in sight.
const FLOW_TRACE: i32 = IGNORE_MONSTERS | 0x100;

/// `m_iSlowedEnts` (`pl+0x288`): the slowed-projectile list holds 32.
const SLOWED_ENTS_MAX: usize = 32;

/// A projectile the flow has just slowed is listed only while the list has room (0x86f94: `if
/// (count <= 31) array[count++] = ent`); past that it stays slowed but unlisted, so
/// `DeactivateSlowMotionFlow` never resets it.
fn list_slowed(list: &mut Vec<*mut edict_t>, e: *mut edict_t) {
    if list.len() < SLOWED_ENTS_MAX {
        list.push(e);
    }
}

/// `SlowMotionFlow` (0x96bb0): every 0.5 s an activator slows every player and projectile it can
/// see.
pub fn slow_flow(p: &mut Player, cx: &mut Cx) {
    let now = cx.time();
    if p.ts.slow_self == 0.0 || p.ts.next_flow > now {
        return;
    }
    p.ts.next_flow = now + 0.5;
    let eng = engine();
    let me = cx.edict;
    let my_eye = eye(cx.pev());
    let mine = p.ts.slow_others;
    let aura = p.ts.plugin_speed.map(|s| s.aura);
    let mut abort = false;
    // `UTIL_PlayerByIndex` and `IsPlayer()`: players who left are slowed too (TS keeps their
    // object; the slowed list holds the slot).
    for i in 1..=eng.max_clients().min(32) {
        let o = eng.player(i);
        let i = i as usize;
        if p.ts.slowed & (1 << i) != 0 {
            // Someone we slowed became an activator: re-evaluate.
            let activator = !o.is_null() && other(o, |q| q.ts.slow_self != 0.0).unwrap_or(false);
            if !activator {
                continue;
            }
            p.ts.slowed &= !(1 << i);
        }
        if o.is_null() || o == me || !super::has_player(o) {
            continue;
        }
        // Port addition: a plugin's slow reaches only as far as its aura (`plugin_speed_think`).
        if aura.is_some_and(|a| super::dist(my_eye, eye(o.v())) > a) {
            continue;
        }
        let Some((oself, oothers)) = other(o, |q| (q.ts.slow_self, q.ts.slow_others)) else { continue };
        if oself != 0.0 && oothers == mine {
            continue;
        }
        let tr = eng.trace_line(my_eye, eye(o.v()), FLOW_TRACE, me);
        if tr.flFraction >= 1.0 {
            if oself == 0.0 || mine <= oothers {
                p.ts.slowed |= 1 << i;
            } else {
                // A stronger slow wins.
                p.ts.slowed &= !(1 << i);
                abort = true;
                break;
            }
        }
    }
    if abort {
        return;
    }
    let (self_amt, btime) = (p.ts.slow_self, p.ts.btime);
    go_slow_self(p, cx, self_amt, btime);
    for i in 1..=eng.max_clients().min(32) {
        if p.ts.slowed & (1 << i) == 0 {
            continue;
        }
        let o = eng.player(i);
        let keep = !o.is_null() && other(o, |q| q.ts.slow_self == 0.0 || q.ts.slow_others != mine).unwrap_or(false);
        if keep {
            go_slow(o, mine, btime);
        } else {
            p.ts.slowed &= !(1 << i);
        }
    }
    // Projectiles in sight: grenades, thrown knives and dropped guns that are moving. The list's
    // room is checked once, before the three loops (0x86ea4); a full list doesn't stop the
    // slowing ([`list_slowed`]).
    if p.ts.slowed_ents.len() >= SLOWED_ENTS_MAX {
        return;
    }
    for class in [c"grenade", c"knife", c"WorldGun"] {
        let mut it = std::ptr::null_mut();
        loop {
            it = eng.find_by_string(it, c"classname", class);
            if it.is_null() {
                break;
            }
            let v = it.v();
            if class != c"grenade" && v.velocity == [0.0; 3] {
                continue;
            }
            if p.ts.slowed_ents.contains(&it) || aura.is_some_and(|a| super::dist(my_eye, v.origin) > a) {
                continue;
            }
            let tr = eng.trace_line(my_eye, v.origin, FLOW_TRACE, me);
            if tr.flFraction >= 1.0 {
                go_slow(it, mine, false);
                list_slowed(&mut p.ts.slowed_ents, it);
            }
        }
    }
}

/// `RemoveEntityFromSlowedArray` (0x9fb4c): every player's slowed-entity entries for `e` are
/// cleared (the count stays, as in TS).
pub fn remove_from_slowed(e: *mut edict_t) {
    for o in super::slot_players() {
        other(o, |q| {
            for s in q.ts.slowed_ents.iter_mut().filter(|s| **s == e) {
                *s = std::ptr::null_mut();
            }
        });
    }
}

/// `DeactivateSlowMotionFlow`: back to normal time for us and everything we slowed. A slowed
/// player who isn't an activator himself is released through his virtual `GoSlow(1, 0)` (3.0
/// 0x96a9c, 2.1 FUN_10057610's `GoSlow(0, 0)`), so the plugins' hook and the ruleset's release
/// ([`ServerRules::on_go_slow`](crate::gameplay::ServerRules::on_go_slow)) run for him too
/// (issue #223).
pub fn deactivate_flow(p: &mut Player, cx: &mut Cx) {
    go_slow_self(p, cx, 1.0, false);
    p.ts.slow_self = 0.0;
    p.ts.slow_others = 0.0;
    p.ts.btime = false;
    let eng = engine();
    for i in 1..=eng.max_clients().min(32) {
        if p.ts.slowed & (1 << i) == 0 {
            continue;
        }
        p.ts.slowed &= !(1 << i);
        let o = eng.player(i);
        if o.is_null() {
            continue;
        }
        if other(o, |q| q.ts.slow_self == 0.0) == Some(true) {
            go_slow(o, 1.0, false);
        }
    }
    for e in std::mem::take(&mut p.ts.slowed_ents) {
        // The entity may be gone; only touch edicts that are still in use.
        if !e.is_null() && !e.is_free() {
            go_slow(e, 1.0, false);
        }
    }
}

/// `UsePowerUp` (0x95980): activate the held power-up. Only the `usepowerup` command (which the
/// client sends for a +use double tap) and the bots call it; TS's server-side `TSUse` is dead.
pub fn use_powerup(p: &mut Player, cx: &mut Cx) -> bool {
    if cx.pev().iuser1 != 0 || !alive(cx.pev()) || p.ts.held == 0 || p.active_powerup != 0 {
        return false;
    }
    let now = cx.time();
    if state::with(|g| g.slow_blocked) && p.ts.held & (pw::SLOWMO | pw::SLOWPAUSE) != 0 {
        ts_message_one(cx.edict, [195, 45, 45], 16, 20, "You can't use Slow-Motion now!");
        return false;
    }
    p.active_powerup = p.ts.held;
    p.powerup_until = now + p.ts.held_secs as f32;
    // `(int)(m_flPowerUpTime - time)`, truncated (0x95a88), of which `WriteByte` sends the low
    // byte: the `f32` end time can make it a second short of the held seconds.
    let secs = super::x87_trunc(p.powerup_until as f64 - now as f64) as u8;
    msg::one(Msg::PwUpTime, cx.edict, |w| PwUpTime { secs }.write(w));
    let mode = if p.ts.held & pw::SLOWMO != 0 {
        1
    } else if p.ts.held & pw::SLOWPAUSE != 0 {
        2
    } else {
        0
    };
    if mode != 0 && crate::gameplay::active().server.motion_blur() {
        let b = TsMBlur { entindex: cx.index() as u8, mode };
        msg::pvs(Msg::TsMBlur, cx.pev().origin, |w| b.write(w));
    }
    // Port addition (plugin API version 5): the plugins' `player_powerup`.
    crate::plugins::player_powerup(cx.edict, p.ts.held as i32);
    true
}

/// `PlayerAddPowerUp`: apply a pickup. `secs` is the power-up entity's duration and `armor` the
/// armour it gives (100; [`ServerRules::armor_powerup`](crate::gameplay::ServerRules::armor_powerup)).
/// Returns false when the pickup failed (no gun to multiply, grenade didn't fit).
pub fn add_powerup(p: &mut Player, cx: &mut Cx, bits: u16, secs: i32, armor: f32) -> bool {
    let e = cx.edict;
    let r = crate::gameplay::active().server;
    let mut held = false;
    if bits & pw::ARMOR != 0 {
        r.armor_powerup(p, cx, armor);
    } else if bits & pw::KUNGFU != 0 {
        if !p.kungfu {
            p.kungfu = true;
            msg::one(Msg::PwUp, e, |w| PwUp { bits: pw::KUNGFU as i16, secs: 0 }.write(w));
        }
    } else if bits & pw::SUPERJUMP != 0 {
        if !p.superjump {
            p.superjump = true;
            engine().set_physics_key(e, c"mj", c"1");
            msg::one(Msg::PwUp, e, |w| PwUp { bits: pw::SUPERJUMP as i16, secs: 0 }.write(w));
        }
    } else if bits & pw::HEALTH != 0 {
        let v = cx.pev();
        v.health = v.health.max(150.0);
        p.ts.health_timer = cx.time() + 1.0;
    } else if bits & pw::GRENADE != 0 {
        if !crate::weapons::give_weapon_select(p, cx, 24, 0, r.powerups().grenade_selects) {
            return false;
        }
    } else if bits & pw::INFINITE_CLIP != 0 && r.powerups().infinite_clip_instant {
        if !crate::weapons::multiply_clip(p, cx) {
            return false;
        }
    } else {
        // Port addition: a power-up a plugin runs carries on (TS never reaches this with one
        // running: the held one is the running one, and a full slot refuses pickups).
        if !p.ts.plugin_powerup {
            p.active_powerup = 0;
            p.powerup_until = 0.0;
        }
        p.ts.held = bits;
        p.ts.held_secs = secs;
        held = true;
    }
    // `EMIT_SOUND_DYN(..., 0.5)` (call at 0x97de1): the pickup's pitch drops with the player's time scale.
    crate::entities::stock::explode::emit_sound_slow(e, CHAN_ITEM, c"items/gunpickup2.wav", 1.0, 0.8, 0, 100, 0.5);
    // Port addition (plugin API version 5): a power-up that works at pickup ran; a held one runs
    // when used (`use_powerup`).
    if !held {
        crate::plugins::player_powerup(e, bits as i32);
    }
    true
}

/// The `TSSlowMo` value: 0.225 under slow pause, 0.35 under slow motion (`rules`), else the
/// display amount. Port addition: a plugin's `set_physics_speed` (`PlayerRules::physics_speed`)
/// replaces it. Normal speed goes out as 0 under [`PowerupRules::hud_normal_as_zero`].
pub fn hud_slow_amount(p: &Player, rules: &PowerupRules) -> f32 {
    let v = hud_slow_raw(p, rules);
    if rules.hud_normal_as_zero && v == 1.0 { 0.0 } else { v }
}

fn hud_slow_raw(p: &Player, rules: &PowerupRules) -> f32 {
    if let Some(v) = p.ts.physics_speed {
        v
    } else if p.active_powerup & pw::SLOWPAUSE != 0 {
        rules.hud_slowpause
    } else if p.active_powerup & pw::SLOWMO != 0 {
        rules.hud_slowmo
    } else {
        p.ts.slow_display
    }
}

// ---- server plugins (port additions, `crate::plugin_api`) ---------------------------------------

/// A plugin's time scale for a player and everyone within `aura` units of them, until `until`
/// (`PlayerRules::plugin_speed`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PluginSpeed {
    /// The time scale (`fuser1` target), 0.01 to 2.
    pub amount: f32,
    pub aura: f32,
    pub until: f32,
}

/// Port addition (`set_speed`): the player becomes a slow-motion activator, as a slow-motion
/// power-up makes it (`UpdatePowerUpState` 0x95748), with `amount` for itself and for the others
/// (`pl+0x17c` / `pl+0x180`), but only for players and projectiles within `aura` units
/// ([`slow_flow`]). `secs` 0 ends a running one at the next think.
pub fn set_plugin_speed(p: &mut Player, now: f32, amount: f32, aura: f32, secs: f32) {
    if secs == 0.0 {
        if let Some(s) = p.ts.plugin_speed.as_mut() {
            s.until = 0.0;
        }
        return;
    }
    // `FixedSlowAmt` (0x990bc): TS never scales time below 0.01, and 0 means "no activator".
    p.ts.plugin_speed = Some(PluginSpeed { amount: amount.max(0.01), aura, until: now + secs });
    p.ts.next_flow = 0.0; // the first `SlowMotionFlow` on the next think, not up to 0.5 s later
}

/// Port addition: keeps a plugin's [`PluginSpeed`] going, between `UpdatePowerUpState` and
/// `SlowMotionFlow` in PreThink, so it wins over a slow-motion power-up's amounts while it lasts.
/// At its end (or the player's death) everything it slowed goes back to normal time, as
/// `DeactivateSlowMotionFlow` (0x96a9c) does for a power-up.
pub fn plugin_speed_think(p: &mut Player, cx: &mut Cx) {
    let Some(s) = p.ts.plugin_speed else { return };
    if s.until <= cx.time() || !alive(cx.pev()) {
        p.ts.plugin_speed = None;
        deactivate_flow(p, cx);
        return;
    }
    p.ts.slow_self = s.amount;
    p.ts.slow_others = s.amount;
    p.ts.btime = false;
}

/// The power-ups that run for a time once used: the stored ones (`STORED`), infinite clip and
/// superjump, whose running bits TS also reads (`InfiniteClips` 0x75fa8.., the 0.15 gravity of
/// `UpdatePowerUpState`).
pub const RUNNABLE: u16 = pw::STORED | pw::INFINITE_CLIP | pw::SUPERJUMP;

/// The seconds a power-up runs for when used: `SetupPowerUp`'s base duration (0x98014) with
/// `pwupmult` 1, as the automatic spawner's power-ups have, in the active ruleset. `None` for
/// those without one.
pub fn normal_secs(bits: u16) -> Option<f32> {
    let secs = crate::gameplay::active().server.powerups().base(bits);
    (runnable(bits) && secs > 0).then_some(secs as f32)
}

/// `bits` is one [`RUNNABLE`] power-up.
pub fn runnable(bits: u16) -> bool {
    RUNNABLE & bits == bits && bits.count_ones() == 1
}

/// Port addition (`run_powerup`): runs `bits` for `secs` seconds as `UsePowerUp` (0x95980) runs
/// the held one, without taking it: the held power-up stays for later. A power-up that was
/// running ends first (a used one is gone, as when it runs out). `bits` 0 ends the running
/// power-up at the next think; anything else that isn't one [`RUNNABLE`] bit does nothing.
pub fn run_powerup(p: &mut Player, cx: &mut Cx, bits: u16, secs: f32) {
    let now = cx.time();
    if bits == 0 {
        if p.active_powerup != 0 {
            p.powerup_until = 0.0;
        }
        return;
    }
    if !runnable(bits) {
        return;
    }
    let old = p.active_powerup;
    if old != 0 && !p.ts.plugin_powerup {
        // A used power-up is the held one: it's spent, as `UpdatePowerUpState` would end it.
        p.ts.held = 0;
        p.ts.held_secs = 0;
    }
    if old & (pw::SLOWMO | pw::SLOWPAUSE) != 0 && bits & (pw::SLOWMO | pw::SLOWPAUSE) == 0 {
        deactivate_flow(p, cx);
    }
    if old & pw::SUPERJUMP != 0 && bits & pw::SUPERJUMP == 0 {
        cx.pev().gravity = 1.0;
    }
    p.active_powerup = bits;
    p.powerup_until = now + secs;
    p.ts.plugin_powerup = true;
    // `UsePowerUp`'s messages: the HUD timer, and the motion blur of the slow ones.
    let secs = super::x87_trunc(p.powerup_until as f64 - now as f64) as u8;
    msg::one(Msg::PwUpTime, cx.edict, |w| PwUpTime { secs }.write(w));
    let mode = if bits & pw::SLOWMO != 0 {
        1
    } else if bits & pw::SLOWPAUSE != 0 {
        2
    } else {
        0
    };
    if mode != 0 && crate::gameplay::active().server.motion_blur() {
        let b = TsMBlur { entindex: cx.index() as u8, mode };
        msg::pvs(Msg::TsMBlur, cx.pev().origin, |w| b.write(w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #324: a ruleset whose client reads `TSSlowMo` 0 as normal gets 0 at normal speed, and the
    /// slowed amounts unchanged.
    #[test]
    fn hud_slow_normal_as_zero() {
        let mut p = Player::default();
        p.ts.slow_display = 1.0;
        let mut r = *crate::gameplay::active().server.powerups();
        assert_eq!(hud_slow_amount(&p, &r), 1.0);
        r.hud_normal_as_zero = true;
        assert_eq!(hud_slow_amount(&p, &r), 0.0);
        p.ts.slow_display = 0.5;
        assert_eq!(hud_slow_amount(&p, &r), 0.5);
        p.active_powerup = pw::SLOWMO;
        assert_eq!(hud_slow_amount(&p, &r), r.hud_slowmo);
    }

    /// #132: the flow sees through glass, and a full slowed list stops only the listing.
    #[test]
    fn slow_flow_traces_and_list_cap() {
        assert_eq!(FLOW_TRACE, 0x101);
        let ent = |i: usize| (i * 4096) as *mut edict_t;
        let mut list: Vec<*mut edict_t> = (1..=31).map(ent).collect();
        list_slowed(&mut list, ent(32));
        list_slowed(&mut list, ent(33));
        assert_eq!(list.len(), 32);
        assert_eq!(list.last(), Some(&ent(32)), "the 33rd is slowed but not listed");
    }

    /// Issue #49: `UsePowerUp` is reached only from `usepowerup` (`ClientCommand`) and the bots'
    /// power-up module, never from a server-side +use double tap (TS's `TSUse` is dead code).
    #[test]
    fn use_powerup_callers_are_the_command_and_the_bots() {
        let call = concat!("use_", "powerup(p, cx)");
        let callers: Vec<&str> = [
            ("rules/commands.rs", include_str!("commands.rs")),
            ("rules/lifecycle.rs", include_str!("lifecycle.rs")),
            ("bots/modules.rs", include_str!("../bots/modules.rs")),
            ("player/mod.rs", include_str!("../player/mod.rs")),
            ("stunt/mod.rs", include_str!("../stunt/mod.rs")),
        ]
        .into_iter()
        .filter(|(_, src)| src.contains(call))
        .map(|(name, _)| name)
        .collect();
        assert_eq!(callers, ["rules/commands.rs", "bots/modules.rs"]);
    }

    #[test]
    fn easing() {
        let mut v = 1.0;
        super::interpolate(0.1, &mut v, 0.35);
        // 70 % of the way in 0.1 s
        assert!((v - (1.0 - 0.65 * 0.7)).abs() < 1e-5);
        let mut v = 0.37;
        super::interpolate(0.01, &mut v, 0.35);
        assert_eq!(v, 0.35, "snaps within 0.05");
        let mut v = 1.0;
        super::interpolate(1.0, &mut v, 0.5);
        assert_eq!(v, 0.5, "factor capped at 1");
    }

    /// Issue #229: `InterpolateSlowMotion` (0x9910c, mp.dll 0x10067030) rounds `cur + d * k` once.
    /// 1/60 s frames from 1.0 toward 0.35: f32 steps would give 0x3f5b7030, 0x3f4c4a5a, 0x3f3ee8ee
    /// from the second frame on.
    #[test]
    fn easing_rounds_once() {
        let ft = 1.0f32 / 60.0;
        for (name, step) in
            [("msvc", interpolate_in::<shared::pm::Msvc> as fn(f32, f32, f32) -> f32), ("gcc", interpolate_in::<shared::pm::Gcc>)]
        {
            let mut v = 1.0f32;
            let got: Vec<u32> = (0..4)
                .map(|_| {
                    v = step(ft, v, 0.35);
                    v.to_bits()
                })
                .collect();
            assert_eq!(got, [0x3f6c9630, 0x3f5b702f, 0x3f4c4a59, 0x3f3ee8ed], "{name}");
            assert_eq!(step(0.01, 0.37, 0.35), 0.35, "{name}: snaps within 0.05");
            assert_eq!(step(1.0, 1.0, 0.5), 0.5, "{name}: factor capped at 1");
        }
        // A NaN factor: GCC keeps it (`test ah,0x41; jne`), MSVC takes 1 (`fcomp; test ah,1`).
        assert!(interpolate_in::<shared::pm::Gcc>(f32::NAN, 1.0, 0.35).is_nan());
        assert_eq!(interpolate_in::<shared::pm::Msvc>(f32::NAN, 1.0, 0.35), 0.35);
    }

    /// Issue #245: a dead player's power-up drops with `CreatePowerUp(type, -1)` (3.0 0x858a8 /
    /// 0x8592b, 2.1 0x100569db / 0x10056a4a), so `SetupPowerUp` (0x98014) gives it the type's base
    /// duration (a created power-up's `pwupmult` is the constructor's 1.0, 0x989a8), whatever the
    /// player held.
    #[test]
    fn death_drop_has_the_base_duration() {
        use crate::entities::ts::powerup::setup_secs;
        let rules = crate::gameplay::TEST_POWERUPS;
        assert_eq!(DEATH_DROP_SECS, -1);
        for (kind, base) in [(pw::SLOWMO, 3), (pw::INFINITE_CLIP, 3), (pw::SLOWPAUSE, 2), (pw::DOUBLE_FIRERATE, 3)] {
            assert_eq!(rules.base(kind), base);
            assert_eq!(setup_secs(DEATH_DROP_SECS, rules.base(kind), rules.multiplier, 1.0), base, "{kind:#x}");
        }
        // What the held seconds would have given: a `pwupmult 2` slow motion's 6 s stays 6.
        assert_eq!(setup_secs(6, 3, true, 1.0), 6);
    }

    /// Issue #223: `DeactivateSlowMotionFlow` releases the players it slowed through `GoSlow`
    /// (3.0 0x96a9c's virtual call, 2.1 FUN_10057610 → FUN_10055220), whose 2.1 release clamps
    /// their next-attack times (`ServerRules::on_go_slow`), not by writing their state.
    #[test]
    fn deactivate_flow_releases_victims_through_go_slow() {
        let src = include_str!("powerup.rs");
        let start = src.find(concat!("pub fn deactivate_", "flow")).unwrap();
        let body = &src[start..start + src[start..].find("\n}\n").unwrap()];
        assert!(body.contains("go_slow(o, 1.0, false)"));
        assert!(!body.contains("set_slow_target"));
    }

    /// Issue #253: a player's own `GoSlow` is TS's virtual call (`vtable[5]`: `SlowMotionFlow`
    /// 0x96bb0, `DeactivateSlowMotionFlow` 0x96a9c, `Killed`), so it goes through the hooks.
    #[test]
    fn own_go_slow_goes_through_the_hooks() {
        let src = include_str!("powerup.rs");
        let start = src.find(concat!("pub fn go_slow_", "self(")).unwrap();
        let body = &src[start..start + src[start..].find("\n}\n").unwrap()];
        assert!(body.contains("crate::ham::player_go_slow(p, cx, amount, btime, go_slow_self_plain)"));
    }
}
