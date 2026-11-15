//! Drowning, slime/lava and time-based damage. Owner: rules work package.
//! Reference: `CBasePlayer::WaterMove` (0xd905c), `CBasePlayer::CheckTimeBasedDamage` (0xda00c)
//! and the time-based bookkeeping at the end of `CBasePlayer::TakeDamage` (0x8fa94), all called
//! from `CBasePlayer::PreThink` (0x908cc) through `rules::lifecycle::pre_think`.
//!
//! TS keeps the SDK's versions almost unchanged. They run on the real clock (no `fuser1`
//! scaling); only the sounds go through TS's `EMIT_SOUND_DYN`, whose pitch follows the time
//! scale. Float-to-int conversions truncate (GCC 2.95 sets the x87 control word to chop).

use super::Player;
use crate::damage::bits;
use crate::entities::stock::explode::emit_sound_slow;
use crate::world::{Cx, DamageInfo};
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::ffi::CStr;

/// `AIRTIME`: seconds of air after the head goes under.
pub const AIR_TIME: f32 = 12.0;
/// Number of time-based damage types (`CDMG_TIMEBASED`).
const TIMEBASED_TYPES: usize = 8;
/// `itbd_*` indices into `TimeBasedDamage::left`.
const ITBD_NERVEGAS: usize = 1;
const ITBD_POISON: usize = 2;
const ITBD_DROWNRECOVER: usize = 4;
/// `POISON_DAMAGE` per tick (TS: `TakeDamage(pev, pev, 2.0, DMG_GENERIC)`).
pub const POISON_DAMAGE: f32 = 2.0;
/// Most drowned health given back per tick.
pub const DROWN_RECOVER_STEP: i32 = 10;
const ATTN_NORM: f32 = 0.8;

/// The SDK's `m_bitsDamageType` and friends. TS `Spawn` clears only `bits`; the drown counters
/// and `prev` live for the whole connection (the private data is zeroed once), so health drowned
/// away in one life can come back in the next.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct TimeBasedDamage {
    /// `m_bitsDamageType` (+0x110): every damage type taken since spawn. TS never clears the
    /// instant types (the SDK did that in `UpdateClientData` next to the removed `Damage`
    /// message), which is harmless: only the eight time-based bits do anything.
    pub bits: i32,
    /// `m_rgbTimeBasedDamage` (+0x114): ticks left per time-based type; 0 = not started.
    pub left: [u8; TIMEBASED_TYPES],
    /// `m_tbdPrev` (+0x980): time of the last tick.
    pub prev: f32,
    /// `m_idrowndmg` (+0x9a8) and `m_idrownrestored` (+0x9ac).
    pub drowned: i32,
    pub restored: i32,
    /// `m_rgItems[ITEM_ANTIDOTE]` (+0x834). Only `item_antidote` adds to it; like TS, nothing
    /// resets it on death or respawn.
    pub antidotes: i32,
}

/// What a `CheckTimeBasedDamage` tick asks the caller to do, in this order.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct Tick {
    /// An antidote cured nerve gas: queue the suit's `!HEV_HEAL4`.
    pub cured_nervegas: bool,
    /// Deal `POISON_DAMAGE` (`DMG_GENERIC`, from the player to itself).
    pub poison: bool,
    /// An antidote cured poison (after that tick's poison damage): `!HEV_HEAL4`.
    pub cured_poison: bool,
    /// `TakeHealth(heal)`; the restored counter has already been bumped, as in TS, whether or
    /// not the health is accepted.
    pub heal: i32,
}

impl TimeBasedDamage {
    /// `TakeDamage` (0x8fa94), after `CBaseMonster::TakeDamage` (even when that refused the
    /// damage): remember the types and restart the countdown of every time-based type taken.
    pub fn on_damage(&mut self, dmg_bits: i32) {
        self.bits |= dmg_bits;
        for (i, left) in self.left.iter_mut().enumerate() {
            if dmg_bits & (bits::PARALYZE << i) != 0 {
                *left = 0;
            }
        }
    }

    /// `WaterMove` with the head under: no drown recovery while submerged.
    pub fn submerged(&mut self) {
        self.bits &= !bits::DROWNRECOVER;
        self.left[ITBD_DROWNRECOVER] = 0;
    }

    /// `WaterMove` with the head out (every frame, also on dry land): while drowned health is
    /// owed, keep (re)starting drown recovery. Restarting the counter each frame means recovery
    /// never times out until everything is given back.
    pub fn surfaced(&mut self) {
        if self.restored < self.drowned {
            self.bits = (self.bits & !bits::DROWN) | bits::DROWNRECOVER;
            self.left[ITBD_DROWNRECOVER] = 0;
        }
    }

