//! Damage plumbing shared by every subsystem. Weapons build a `DamageInfo` and call `apply`;
//! the victim's `Entity::take_damage` decides what happens (players: player/damage.rs).

use goldsrc::edict::EdictPtr;
use goldsrc::sys::*;
use shared::pm::{FpuMath, Msvc, ORDER_XYZ};

/// Damage bits (SDK `DMG_*`) used by TS.
pub mod bits {
    pub const GENERIC: i32 = 0;
    pub const CRUSH: i32 = 1 << 0;
    pub const BULLET: i32 = 1 << 1;
    pub const SLASH: i32 = 1 << 2;
    pub const BURN: i32 = 1 << 3;
    pub const FALL: i32 = 1 << 5;
    pub const BLAST: i32 = 1 << 6;
    pub const CLUB: i32 = 1 << 7;
    pub const SHOCK: i32 = 1 << 8;
    pub const DROWN: i32 = 1 << 14;
    pub const NEVERGIB: i32 = 1 << 12;
    pub const ALWAYSGIB: i32 = 1 << 13;
    /// First of the eight time-based types (`player::water::TimeBasedDamage`): paralyze, nerve
    /// gas, poison, radiation, drown recover, acid, slow burn, slow freeze.
    pub const PARALYZE: i32 = 1 << 15;
    pub const POISON: i32 = 1 << 17;
    pub const DROWNRECOVER: i32 = 1 << 19;
    pub const ACID: i32 = 1 << 20;
    /// `DMG_TIMEBASED` (`~0x3fff`): the mask `CheckTimeBasedDamage` (0xda00c) gates on. It also
    /// covers `DROWN` and every bit above the eight time-based ones.
    pub const TIMEBASED: i32 = !0x3fff;
    /// TS: thrown knife/katana (part of the armour-exempt mask 0x80040e4, weapons.md §5.9).
    pub const THROWN: i32 = 0x0800_0000;
    /// Damage types kevlar never absorbs.
    pub const ARMOR_EXEMPT: i32 = 0x0800_40e4;
}

/// Hitgroups (studio hitboxes).
pub mod hitgroup {
    pub const GENERIC: i32 = 0;
    pub const HEAD: i32 = 1;
    pub const CHEST: i32 = 2;
    pub const STOMACH: i32 = 3;
    pub const LEFTARM: i32 = 4;
    pub const RIGHTARM: i32 = 5;
    pub const LEFTLEG: i32 = 6;
    pub const RIGHTLEG: i32 = 7;
}

#[derive(Clone, Copy, Debug)]
pub struct DamageInfo {
    /// What did the damage (bullet owner, grenade, trigger...).
    pub inflictor: *mut edict_t,
    /// Who gets the credit.
    pub attacker: *mut edict_t,
    pub damage: f32,
    pub bits: i32,
    pub hitgroup: i32,
    /// `TraceAttack`'s `vecDir` (direction of travel, normalised) and `ptr->vecEndPos`, when
    /// known. Like the hitgroup they only reach `TraceAttack`: `TakeDamage` gets TS's four
    /// arguments (inflictor, attacker, amount, bits), and `Killed` pushes the body along
    /// [`attack_dir`] (`g_vecAttackDir`), not along `dir`.
    pub dir: [f32; 3],
    pub pos: [f32; 3],
    /// Dealt as TS's `ClearMultiDamage`, `victim->TraceAttack` (vtable +0x54),
    /// `ApplyMultiDamage(inflictor, attacker)` sequence rather than a direct `TakeDamage` (+0x58):
    /// [`apply`] then runs the victim's [`world::Entity::trace_attack`] (players: spawn
    /// protection, stunt ×0.7, team check, hitgroup, `PTakeDam`; breakables and doors: their hit
    /// effects), which adds to the multi-damage, and the summed damage reaches `take_damage` with
    /// this field false. Set it with `DamageInfo::traced()` in every caller that TS routes through
    /// one TraceAttack between a Clear and an Apply: `CBulletManager::UpdateBullet`,
    /// `FireBullets` (one shot per `func_tank` call), melee (`KnifeFire`, `KatanaFire`,
    /// `CloseCombatAttack`, `ColdCockAttack`, `CTSStunt::CheckForBreakables`), `TSExplosion`
    /// (also when occluded), thrown/falling objects (`CTSKnife::BounceTouch`/`WaitThink`,
    /// `CTSShrapnel::BounceTouch`, `CM61Grenade::WaitThink`, `CTSWorldGun::Fall`,
    /// `CTSFlag::Fall`), `CBeam::BeamDamage` and `RadiusDamage` when its trace hit something.
    /// `ShootBulletsPlayer` traces every pellet before one Apply, so it drives [`multi`] itself.
    /// Crushers, `trigger_hurt`, falling, drowning, time-based damage, telefrags, team changes,
    /// breakable touch and Xen trees call `TakeDamage` directly and leave it false.
    pub traced: bool,
}

