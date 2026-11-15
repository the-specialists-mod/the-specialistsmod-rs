//! What the version 6 callbacks report beyond the bare event (`plugins` sends them): the damage
//! event with the hit's hitgroup, direction and position, a sound's responsible player, and the
//! pickups. Version 8 (#327) adds the health taken, armour absorbed, refused flag, per-hitgroup
//! pellet counts and damage flags (stunting, dodging, headshot).

use crate::player::damage::TakeDamageExtra;
use crate::rules;
use crate::world::DamageInfo;
use goldsrc::Global;
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::cell::Cell;
use std::ffi::c_int;
use ts_plugin_api::DamageEvent;

/// The last `TraceAttack` on a player: (victim, time, hitgroup, dir, pos).
type Trace = (*mut edict_t, f32, i32, [f32; 3], [f32; 3]);

static LAST_TRACE: Global<Cell<Option<Trace>>> = Global(Cell::new(None));

/// `CBasePlayer::TraceAttack` ran on `e`: keep where it hit for the `TakeDamage` it leads to,
/// which gets only TS's four arguments (`damage::multi`).
pub fn traced(e: *mut edict_t, dmg: &DamageInfo) {
    LAST_TRACE.0.set(Some((e, engine().time(), dmg.hitgroup, dmg.dir, dmg.pos)));
}

/// The hit `TakeDamage(dmg)` on `e` came from: its own fields when it was dealt with them, else
/// the last `TraceAttack` on `e` in this frame (taken, so a later direct damage doesn't inherit
/// it), else none (hitgroup 0, zero vectors).
pub fn hit_of(e: *mut edict_t, dmg: &DamageInfo, now: f32) -> (i32, [f32; 3], [f32; 3]) {
    let last = LAST_TRACE.0.take();
    pick_hit(e, dmg, now, last)
}

/// [`hit_of`]'s choice, without the engine.
pub fn pick_hit(e: *mut edict_t, dmg: &DamageInfo, now: f32, last: Option<Trace>) -> (i32, [f32; 3], [f32; 3]) {
    if dmg.hitgroup != 0 || dmg.dir != [0.0; 3] {
        return (dmg.hitgroup, dmg.dir, dmg.pos);
    }
    match last {
        Some((v, t, hg, dir, pos)) if v == e && t == now => (hg, dir, pos),
        _ => (0, [0.0; 3], [0.0; 3]),
    }
}

/// An entity's index for an event: -1 for none.
fn index(e: *mut edict_t) -> c_int {
    if e.is_null() { -1 } else { engine().index_of(e) }
}

/// Player `e`'s `TakeDamage(dmg)` ran: the `Plugin::damage` event, with the version 8 extras
/// from `take_damage` (#327, #328).
pub fn damage(e: *mut edict_t, dmg: &DamageInfo, extra: &TakeDamageExtra) {
    let now = engine().time();
    let (hitgroup, dir, pos) = hit_of(e, dmg, now);
    let gun = |a: *mut edict_t| {
        if rules::is_player(a) { super::player_info(engine().index_of(a)).ok().map(|i| i.weapon as u8) } else { None }
    };
    let v = e.v();
    // #327: health actually lost and whether the damage was refused.
    let taken = if extra.refused { 0.0 } else { (extra.health_prev - v.health).max(0.0) };
    // #327: per-hitgroup pellet counts and damage flags from the multi-damage.
    // Only for `ApplyMultiDamage`'s damage: direct damage has none.
    let (pellets, mut flags) = if crate::damage::multi::applying() {
        (crate::damage::multi::pellets(), crate::damage::multi::damage_flags())
    } else {
        ([0; 8], 0)
    };
    if crate::damage::multi::applying() && crate::damage::multi::headshot() {
        flags |= ts_plugin_api::damage_flags::HEADSHOT;
    }
    let event = DamageEvent {
        victim: index(e),
        attacker: index(dmg.attacker),
        inflictor: index(dmg.inflictor),
        damage: dmg.damage,
        bits: dmg.bits,
        health: v.health,
        armor: v.armorvalue,
        hitgroup,
        weapon: crate::rules::scoring::blow_weapon(dmg, dmg.attacker, gun(dmg.attacker)),
        dir,
        pos,
        taken,
        armor_absorbed: extra.armor_absorbed,
        refused: extra.refused as c_int,
        pellets,
        flags,
    };
    crate::plugins::damage(e, dmg.attacker, event);
}

/// The player responsible for entity `entity` (an index): itself when a player, else
/// `projectile_owner`'s answer, 0 for none.
pub fn source_player(entity: c_int) -> c_int {
    if entity <= 0 {
        return 0;
    }
    let eng = engine();
    if entity <= eng.max_clients() && rules::is_player(eng.player(entity)) {
        return entity;
    }
    super::projectile_owner(entity).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(n: usize) -> *mut edict_t {
        std::ptr::without_provenance_mut(n * 0x100)
    }

    #[test]
    fn the_hit_comes_from_this_frames_trace_on_the_victim() {
        let (a, b) = (fake(1), fake(2));
        let direct = DamageInfo::new(std::ptr::null_mut(), std::ptr::null_mut(), 10.0, 0);
        let trace = Some((a, 5.0, 1, [1.0, 0.0, 0.0], [3.0, 4.0, 5.0]));
        assert_eq!(pick_hit(a, &direct, 5.0, trace), (1, [1.0, 0.0, 0.0], [3.0, 4.0, 5.0]), "the head shot");
        assert_eq!(pick_hit(b, &direct, 5.0, trace).0, 0, "another victim's trace");
        assert_eq!(pick_hit(a, &direct, 5.1, trace).0, 0, "a stale trace");
        let mut own = direct;
        (own.hitgroup, own.dir) = (6, [0.0, 1.0, 0.0]);
        assert_eq!(pick_hit(a, &own, 5.0, trace).0, 6, "damage that carries its own hit");
    }
}