    /// `WaterMove`: a drowning hit of `dmg` was dealt; TS adds it to an `int` (truncating).
    pub fn add_drowned(&mut self, dmg: f32) {
        self.drowned = (self.drowned as f32 + dmg) as i32;
    }

    /// `CheckTimeBasedDamage` (0xda00c): at most every 2 s (whole seconds, truncated), run one
    /// tick of every pending time-based type. Durations: poison 5 ticks, drown recovery 4, the
    /// rest 2. The first tick only starts the countdown; a type ends when its counter runs out.
    pub fn tick(&mut self, now: f32) -> Option<Tick> {
        if self.bits & bits::TIMEBASED == 0 {
            return None;
        }
        if ((now - self.prev) as i32).abs() < 2 {
            return None;
        }
        self.prev = now;
        let mut out = Tick::default();
        for i in 0..TIMEBASED_TYPES {
            let bit = bits::PARALYZE << i;
            if self.bits & bit == 0 {
                continue;
            }
            let duration = match i {
                ITBD_POISON => {
                    out.poison = true;
                    5
                }
                ITBD_DROWNRECOVER => {
                    if self.restored < self.drowned {
                        let step = (self.drowned - self.restored).min(DROWN_RECOVER_STEP);
                        out.heal = step;
                        self.restored += step;
                    }
                    4
                }
                // Paralyze, nerve gas, radiation, acid, slow burn, slow freeze: no effect.
                _ => 2,
            };
            if self.left[i] == 0 {
                self.left[i] = duration;
                continue;
            }
            // An antidote cures nerve gas or poison once its countdown is below the full duration.
            let cures = (i == ITBD_NERVEGAS && self.left[i] < 2) || (i == ITBD_POISON && self.left[i] < 5);
            if cures && self.antidotes != 0 {
                self.left[i] = 0;
                self.antidotes -= 1;
                if i == ITBD_NERVEGAS {
                    out.cured_nervegas = true;
                } else {
                    out.cured_poison = true;
                }
            }
            if self.left[i] == 0 || {
                self.left[i] -= 1;
                self.left[i] == 0
            } {
                self.left[i] = 0;
                self.bits &= !bit;
            }
        }
        Some(out)
    }
}

/// `WaterMove`: the next drowning hit, one more than the last, at most 5 (`pev->dmg` is 2 when
/// the head goes under, so the hits are 3, 4, 5, 5, ...).
pub fn next_drown_damage(dmg: f32) -> f32 {
    (dmg + 1.0).min(5.0)
}

/// `WaterMove`: the "up for air" gasp when the head comes out. `pl_wade1` once the air ran out,
/// `pl_wade2` after more than 3 s under, nothing for shorter dips (or on dry land).
pub fn surface_sound(air_finished: f32, now: f32) -> Option<&'static CStr> {
    if now > air_finished {
        Some(c"player/pl_wade1.wav")
    } else if air_finished < now + 9.0 {
        Some(c"player/pl_wade2.wav")
    } else {
        None
    }
}

/// The world: inflictor and attacker of drowning and slime/lava damage (`VARS(eoNullEntity)`).
fn world_damage(dmg: f32, dmg_bits: i32) -> DamageInfo {
    let world = engine().entity_of_index(0);
    DamageInfo::new(world, world, dmg, dmg_bits)
}