impl DamageInfo {
    pub fn new(inflictor: *mut edict_t, attacker: *mut edict_t, damage: f32, bits: i32) -> Self {
        Self { inflictor, attacker, damage, bits, hitgroup: hitgroup::GENERIC, dir: [0.0; 3], pos: [0.0; 3], traced: false }
    }

    /// The same damage, dealt through `TraceAttack` (sets the `traced` field).
    pub fn traced(mut self) -> Self {
        self.traced = true;
        self
    }
}

/// Deals damage to `victim`. Returns false if it isn't game-owned or refused the damage.
///
/// A `traced` hit is TS's `ClearMultiDamage`, `victim->TraceAttack(attacker, …)`,
/// `ApplyMultiDamage(inflictor, attacker)`; it returns whether the victim's `TakeDamage` took
/// the damage (false when its TraceAttack added nothing). Like TS's callers it doesn't check
/// `takedamage` first: each TraceAttack does (a `func_breakable` still sparks, a door still runs
/// its own check), and callers that test it in TS (melee, explosions) test it themselves.
/// Otherwise this is a direct `TakeDamage`, with no `takedamage` test here either: TS's crushers
/// (`CFuncPlat::Blocked` 0xd2e64, the doors', trains' and rotators' `Blocked`), telefrags,
/// `game_player_hurt`, falling and the Xen tree call the victim's `TakeDamage` straight away, and
/// each class's `TakeDamage` decides (`CBaseEntity::TakeDamage` and `CBaseMonster::TakeDamage`
/// refuse with `takedamage` off; `CBreakable`, `CBaseButton`, the cyclers and `CGunTarget` don't
/// look at it; `CBasePlayer::TakeDamage` 0x8fa94 still runs its time-based reset, director event
/// and fade before `CBaseMonster::TakeDamage` refuses). The callers that test it in TS, like
/// `trigger_hurt` and `RadiusDamage`, test it themselves.
///
/// Re-entrancy: if `victim` is the entity currently being dispatched (self-damage, e.g. your own
/// grenade), world::dispatch can't reach it and this returns false; handle self-damage at the
/// call site with the `&mut` you already hold.
pub fn apply(victim: *mut edict_t, info: &DamageInfo) -> bool {
    if victim.is_null() {
        return false;
    }
    if info.traced {
        multi::clear();
        trace_attack(victim, info);
        return multi::apply(info.inflictor, info.attacker);
    }
    crate::ham::take_damage(victim, info).unwrap_or(false)
}

/// `victim->TraceAttack(info.attacker, info.damage, info.dir, &tr, info.bits)` (vtable +0x54):
/// the victim's hit effects, adding what it takes to the [`multi`] damage. Nothing happens to an
/// edict the game doesn't own (or that is being dispatched further up the stack).
pub fn trace_attack(victim: *mut edict_t, info: &DamageInfo) {
    if victim.is_null() {
        return;
    }
    crate::ham::trace_attack(victim, info);
}

/// TS's multi-damage (`gMultiDamage` 0x19f250: entity, amount, type, and `glast_headshot`):
/// `TraceAttack`s add to it and `ApplyMultiDamage` hands the sum to one `TakeDamage`, so the
/// pellets of one shotgun shot that hit the same victim in a row become one hit.
pub mod multi {
    use super::DamageInfo;
    use goldsrc::Global;
    use goldsrc::edict::EdictPtr;
    use goldsrc::sys::edict_t;
    use std::cell::Cell;