/// `CBasePlayer::WaterMove` (0xd905c).
pub fn water_move(p: &mut Player, cx: &mut Cx) {
    let eng = engine();
    let now = cx.time();
    let e = cx.edict;
    {
        let v = cx.pev();
        if v.movetype == MOVETYPE_NOCLIP as i32 || v.health < 0.0 {
            return;
        }
    }

    if cx.pev().waterlevel == 3 {
        p.tbd.submerged();
        let (air_finished, pain_finished) = (cx.pev().air_finished, cx.pev().pain_finished);
        if now <= air_finished {
            p.tbd.bits &= !bits::DROWN;
        } else if pain_finished < now {
            let dmg = next_drown_damage(cx.pev().dmg);
            cx.pev().dmg = dmg;
            // #326: TS calls the virtual `TakeDamage` (`WaterMove` 0xd905c), so the ham hooks
            // and the damage event must fire.
            crate::ham::player_take_damage(p, cx, &world_damage(dmg, bits::DROWN));
            let v = cx.pev();
            v.pain_finished = now + 1.0;
            // TS re-reads pev->dmg after the damage call.
            let dmg = v.dmg;
            p.tbd.add_drowned(dmg);
        }
    } else {
        if let Some(s) = surface_sound(cx.pev().air_finished, now) {
            emit_sound_slow(e, CHAN_VOICE, s, 1.0, ATTN_NORM, 0, 100, 0.5);
        }
        let v = cx.pev();
        v.air_finished = now + AIR_TIME;
        v.dmg = 2.0;
        p.tbd.surfaced();
    }

    if cx.pev().waterlevel == 0 {
        cx.pev().flags &= !(FL_INWATER as i32);
        return;
    }

    // Swimming sounds, likelier the less air is left.
    let air = (cx.pev().air_finished - now) as i32;
    if eng.random_long(0, 31) == 0 && air <= eng.random_long(0, AIR_TIME as i32 - 1) {
        let s = [c"player/pl_swim1.wav", c"player/pl_swim2.wav", c"player/pl_swim3.wav", c"player/pl_swim4.wav"]
            [eng.random_long(0, 3) as usize];
        emit_sound_slow(e, CHAN_BODY, s, 0.8, ATTN_NORM, 0, 100, 0.5);
    }

    // SDK quirks kept: lava never advances `dmgtime` (entering any liquid zeroes it), so it
    // burns every frame; slime sets `dmgtime` but never checks it, so it too hurts every frame.
    let (watertype, waterlevel) = (cx.pev().watertype, cx.pev().waterlevel);
    // #326: TS calls the virtual `TakeDamage` for lava and slime (`WaterMove` 0xd905c).
    if watertype == CONTENTS_LAVA {
        if now > cx.pev().dmgtime {
            crate::ham::player_take_damage(p, cx, &world_damage((waterlevel * 10) as f32, bits::BURN));
        }
    } else if watertype == CONTENTS_SLIME {
        cx.pev().dmgtime = now + 1.0;
        crate::ham::player_take_damage(p, cx, &world_damage((waterlevel * 4) as f32, bits::ACID));
    }

    let v = cx.pev();
    if v.flags & FL_INWATER as i32 == 0 {
        v.flags |= FL_INWATER as i32;
        v.dmgtime = 0.0;
    }
}