    /// `gMultiDamage` (entity, amount, type). Nothing else of a TraceAttack is kept: `TakeDamage`
    /// gets TS's four arguments, and what `Killed`/`PlayerKilled` read besides them are TS's
    /// globals ([`super::attack_dir`], [`super::glast`], [`headshot`]).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct MultiDamage {
        pub entity: *mut edict_t,
        pub amount: f32,
        pub bits: i32,
    }

    impl MultiDamage {
        pub const EMPTY: Self = Self { entity: std::ptr::null_mut(), amount: 0.0, bits: 0 };

        /// The `TakeDamage(inflictor, attacker, amount, type)` arguments
        /// `ApplyMultiDamage(inflictor, attacker)` passes, or `None` without a victim.
        pub fn take_damage_info(&self, inflictor: *mut edict_t, attacker: *mut edict_t) -> Option<DamageInfo> {
            if self.entity.is_null() {
                return None;
            }
            Some(DamageInfo::new(inflictor, attacker, self.amount, self.bits))
        }
    }

    static MULTI: Global<Cell<MultiDamage>> = Global(Cell::new(MultiDamage::EMPTY));
    static LAST_HEADSHOT: Global<Cell<bool>> = Global(Cell::new(false));
    /// Port addition (#327): per-hitgroup pellet counts for the current multi-damage, so the
    /// damage event can report where each pellet landed.
    static PELLET_COUNTS: Global<Cell<[u8; 8]>> = Global(Cell::new([0; 8]));
    /// Port addition (#327): damage flags accumulated by TraceAttacks for this multi-damage.
    static DAMAGE_FLAGS: Global<Cell<i32>> = Global(Cell::new(0));
    /// Port addition (#327): `ApplyMultiDamage`'s `TakeDamage` is running, so the pellets and
    /// flags belong to the damage being dealt (a direct hit in between, a fall after a shotgun
    /// shot, mustn't report them).
    static APPLYING: Global<Cell<bool>> = Global(Cell::new(false));

    /// `ClearMultiDamage` (0xf0d0c): no victim, no damage, no type. `glast_headshot` is left alone.
    pub fn clear() {
        MULTI.0.set(MultiDamage::EMPTY);
        PELLET_COUNTS.0.set([0; 8]);
        DAMAGE_FLAGS.0.set(0);
    }

    /// `gMultiDamage.type = bits`: `ShootBulletsPlayer` (0x6f69c) and `FireBullets` (0xb39a4)
    /// start the type at `DMG_BULLET | DMG_NEVERGIB` (0x1002) after the Clear.
    pub fn set_type(bits: i32) {
        let mut m = MULTI.0.get();
        m.bits = bits;
        MULTI.0.set(m);
    }

    /// `AddMultiDamage` on `cell`; `flush` is the `ApplyMultiDamage` of the old victim.
    fn add_to(cell: &Cell<MultiDamage>, victim: *mut edict_t, amount: f32, bits: i32, flush: impl FnOnce()) {
        if victim.is_null() {
            return;
        }
        let mut m = cell.get();
        m.bits |= bits;
        let switch = victim != m.entity;
        cell.set(m);
        if switch {
            flush();
            let mut m = cell.get();
            m.entity = victim;
            m.amount = 0.0;
            cell.set(m);
        }
        let mut m = cell.get();
        m.amount += amount;
        cell.set(m);
    }

    /// `AddMultiDamage(pevInflictor, victim, amount, bits)` (0xf0d84), from the end of a
    /// TraceAttack (`attacker` is the TraceAttack's `pevAttacker`). The type keeps every bit since
    /// the last Clear, even across a change of victim; a different victim first gets the damage
    /// collected so far applied to the old one, with `attacker` as inflictor and attacker.
    pub fn add(attacker: *mut edict_t, victim: *mut edict_t, amount: f32, bits: i32) {
        add_to(&MULTI.0, victim, amount, bits, || {
            apply(attacker, attacker);
            // Port addition (#327): the new victim's pellets and flags start from none.
            PELLET_COUNTS.0.set([0; 8]);
            DAMAGE_FLAGS.0.set(0);
        });
    }

    /// `ApplyMultiDamage` on `cell`/`headshot`; `take` is the victim's `TakeDamage`.
    fn apply_from(
        cell: &Cell<MultiDamage>,
        headshot: &Cell<bool>,
        inflictor: *mut edict_t,
        attacker: *mut edict_t,
        take: impl FnOnce(*mut edict_t, &DamageInfo) -> bool,
    ) -> bool {
        let m = cell.get();
        let Some(info) = m.take_damage_info(inflictor, attacker) else {
            return false;
        };
        let was = APPLYING.0.replace(true);
        let took = take(m.entity, &info);
        APPLYING.0.set(was);
        headshot.set(false);
        took
    }

    /// `ApplyMultiDamage(inflictor, attacker)` (0xf0d38): the victim's `TakeDamage` with the sum,
    /// then `glast_headshot = 0`. The multi-damage is not cleared, so a second Apply would hit
    /// again (no TS caller does that). Returns what `TakeDamage` returned.
    pub fn apply(inflictor: *mut edict_t, attacker: *mut edict_t) -> bool {
        apply_from(&MULTI.0, &LAST_HEADSHOT.0, inflictor, attacker, |victim, info| {
            // Guard: TS keeps a raw `CBaseEntity *`; an edict freed since its TraceAttack has no
            // object to call.
            !victim.is_free() && crate::ham::take_damage(victim, info).unwrap_or(false)
        })
    }

    /// `glast_headshot = 1` (`CBasePlayer::TraceAttack`, 0x90341): a bullet or thrown hit in the
    /// head since the last `ApplyMultiDamage` with a victim.
    pub fn set_headshot() {
        LAST_HEADSHOT.0.set(true);
    }

    /// `glast_headshot`, which `PlayerKilled` (0x895e4) reads for the `#TS_Headshot` message.
    pub fn headshot() -> bool {
        LAST_HEADSHOT.0.get()
    }

    /// Port addition (#327): records a TraceAttack pellet for its hitgroup.
    pub fn add_pellet(hitgroup: i32) {
        let mut p = PELLET_COUNTS.0.get();
        let i = hitgroup.clamp(0, 7) as usize;
        p[i] = p[i].saturating_add(1);
        PELLET_COUNTS.0.set(p);
    }

    /// Port addition (#327): the per-hitgroup pellet counts since the last `clear`.
    pub fn pellets() -> [u8; 8] {
        PELLET_COUNTS.0.get()
    }

    /// Port addition (#327): adds `damage_flags` bits for this multi-damage.
    pub fn add_damage_flags(flags: i32) {
        DAMAGE_FLAGS.0.set(DAMAGE_FLAGS.0.get() | flags);
    }

    /// Port addition (#327): whether the damage being dealt is `ApplyMultiDamage`'s.
    pub fn applying() -> bool {
        APPLYING.0.get()
    }

    /// Port addition (#327): the damage flags since the last `clear`.
    pub fn damage_flags() -> i32 {
        DAMAGE_FLAGS.0.get()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::damage::bits;

        fn fake(n: usize) -> *mut edict_t {
            std::ptr::without_provenance_mut(n * 0x100)
        }

        #[test]
        fn pellets_on_one_victim_sum_into_one_take_damage() {
            let cell = Cell::new(MultiDamage::EMPTY);
            let hs = Cell::new(true);
            let (shooter, a) = (fake(1), fake(2));
            cell.set(MultiDamage { bits: 0x1002, ..MultiDamage::EMPTY });
            for _ in 0..3 {
                // Only the first Add "flushes", the cleared (null) victim.
                add_to(&cell, a, 7.5, 0x0200_1002, || assert!(cell.get().entity.is_null(), "same victim never flushes"));
            }
            let mut calls = Vec::new();
            let took = apply_from(&cell, &hs, shooter, shooter, |v, info| {
                calls.push((v, info.damage, info.bits, info.traced));
                true
            });
            assert!(took);
            assert_eq!(calls, vec![(a, 22.5, 0x0200_1002, false)]);
            assert!(!hs.get(), "Apply resets glast_headshot");
            // Not cleared: a second Apply hits again.
            assert!(apply_from(&cell, &hs, shooter, shooter, |_, info| info.damage == 22.5));
        }

        #[test]
        fn take_damage_gets_only_the_four_arguments() {
            // TS's `TakeDamage(pevInflictor, pevAttacker, flDamage, bitsDamageType)`: no hitgroup,
            // direction, impact point or weapon survives the multi-damage.
            let (inflictor, attacker) = (fake(1), fake(4));
            let m = MultiDamage { entity: fake(2), amount: 12.0, bits: bits::CLUB };
            let info = m.take_damage_info(inflictor, attacker).expect("a victim");
            assert_eq!((info.inflictor, info.attacker, info.damage, info.bits), (inflictor, attacker, 12.0, bits::CLUB));
            assert_eq!((info.hitgroup, info.dir, info.pos, info.traced), (crate::damage::hitgroup::GENERIC, [0.0; 3], [0.0; 3], false));
            assert_eq!(MultiDamage::EMPTY.take_damage_info(inflictor, attacker).map(|i| i.damage), None);
        }

        #[test]
        fn a_new_victim_flushes_the_old_one_first() {
            let cell = Cell::new(MultiDamage::EMPTY);
            let (a, b) = (fake(2), fake(3));
            add_to(&cell, a, 10.0, bits::BULLET, || {});
            let mut flushed = None;
            add_to(&cell, b, 4.0, bits::CLUB, || flushed = Some(cell.get()));
            let f = flushed.expect("victim changed");
            assert_eq!((f.entity, f.amount), (a, 10.0));
            assert_eq!(f.bits, bits::BULLET | bits::CLUB, "the old victim gets the new bits too");
            let m = cell.get();
            assert_eq!((m.entity, m.amount, m.bits), (b, 4.0, bits::BULLET | bits::CLUB));
        }

        #[test]
        fn no_victim_nothing_applied() {
            let cell = Cell::new(MultiDamage::EMPTY);
            let hs = Cell::new(true);
            add_to(&cell, std::ptr::null_mut(), 5.0, bits::BULLET, || panic!());
            assert_eq!(cell.get().bits, 0, "a null victim adds nothing, not even its bits");
            assert!(!apply_from(&cell, &hs, fake(1), fake(1), |_, _| panic!("no victim")));
            assert!(hs.get(), "glast_headshot is only reset with a victim");
        }

        #[test]
        fn pellets_and_flags_only_while_applying() {
            // #327: a direct hit (not `ApplyMultiDamage`'s) mustn't report the last shot's pellets.
            clear();
            add_pellet(1);
            add_pellet(1);
            add_pellet(9); // out of range: clamped to the last group
            add_damage_flags(0x2);
            assert!(!applying());
            let cell = Cell::new(MultiDamage { entity: fake(2), amount: 8.0, bits: bits::BULLET });
            let hs = Cell::new(false);
            apply_from(&cell, &hs, fake(1), fake(1), |_, _| {
                assert!(applying());
                assert_eq!(pellets(), [0, 2, 0, 0, 0, 0, 0, 1]);
                assert_eq!(damage_flags(), 0x2);
                true
            });
            assert!(!applying());
            clear();
            assert_eq!((pellets(), damage_flags()), ([0; 8], 0));
        }

        #[test]
        fn the_first_add_after_a_clear_flushes_nothing_real() {
            // The "old victim" of the first Add is the cleared null entity: its Apply is a no-op.
            let cell = Cell::new(MultiDamage::EMPTY);
            let hs = Cell::new(false);
            add_to(&cell, fake(2), 1.0, 0, || assert!(!apply_from(&cell, &hs, fake(1), fake(1), |_, _| panic!())));
            assert_eq!(cell.get().amount, 1.0);
        }
    }
}

/// `g_vecAttackDir` (0x190ee0, zero at load): the last attack direction `CBaseMonster::TakeDamage`
/// (0xb2430) computed, from the victim towards just below the inflictor's centre. Only a hit with
/// a real inflictor (not NULL, not the world) writes it, so it keeps the previous hit's value
/// otherwise. `CBasePlayer::Killed` (0x8f2a0..0x8f380) pushes the body against it and the death
/// pose (`CTSAnimation::Animate(7)`) reads it.
static ATTACK_DIR: goldsrc::Global<std::cell::Cell<[f32; 3]>> = goldsrc::Global(std::cell::Cell::new([0.0; 3]));

/// `g_vecAttackDir`.
pub fn attack_dir() -> [f32; 3] {
    ATTACK_DIR.0.get()
}

/// Writes `g_vecAttackDir`, as `CBaseEntity::TakeDamage`, `CBreakable::TakeDamage`/`Use` and
/// `DeadTakeDamage` do with their own directions.
pub fn set_attack_dir(dir: [f32; 3]) {
    ATTACK_DIR.0.set(dir);
}

/// mp.dll's out-of-line `Vector::Normalize` (0x100148f0), which the stock entities call:
/// `x² + y² + z²`, the length and `1 / length` kept on the FPU, each component stored; `(0, 0, 1)`
/// for a zero (or NaN) length. The port used to store the length and its inverse as floats.
pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    Msvc::vec_normalize(v, ORDER_XYZ)
}