/// `CBasePlayer::CheckTimeBasedDamage` (0xda00c): poison damage and drown recovery.
pub fn check_time_based_damage(p: &mut Player, cx: &mut Cx) {
    let now = cx.time();
    let Some(tick) = p.tbd.tick(now) else {
        return;
    };
    // `SetSuitUpdate("!HEV_HEAL4", FALSE, 0)` (0xda199) for a cure, in the loop's type order.
    if tick.cured_nervegas {
        super::suit::set_suit_update(p, cx, Some(c"!HEV_HEAL4"), false, 0);
    }
    if tick.poison {
        let e = cx.edict;
        // #326: TS calls the virtual `TakeDamage` (`CheckTimeBasedDamage` 0xda00c).
        crate::ham::player_take_damage(p, cx, &DamageInfo::new(e, e, POISON_DAMAGE, bits::GENERIC));
    }
    if tick.cured_poison {
        super::suit::set_suit_update(p, cx, Some(c"!HEV_HEAL4"), false, 0);
    }
    if tick.heal > 0 {
        super::damage::take_health(p, cx, tick.heal as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drown_damage_ramps_to_five() {
        let mut dmg = 2.0; // set on spawn and whenever the head is out
        let hits: Vec<f32> = (0..5)
            .map(|_| {
                dmg = next_drown_damage(dmg);
                dmg
            })
            .collect();
        assert_eq!(hits, [3.0, 4.0, 5.0, 5.0, 5.0]);
    }

    #[test]
    fn surface_sounds() {
        assert_eq!(surface_sound(10.0, 10.5), Some(c"player/pl_wade1.wav"), "out of air");
        assert_eq!(surface_sound(18.9, 10.0), Some(c"player/pl_wade2.wav"), "more than 3 s under");
        assert_eq!(surface_sound(19.0, 10.0), None, "brief dip");
        assert_eq!(surface_sound(22.0, 10.0), None, "dry land (air refilled every frame)");
        assert_eq!(surface_sound(10.0, 10.0), Some(c"player/pl_wade2.wav"), "air just used up");
    }

    #[test]
    fn on_damage_sets_bits_and_restarts_countdowns() {
        let mut t = TimeBasedDamage { left: [3; 8], ..Default::default() };
        t.on_damage(bits::POISON | bits::BULLET);
        assert_eq!(t.bits, bits::POISON | bits::BULLET);
        assert_eq!(t.left, [3, 3, 0, 3, 3, 3, 3, 3]);
        t.on_damage(bits::GENERIC);
        assert_eq!(t.bits, bits::POISON | bits::BULLET, "bits accumulate");
    }

    #[test]
    fn tick_waits_two_whole_seconds() {
        let mut t = TimeBasedDamage::default();
        assert_eq!(t.tick(100.0), None, "nothing pending");
        t.on_damage(bits::POISON);
        assert!(t.tick(100.0).is_some());
        assert_eq!(t.tick(101.99), None, "1.99 s truncates to 1");
        assert!(t.tick(102.0).is_some());
        assert_eq!(t.prev, 102.0);
        // Only bits in DMG_TIMEBASED open the gate, but any of them do (drown, high TS bits).
        let mut t = TimeBasedDamage::default();
        t.on_damage(bits::BULLET);
        assert_eq!(t.tick(100.0), None);
        t.on_damage(bits::DROWN);
        assert_eq!(t.tick(100.0), Some(Tick::default()));
    }

    #[test]
    fn poison_deals_damage_for_six_ticks() {
        let mut t = TimeBasedDamage::default();
        t.on_damage(bits::POISON);
        let mut hits = 0;
        let mut now = 10.0;
        while let Some(tick) = t.tick(now) {
            if tick.poison {
                hits += 1;
            }
            now += 2.0;
            if t.bits & bits::POISON == 0 {
                break;
            }
        }
        // First tick starts the 5-tick countdown, then 5 more ticks until it reaches 0.
        assert_eq!(hits, 6);
        assert_eq!(t.left[ITBD_POISON], 0);
        assert_eq!(t.bits & bits::TIMEBASED, 0);
    }

    #[test]
    fn new_poison_restarts_the_countdown() {
        let mut t = TimeBasedDamage::default();
        t.on_damage(bits::POISON);
        t.tick(10.0);
        t.tick(12.0);
        assert_eq!(t.left[ITBD_POISON], 4);
        t.on_damage(bits::POISON);
        assert_eq!(t.left[ITBD_POISON], 0);
        t.tick(14.0);
        assert_eq!(t.left[ITBD_POISON], 5);
    }

    #[test]
    fn antidote_cures_poison_after_two_ticks() {
        let mut t = TimeBasedDamage { antidotes: 1, ..Default::default() };
        t.on_damage(bits::POISON);
        assert!(t.tick(10.0).unwrap().poison);
        assert!(!t.tick(12.0).unwrap().cured_poison);
        assert_eq!(t.antidotes, 1, "the countdown must have dropped below 5 first");
        let tick = t.tick(14.0).unwrap();
        assert!(tick.poison && tick.cured_poison && !tick.cured_nervegas, "the cure (and HEV_HEAL4) after this tick's damage");
        assert_eq!(t.antidotes, 0);
        assert_eq!(t.bits & bits::POISON, 0);
    }

    #[test]
    fn antidote_cures_nerve_gas_before_the_poison_damage() {
        let mut t = TimeBasedDamage { antidotes: 2, ..Default::default() };
        t.on_damage(0x10000); // DMG_NERVEGAS
        assert_eq!(t.tick(10.0), Some(Tick::default()), "the first tick starts the countdown");
        assert_eq!(t.tick(12.0), Some(Tick::default()), "the countdown must have dropped below 2 first");
        let tick = t.tick(14.0).unwrap();
        assert!(tick.cured_nervegas && !tick.cured_poison);
        assert_eq!(t.antidotes, 1);
    }

    #[test]
    fn drown_bookkeeping_and_recovery() {
        let mut t = TimeBasedDamage::default();
        // Drown for 3, 4, 5, 5, 5 = 22.
        let mut dmg = 2.0;
        for _ in 0..5 {
            dmg = next_drown_damage(dmg);
            t.on_damage(bits::DROWN);
            t.add_drowned(dmg);
            t.submerged();
        }
        assert_eq!(t.drowned, 22);
        assert_eq!(t.bits & bits::DROWNRECOVER, 0, "no recovery while submerged");

        t.surfaced();
        assert_eq!(t.bits & (bits::DROWN | bits::DROWNRECOVER), bits::DROWNRECOVER);
        let mut now = 100.0;
        let mut healed = Vec::new();
        for _ in 0..10 {
            if let Some(tick) = t.tick(now) {
                healed.push(tick.heal);
            }
            // WaterMove runs every frame with the head out, restarting the recovery countdown.
            t.surfaced();
            now += 2.0;
        }
        assert_eq!(&healed[..4], &[10, 10, 2, 0]);
        assert_eq!(t.restored, 22);
        assert!(healed[4..].iter().all(|&h| h == 0));
        assert_eq!(t.bits & bits::DROWNRECOVER, 0, "recovery ends once everything is back");
    }

    #[test]
    fn going_under_stops_recovery() {
        let mut t = TimeBasedDamage { drowned: 30, restored: 10, ..Default::default() };
        t.surfaced();
        assert_eq!(t.tick(10.0).unwrap().heal, 10);
        t.submerged();
        assert_eq!(t.bits & bits::DROWNRECOVER, 0);
        assert_eq!(t.left[ITBD_DROWNRECOVER], 0);
        assert_eq!(t.restored, 20);
    }

    #[test]
    fn drowned_counter_truncates() {
        let mut t = TimeBasedDamage::default();
        t.add_drowned(3.9);
        assert_eq!(t.drowned, 3);
    }
}