/// `CBaseEntity::Center`: `(absmax + absmin) * 0.5`.
fn center(absmin: [f32; 3], absmax: [f32; 3]) -> [f32; 3] {
    [(absmax[0] + absmin[0]) * 0.5, (absmax[1] + absmin[1]) * 0.5, (absmax[2] + absmin[2]) * 0.5]
}

/// The two ends of the direction `CBaseMonster::TakeDamage` stores in `g_vecAttackDir`,
/// `(inflictor->Center() - (0, 0, 10) - Center()).Normalize()` normalised a second time: the
/// inflictor's centre 10 units down (stored as `float`s) and the victim's centre. The ruleset
/// does the arithmetic ([`ServerRules::attack_dir`](crate::gameplay::ServerRules::attack_dir)).
pub fn attack_dir_ends(inflictor: (&[f32; 3], &[f32; 3]), victim: (&[f32; 3], &[f32; 3])) -> ([f32; 3], [f32; 3]) {
    let from = center(*inflictor.0, *inflictor.1);
    ([from[0], from[1], from[2] - 10.0], center(*victim.0, *victim.1))
}

/// The `g_vecAttackDir` update of `CBaseMonster::TakeDamage` (0xb2430) for `victim` (after its
/// `takedamage` test): only for an inflictor that isn't NULL or the world (`FNullEnt`) and has an
/// object (`CBaseEntity::Instance`).
pub fn update_attack_dir(inflictor: *mut edict_t, victim: &entvars_t) {
    if inflictor.is_null() || goldsrc::server::engine().index_of(inflictor) == 0 || !inflictor.has_private_data() {
        return;
    }
    let iv = inflictor.v();
    let (from, to) = attack_dir_ends((&iv.absmin, &iv.absmax), (&victim.absmin, &victim.absmax));
    ATTACK_DIR.0.set(crate::gameplay::active().server.attack_dir(from, to));
}

/// TS's melee and bullet globals that `CBasePlayer::Killed` and `PlayerKilled` read:
/// `glast_closecombat` (0x114480), `glast_coldcock` (0x114481) and `glast_weaponid` (0x11100c).
/// Each is set right before one Clear/TraceAttack/Apply and reset right after it, so it is only
/// seen by a `TakeDamage` (and the `Killed` inside it) that this one hit causes.
pub mod glast {
    use goldsrc::Global;
    use std::cell::Cell;

    static CLOSE_COMBAT: Global<Cell<bool>> = Global(Cell::new(false));
    static COLDCOCK: Global<Cell<bool>> = Global(Cell::new(false));
    /// -1 (no simulated bullet) at load, like TS's initialised `.data` word.
    static WEAPON_ID: Global<Cell<i32>> = Global(Cell::new(-1));

    /// Sets `cell` to `on` for `f`, then to `off` (TS stores the constant, it doesn't restore).
    fn during<T: Copy, R>(cell: &Cell<T>, on: T, off: T, f: impl FnOnce() -> R) -> R {
        cell.set(on);
        let r = f();
        cell.set(off);
        r
    }

    /// `glast_closecombat = 1` around the hit: `CBasePlayer::CloseCombatAttack` (0x95f88, set at
    /// 0x96490) and the kung-fu hit of `CTSStunt::CheckForBreakables` (0x9bd9c, set when the
    /// stored attack is kung fu, `pl+0x7a4 == 0`; reset at the function's end).
    pub fn close_combat<R>(f: impl FnOnce() -> R) -> R {
        during(&CLOSE_COMBAT.0, true, false, f)
    }

    /// `glast_coldcock = 1` around the stock hit of `CBasePlayer::ColdCockAttack` (0x96610, set at 0x96a29),
    /// when the victim didn't block it.
    pub fn coldcock<R>(f: impl FnOnce() -> R) -> R {
        during(&COLDCOCK.0, true, false, f)
    }

    /// `glast_weaponid = bullet+0x30` around a simulated bullet's hit
    /// (`CBulletManager::UpdateBullet`, 0x6f2ad..0x6f311), -1 after it.
    pub fn weapon_id<R>(id: i32, f: impl FnOnce() -> R) -> R {
        during(&WEAPON_ID.0, id, -1, f)
    }

    /// The globals at one moment, with `glast_headshot`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Snapshot {
        pub headshot: bool,
        pub close_combat: bool,
        pub coldcock: bool,
        /// `glast_weaponid`, -1 outside a simulated bullet's hit.
        pub weapon_id: i32,
    }

    impl Default for Snapshot {
        fn default() -> Self {
            Self { headshot: false, close_combat: false, coldcock: false, weapon_id: -1 }
        }
    }

    /// The globals now. `Killed` takes this as it starts, so a `PlayerKilled` the port has to run
    /// later (the killer is mid-think) still sees the values of the kill.
    pub fn snapshot() -> Snapshot {
        Snapshot {
            headshot: super::multi::headshot(),
            close_combat: CLOSE_COMBAT.0.get(),
            coldcock: COLDCOCK.0.get(),
            weapon_id: WEAPON_ID.0.get(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn set_for_one_hit_then_reset_not_restored() {
            let flag = Cell::new(false);
            assert!(during(&flag, true, false, || flag.get()));
            assert!(!flag.get());
            // A nested hit (a Killed's own damage) leaves the flag off for the rest of the outer
            // one, as TS's `glast_* = 0` does.
            let after_nested = during(&flag, true, false, || {
                during(&flag, true, false, || ());
                flag.get()
            });
            assert!(!after_nested);
            let id = Cell::new(-1);
            assert_eq!(during(&id, 12, -1, || id.get()), 12);
            assert_eq!(id.get(), -1);
            assert_eq!(Snapshot::default().weapon_id, -1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traced_is_explicit() {
        let null = std::ptr::null_mut();
        let direct = DamageInfo::new(null, null, 10.0, bits::BULLET);
        assert!(!direct.traced, "bits and hitgroup don't imply a TraceAttack");
        let traced = DamageInfo::new(null, null, 10.0, bits::BLAST).traced();
        assert!(traced.traced, "an occluded blast keeps hitgroup GENERIC but is still traced");
        assert_eq!(traced.hitgroup, hitgroup::GENERIC);
        assert_eq!((traced.damage, traced.bits), (10.0, bits::BLAST));
    }

    #[test]
    fn attack_dir_ends_are_the_centres_the_inflictor_10_down() {
        let victim = ([-16.0, -16.0, -36.0], [16.0, 16.0, 36.0]);
        let inflictor = ([96.0, -4.0, 6.0], [104.0, 4.0, 14.0]);
        assert_eq!(attack_dir_ends((&inflictor.0, &inflictor.1), (&victim.0, &victim.1)), ([100.0, 0.0, 0.0], [0.0; 3]));
        assert_eq!(normalize([0.0; 3]), [0.0, 0.0, 1.0]);
        let n = normalize([3.0, 0.0, 4.0]);
        assert!((n[0] - 0.6).abs() < 1e-7 && n[1] == 0.0 && (n[2] - 0.8).abs() < 1e-7);
    }
}
