//! TS 2.1's melee (issue #100 §3.2, checklist item 5): kung fu, the katana, the knife slash and
//! the stock hit, as whole-function overrides of `MeleeRules` (the gun code both sides run).
//!
//! Decoded from the 2.1 `mp.dll` (decomp and disassembly): `CCombatAttack` `FUN_1004de10`,
//! `CCombatBlock` `FUN_1004dc70`, `KatanaFire` `FUN_1004c090`, `KnifeFire` `FUN_1004c6c0`,
//! `CTSGun::AlternateAttack` 0x1004ab40 with `ColdCockAttack` `FUN_100572a0`, and their callers
//! `CTSGun::PrimaryAttack` 0x1004aa50 / `SecondaryAttack` 0x1004a800.
//!
//! 2.1 keeps its weapon timers as absolute times with the delays fixed when they're set:
//! `delay × (Double Fire Rate ? 0.5 : 1) × (own Slow Pause ? 6.6666665 : T)`, `T` the time
//! scale's inverse ([`time_scale_inverse`]). The port's fire timers count down, so they get the
//! same delays, run down by the frame time (the gun timer model, checklist item 3). The
//! delayed-hit window and the block timers (`Gun::melee_window`, `block_*`) are end times in 2.1
//! mode, as in 2.1 (issue #202): `(float)(d + time)` with the duration `d` unrounded
//! ([`whiff_end`], [`block_end`]), compared with the current time wherever they're read (the
//! window's expiry in [`pre_think_step`](crate::shared::gun::pre_think_step), the blocks at the
//! attacker's think in the server's `melee_block`). 0 is none, as for 3.0's countdowns.
//!
//! The stunt timers (`BlockPlayer`, the `3·T` hold of the crouched moves, `pl+0x470`) are
//! started in seconds of the player's time: the server's stunt clock applies `T`.
//!
//! Third-person animations use the port's (3.0's) animation ids for the same moves (the 2.1
//! table numbers the blade swings differently); the kung-fu move ids are the same in both.

use ::shared::ts::powerup;
use ::shared::weapons::gun::{MeleeHit, MeleeKind, MeleeResult, Sound};
use ::shared::weapons::spread;
use ::shared::weapons::{Event, EventArgs, Gun, GunHost, kind};

const IN_ATTACK: u32 = 1;
const IN_DUCK: u32 = 4;
const IN_ATTACK2: u32 = 0x800;
const FL_ONGROUND: i32 = 0x200;

/// `FUN_100551c0` (`FixedSlowMul`): `1 / max(s, 0.1)` for a time scale `s` (1 at normal speed;
/// 2.1's own encoding has 0 for normal and the same value otherwise).
pub fn time_scale_inverse(s: f32) -> f32 {
    1.0 / s.max(0.1)
}

/// The Slow Pause owner's delay factor (0x10088928).
const SLOW_PAUSE_DELAY: f32 = 6.666_666_5;

/// A delay fixed when it's set: `base × (dfr ? 0.5 : 1)` stored as a float, then
/// `× (slow pause ? 6.6666665 : T)` stored as a float (`KnifeFire` 0x1004c6e2..0x1004c732).
pub fn fixed_delay(base: f32, dfr: bool, slow_pause: bool, t: f32) -> f32 {
    let d = (base as f64 * if dfr { 0.5 } else { 1.0 }) as f32;
    (d as f64 * if slow_pause { SLOW_PAUSE_DELAY } else { t } as f64) as f32
}

/// The delayed-hit window of a missed swing (`pl+0x718`): `min(3, 0.3·T)` (doubles 0.3 and 3).
pub fn whiff_window(t: f32) -> f32 {
    let w = t as f64 * 0.3;
    if 3.0 <= w { 3.0 } else { w as f32 }
}

/// The block window a knife or katana swing opens (`pl+0x728` / `pl+0x72c`) and the kung-fu
/// block's (`pl+0x724`): `min(hold·5, T·hold·0.5)`.
pub fn block_window(hold: f32, t: f32) -> f32 {
    let cap = (hold * 5.0) as f64;
    let w = t as f64 * hold as f64 * 0.5;
    if cap <= w { cap as f32 } else { w as f32 }
}

/// [`whiff_window`] as an end time (`KnifeFire` 0x1004ca04, `KatanaFire` 0x1004c5c8,
/// `CCombatAttack`): `(float)(min(3, 0.3·T) + time)`, `T` unrounded (`FUN_100551c0`).
pub fn whiff_end(t: f64, time: f32) -> f32 {
    let w = t * 0.3;
    ((if 3.0 <= w { 3.0 } else { w }) + time as f64) as f32
}

/// [`block_window`] as an end time (knife `pl+0x728`, katana `+0x72c`, mp.dll's kung-fu block
/// `+0x724`): `(float)(min((float)(hold·5), T·hold·0.5) + time)`, `T` and the product unrounded.
pub fn block_end(hold: f32, t: f64, time: f32) -> f32 {
    let cap = (hold * 5.0) as f64;
    let w = t * hold as f64 * 0.5;
    ((if cap <= w { cap } else { w }) + time as f64) as f32
}

/// `CCombatBlock`'s block.wav (`FUN_1004dc70`, client `FUN_100245b0`): when the last block
/// ended over a second ago, `1.0 < time − end` on the FPU. 0 (none yet) is `time` seconds ago.
pub fn block_sound(time: f32, end: f32) -> bool {
    1.0 < time as f64 - end as f64
}

/// `CCombatBlock`'s hold (`DAT_100b2230`, client `DAT_10299698`).
const BLOCK_HOLD: f32 = 0.6;

/// `T` for a melee end time, unrounded as the side computes it.
fn t_end(host: &dyn GunHost) -> f64 {
    crate::shared::gun::side_slow_div(host)
}

fn t_of(host: &dyn GunHost) -> f32 {
    time_scale_inverse(host.slow_mul())
}

fn slow_pause(host: &dyn GunHost) -> bool {
    host.powerups() & powerup::SLOWPAUSE != 0
}

fn on_ground(host: &dyn GunHost) -> bool {
    host.flags() & FL_ONGROUND != 0
}

/// The attacker's motion as a 2.1 attack sees it ([`motion`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    /// `velocity.Length()` stored as a `float`.
    pub speed: f32,
    /// Faster than 100 u/s, as the build tests it.
    pub fast: bool,
    /// Fast, and `dot(normalize(velocity), v_forward) <= −0.5` as the build tests it.
    pub back: bool,
    /// That dot stored as a `float` (0 when not fast).
    pub dot: f32,
}

/// The speed and, above 100 u/s, `dot(normalize(velocity), v_forward)` (`MakeVectors(v_angle)`,
/// the full view angles); a move goes backward when that is at most −0.5. Every sum is at 53 bits
/// (issue #171).
///
/// * mp.dll's `KatanaFire` (0x1004c0cf..0x1004c192) tests the root of `x² + y² + z²` unrounded;
///   its `CCombatAttack` (0x1004de96..0x1004dfe9) stores it first (`katana` picks which). Both
///   normalise through the inline `Normalize`: the katana's squares `(y² + z²) + x²`, kung fu's
///   `(x² + y²) + z²`, the root unrounded, `1 / len` stored as a `float`, the components left on
///   the FPU; the dot sums `z, y, x` and is tested unrounded against the `double` −0.5.
/// * client.dll (unoptimised; katana `FUN_10022cc0`, kung fu `FUN_10024890`) stores the speed
///   from its out-of-line `Length` (0x1001e850), normalises out of line (0x100230c0: the length,
///   `1 / len` and each component stored) and stores the `x, y, z` dot (0x10023170).
///
/// A fast velocity has a length of at least 100, so neither `Normalize`'s zero case arises.
fn motion(host: &dyn GunHost, katana: bool) -> Motion {
    let (fwd, _, _) = spread::angle_vectors(host.v_angle());
    motion_of(host.velocity(), fwd, katana, host.prediction())
}

/// [`motion`] of `vel` against the view's forward vector, on mp.dll or (`client`) client.dll.
fn motion_of(vel: spread::Vec3, fwd: spread::Vec3, katana: bool, client: bool) -> Motion {
    let [x, y, z] = vel.map(f64::from);
    let root = (x * x + y * y + z * z).sqrt();
    let speed = root as f32;
    let fast = if katana && !client { root > 100.0 } else { speed > 100.0 };
    if !fast {
        return Motion { speed, fast, back: false, dot: 0.0 };
    }
    let [fx, fy, fz] = fwd.map(f64::from);
    let (back, dot) = if client {
        let inv = (1.0 / f64::from(speed)) as f32;
        let n = vel.map(|c| f64::from(inv * c));
        let d = (n[0] * fx + n[1] * fy + n[2] * fz) as f32;
        (d <= -0.5, d)
    } else {
        let len = if katana { (y * y + z * z + x * x).sqrt() } else { root };
        let inv = f64::from((1.0 / len) as f32);
        let d = inv * z * fz + inv * y * fy + inv * x * fx;
        (::shared::pm::not_above87(d, -0.5), d as f32)
    };
    Motion { speed, fast, back, dot }
}

/// The rest of a melee hit's fields, as every 2.1 attack fills them.
fn hit(kind: MeleeKind, weapon: u8, reach: f32, hull: i32, damage: f32, bits: i32, anim: i32) -> MeleeHit {
    MeleeHit { kind, weapon, reach, hull, damage, bits, player_anim: anim, disarm: false, knockback: false, zero_roll: false }
}

// ---- the knife -----------------------------------------------------------------------------

/// `KnifeFire` (`FUN_1004c6c0`).
pub fn knife_fire(gun: &mut Gun, host: &mut dyn GunHost) {
    if !gun.has_slot() {
        return;
    }
    let info = gun.info();
    let t = t_of(host);
    // Both timers and the idle are set before anything else, also for a refused slash.
    let d = fixed_delay(info.rate, Gun::double_fire_rate(host), slow_pause(host), t);
    let next = (d as f64 * 0.7) as f32;
    gun.next_primary = next;
    gun.next_secondary = next;
    gun.idle = 3.0;
    host.set_close_combat_cooldown(next);
    if gun.status[gun.current as usize].toggle {
        knife_throw(gun, host);
        return;
    }
    if host.iuser4() & 0x10c20 != 0 {
        return;
    }
    let r = host.random_long(0, 2);
    if on_ground(host) {
        host.player_anim(0x1d + r);
    }
    // Swinging opens a block window against knives (`pl+0x728`).
    gun.block_knife = block_end(info.rate, t_end(host), host.time());
    // 35 for the combat knife, 15 for the seal knife (0x1004c78a: `btype == 6 ? 35 : 15`).
    let dmg = if info.bullet == 6 { 35.0 } else { 15.0 };
    match host.melee(&hit(MeleeKind::Knife, gun.current, 38.0, 3, dmg, 0x1080, -1), gun) {
        MeleeResult::Hit { player } => {
            if player {
                gun.status[gun.current as usize].state = 1;
                gun.body = 1;
                host.sound(Sound::KnifeHitBody);
            }
        }
        MeleeResult::Blocked { .. } => gun.melee_damage = 0.0,
        MeleeResult::Miss => {
            gun.melee_window = whiff_end(t_end(host), host.time());
            gun.melee_damage = dmg;
            gun.melee_source = 1;
        }
    }
    let st = gun.status[gun.current as usize].state;
    host.event(Event::Knife, EventArgs { i1: gun.current as i32, i2: st, ..Default::default() });
}

/// The throw half of `KnifeFire`: the blade flies (`CTSKnife::CreateKnife`; its speed and damage
/// are the server's `thrown_blade` hooks), the stack shrinks unless Infinite Clip is on, and an
/// empty stack switches to the best weapon (`FUN_10049fb0`).
fn knife_throw(gun: &mut Gun, host: &mut dyn GunHost) {
    let info = gun.info();
    host.throw_blade(info.bullet);
    if host.iuser4() & 0x30c30 == 0 && on_ground(host) {
        host.player_anim(0x20);
    }
    host.sound(Sound::KnifeMiss);
    if !Gun::infinite_clip(host) {
        host.slots_changed(info.slots);
        let s = &mut gun.status[gun.current as usize];
        s.clip = s.clip.wrapping_sub(1);
    }
    if gun.status[gun.current as usize].clip < 1 {
        gun.remove_weapon(gun.current, true, host);
    } else {
        host.weapon_anim(gun.seq(20), true, gun.body);
        let items = gun.status[gun.current as usize].items as i32;
        host.event(Event::Draw, EventArgs { i1: gun.current as i32, i2: items, ..Default::default() });
        gun.idle = 2.0;
    }
}

// ---- the katana ----------------------------------------------------------------------------

/// One katana swing (`FUN_1004c090` 0x1004c1a0..0x1004c35d).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KatanaSwing {
    /// The strength before `× 0.85`; negative swings behind.
    pub base: i32,
    pub reach: f32,
    /// The delay before `× 0.7`, and the block window's base.
    pub hold: f32,
    /// The port's player animation and the event's view-animation index.
    pub anim: i32,
    pub view: i32,
    /// `iuser4` bits set (held for 3 s of the player's time) and the crouched view.
    pub bits: i32,
    pub crouch: bool,
}

/// Picks the swing: backward above 100 u/s (`dot <= −0.5`), airborne, crouched, or the ground
/// swings alternating (`gun+0xdb0`, re-rolled 1..2 when out of range).
pub fn katana_swing(fast: bool, back: bool, on_ground: bool, ducking: bool, last: &mut i32, roll: &mut dyn FnMut() -> i32) -> KatanaSwing {
    let base = if fast { 35 } else { 70 };
    let mut s = KatanaSwing { base, reach: 48.0, hold: 0.5, anim: 33, view: 0x11, bits: 0, crouch: false };
    if !(1..=2).contains(last) {
        *last = roll();
    }
    if *last == 1 {
        *last = 2;
    } else {
        *last = 1;
        (s.anim, s.view) = (34, 0x10);
    }
    if back {
        s = KatanaSwing { base: -100, reach: 54.0, hold: 1.0, anim: 35, view: 0xf, bits: 1, crouch: false };
    } else if !on_ground {
        (s.anim, s.view) = (36, 0x11);
    } else if ducking {
        s = KatanaSwing { base: 90, reach: 48.0, hold: 1.0, anim: 37, view: 0x11, bits: 4 | 1, crouch: true };
    }
    s
}

/// The swing's damage: `trunc(base × 0.85)` (0x10088938, `__ftol`): 29 moving, 59 standing, 76
/// crouched, −85 backward.
pub fn katana_damage(base: i32) -> i32 {
    (base as f64 * 0.85) as i32
}

/// `KatanaFire` (`FUN_1004c090`): no throw and no block in 2.1.
pub fn katana_fire(gun: &mut Gun, host: &mut dyn GunHost) {
    if !gun.has_slot() || host.iuser4() & 0x30c20 != 0 {
        return;
    }
    let t = t_of(host);
    let m = motion(host, true);
    // The crouch test reads `IN_DUCK` or `FL_DUCKING` on the gun entity's own pev, which never has
    // it, so only `+duck` counts.
    let ducking = host.buttons() & IN_DUCK != 0;
    let mut last = gun.last_melee_anim;
    let ground = on_ground(host);
    let swing = {
        let mut roll = || host.random_long(1, 2);
        katana_swing(m.fast, m.back, ground, ducking, &mut last, &mut roll)
    };
    gun.last_melee_anim = last;
    if swing.bits != 0 {
        host.melee_hold(swing.bits, 3.0, swing.crouch);
    }
    host.player_anim(swing.anim);
    gun.block_katana = block_end(swing.hold, t_end(host), host.time());
    let v = katana_damage(swing.base);
    let behind = v < 0;
    let reach = if behind { -swing.reach } else { swing.reach };
    match host.melee(&hit(MeleeKind::Katana, gun.current, reach, if behind { 1 } else { 3 }, v.unsigned_abs() as f32, 0x1080, -1), gun) {
        MeleeResult::Hit { player } => {
            if player {
                gun.status[gun.current as usize].state = 1;
                gun.body = 1;
                host.sound(Sound::KatanaHitBody);
            }
        }
        MeleeResult::Blocked { .. } => gun.melee_damage = 0.0,
        MeleeResult::Miss => {
            gun.melee_window = whiff_end(t_end(host), host.time());
            gun.melee_damage = v as f32;
            gun.melee_source = 2;
        }
    }
    let st = gun.status[gun.current as usize].state;
    host.event(Event::Katana, EventArgs { i1: swing.view, i2: st, ..Default::default() });
    let d = fixed_delay(swing.hold, Gun::double_fire_rate(host), slow_pause(host), t);
    let next = (d as f64 * 0.7) as f32;
    gun.next_primary = next;
    gun.next_secondary = next;
    gun.idle = 3.0;
    host.set_close_combat_cooldown(next);
}

// ---- kung fu -------------------------------------------------------------------------------

/// A kung-fu move (`FUN_1004de10`; the table is the decomp's `docs/ts21/weapons.md` §5.4,
/// checked against the disassembly: reaches 38/48/56 at `esp+0x18`, the clamps' floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KungFuMove {
    /// 1 punch, 2 kick, 3 running back kick (`DAT_100b1e54`); 0 none.
    pub atype: i32,
    /// The move id (`DAT_100bbe9c`): the attack's player animation and what the combos chain on.
    pub move_id: i32,
    pub damage: f32,
    pub reach: f32,
    /// The delay before `× T` (`DAT_100b1e90`, 0.6 when nothing is done).
    pub delay: f32,
    /// `iuser4` bits and the crouched view, held for 3 s of the player's time (the sweep and
    /// the flying kick).
    pub bits: i32,
}

/// `clamp(x, lo, hi)` as the code writes it: `< lo → lo`, `> hi → hi`, on the x87 stack.
fn clamp(x: f64, lo: f64, hi: f64) -> f32 {
    (if x > hi {
        hi
    } else if x >= lo {
        x
    } else {
        lo
    }) as f32
}

/// What the 2.1 kung-fu code reads from the player for [`kungfu_move`].
#[derive(Clone, Copy, Debug)]
pub struct KungFuInput {
    pub buttons: u32,
    pub pressed: u32,
    pub iuser4: i32,
    pub on_ground: bool,
    pub speed: f32,
    /// `dot(normalize(velocity), v_forward)` above 100 u/s, else 0.
    pub dot: f32,
    /// Above 100 u/s with that dot at most −0.5, as the build tests it ([`Motion::back`]).
    pub back: bool,
}

/// Picks the move, updating the combo counters (`gun+0xda8` punches, `+0xdac` kicks) from the
/// last move (`gun+0xdb0`). `roll(lo, hi)` is `RANDOM_LONG`.
pub fn kungfu_move(
    i: &KungFuInput,
    punch_combo: &mut i32,
    kick_combo: &mut i32,
    last: i32,
    roll: &mut dyn FnMut(i32, i32) -> i32,
) -> KungFuMove {
    let speed = i.speed as f64;
    let back = i.back;
    let mut m = KungFuMove { atype: 0, move_id: 0, damage: 0.0, reach: 0.0, delay: 0.6, bits: 0 };
    if i.buttons & IN_ATTACK != 0 {
        m.atype = 1;
        m.reach = 38.0;
        if i.iuser4 & 0x10 == 0 && i.on_ground {
            if *punch_combo < 5 {
                *punch_combo += 1;
                (m.move_id, m.damage, m.delay) = (0x16, 10.0, 0.15);
            } else {
                *punch_combo = 0;
                (m.move_id, m.delay) = (0x17, 0.2);
                m.damage = clamp(speed * 0.066_666_666_666_666_67, 10.0, 20.0);
            }
            return m;
        }
        // Stunting keeps move 0; airborne is the jumping punch.
        if i.iuser4 & 0x10 == 0 {
            m.move_id = 0x18;
        }
        m.damage = clamp(speed * 0.1, 10.0, 30.0);
        m.delay = 0.3;
        return m;
    }
    if i.buttons & IN_ATTACK2 == 0 {
        return m;
    }
    m.reach = 48.0;
    if i.iuser4 & 0xc00 != 0 {
        (m.atype, m.delay) = (2, 1.0);
        // The `float` 0.1 here (0x1004e188 `fmul dword [0x1008018c]`; issue #150), where the
        // punches use the `double`.
        m.damage = clamp(speed * 0.1f32 as f64, 10.0, 30.0);
    } else if i.on_ground && i.buttons & IN_DUCK != 0 {
        (m.atype, m.move_id, m.delay, m.damage, m.bits) = (2, 0x14, 1.0, 30.0, 4 | 1);
    } else if back {
        (m.atype, m.move_id, m.delay, m.reach) = (3, 0x11, 0.9, 56.0);
        m.damage = clamp(speed * 0.303_030_312_061_309_8, 10.0, 90.0);
    } else if !i.on_ground && i.speed > 270.0 && i.buttons & IN_DUCK != 0 && i.dot > 0.5 {
        *kick_combo = 0;
        (m.atype, m.move_id, m.delay, m.bits) = (2, 0x10, 0.9, 4);
        m.damage = clamp(speed * 0.153_846_159_577_369_7, 10.0, 45.0);
    } else if i.on_ground || i.speed <= 200.0 {
        m.atype = 2;
        // Never true in 2.1: CCombatAttack only runs on a fresh press (`pl+0x770`).
        if i.pressed & IN_ATTACK2 == 0 && *kick_combo < 3 {
            *kick_combo += 1;
            (m.damage, m.delay) = (10.0, 0.25);
            m.move_id = match last {
                0xb => 0xc,
                0xc => 0xb,
                _ => [0xb, 0xc][(roll(0, 1) & 1) as usize],
            };
        } else {
            *kick_combo = 0;
            m.delay = 0.5;
            m.move_id = match last {
                0xf => 10,
                10 => 0xe,
                0xe => 0xd,
                0xd => 0xf,
                _ => [0xe, 0xd, 10, 0xf][roll(0, 3).clamp(0, 3) as usize],
            };
            m.damage = clamp(speed * 0.066_666_666_666_666_67, 10.0, 20.0);
        }
    } else {
        *kick_combo = 0;
        (m.atype, m.delay) = (2, 0.7);
        m.damage = clamp(speed * 0.1, 10.0, 30.0);
        m.move_id = match last {
            0xf => 0x13,
            0x13 => 0x12,
            0x12 => 0xf,
            _ => [0x13, 0x12, 0xf][roll(0, 2).clamp(0, 2) as usize],
        };
    }
    m
}

/// The hit's damage and the bar's cost (0x1004e2dd..): `cost = base × 0.9` (stored as a float),
/// `damage = base × 0.9 × bar × 0.01` on the x87 stack, so an empty bar deals nothing.
pub fn kungfu_damage(base: f32, bar: f32) -> (f32, f32) {
    let c = base as f64 * 0.9;
    (((c * bar as f64) * 0.01f32 as f64) as f32, c as f32)
}

/// The 3.0 client's view-animation index for the move's event (`CloseCombat` `iparam2`), so the
/// port and 3.0 clients show the matching move.
fn view_anim(m: &KungFuMove) -> i32 {
    match (m.atype, m.move_id) {
        (_, 0x16) => 3,
        (_, 0x17) => 11,
        (1, _) => 5,
        (_, 0x14) => 6,
        (_, 0x11) => 7,
        (_, 0x10) => 8,
        (_, 0xb | 0xc) => 9,
        _ => 10,
    }
}

/// The kung-fu attack's `BlockPlayer` argument (0x1004e98f..0x1004e9a4): `FixedSlowMul()` (left on
/// the FPU) × the delay × the `float` 0.9 (0x3f666666), stored as a `float`.
pub fn block_duration(slow_div: f64, delay: f32) -> f32 {
    (slow_div * f64::from(delay) * f64::from(0.9f32)) as f32
}

/// `CCombatAttack` (`FUN_1004de10`).
pub fn ccombat_attack(gun: &mut Gun, host: &mut dyn GunHost) {
    // `PrimaryAttack` / `SecondaryAttack` enter only on a fresh press.
    if host.pressed() & (IN_ATTACK | IN_ATTACK2) == 0 {
        return;
    }
    if gun.next_attack > 0.0 || gun.next_primary > 0.0 || gun.next_secondary > 0.0 || host.iuser4() & 0x70c30 != 0 {
        return;
    }
    let t = t_of(host);
    let mo = motion(host, false);
    let input = KungFuInput {
        buttons: host.buttons(),
        pressed: host.pressed(),
        iuser4: host.iuser4(),
        on_ground: on_ground(host),
        speed: mo.speed,
        dot: mo.dot,
        back: mo.back,
    };
    let (mut pc, mut kc) = (gun.punch_combo, gun.kick_combo);
    let m = {
        let mut roll = |lo, hi| host.random_long(lo, hi);
        kungfu_move(&input, &mut pc, &mut kc, gun.last_melee_anim, &mut roll)
    };
    (gun.punch_combo, gun.kick_combo) = (pc, kc);
    // Port addition (plugin API version 5): plugins see the move's delay (before the time-scale
    // factor `T`) and the damage a hit deals, and may change or block it, as on the 3.0 path.
    // The damage and the doubling are pure, so working them out before the hold changes nothing
    // 2.1 does; a blocked move holds nothing, spends no bar and plays nothing.
    let mut delay = m.delay;
    let (mut dmg, cost) = kungfu_damage(m.damage, gun.melee_bar);
    let mut go = m.atype != 0;
    if go {
        if host.kungfu() {
            dmg = (dmg as f64 * 2.0) as f32;
        }
        go = host.melee_attack_pre(gun, &mut delay, &mut dmg);
    }
    if go {
        if m.bits != 0 {
            host.melee_hold(m.bits, 3.0, true);
        }
        gun.last_melee_anim = m.move_id;
        // The One's bar isn't spent (`pl+0x731`); the bar is sent either way.
        if !host.the_one() {
            let b = gun.melee_bar - cost;
            gun.melee_bar = if b < 0.0 { 0.0 } else { b };
        }
        host.melee_bar_changed(gun.melee_bar);
        let back = m.atype == 3;
        let mut h = hit(MeleeKind::KungFu, 0, if back { -m.reach } else { m.reach }, if back { 1 } else { 3 }, dmg, 0x0400_1080, m.move_id);
        // CloseCombatAttack's flags: disarm (the server's `melee_disarm` applies 2.1's `dmg > 15`),
        // knockback, any target, teammates by `mp_friendlyfire`.
        h.disarm = true;
        h.knockback = true;
        h.zero_roll = back;
        match host.melee(&h, gun) {
            MeleeResult::Hit { .. } => {}
            MeleeResult::Blocked { next_attack, bar_cost } => blocked(gun, host, next_attack, bar_cost),
            MeleeResult::Miss => {
                gun.melee_window = whiff_end(t_end(host), host.time());
                gun.melee_damage = if back { -dmg } else { dmg };
                gun.melee_source = 0;
            }
        }
        // `BlockPlayer` (`FUN_10056b60`) only acts on the ground. Its argument (0x1004e98f..
        // 0x1004e9a4) is `(float)(FixedSlowMul() × delay × 0.9f)`, the slowed duration; the stunt
        // clock adds the time to it as a `float` (`pl+0x460 = t + time`, issue #242).
        if host.iuser4() & 1 == 0 && on_ground(host) {
            host.block_player(block_duration(t_end(host), delay));
        }
        host.event(Event::CloseCombat, EventArgs { f1: 0.0, f2: 0.0, i1: m.atype, i2: view_anim(&m), b1: 0, b2: 0 });
    }
    let d = (t as f64 * delay as f64) as f32;
    gun.next_primary = d;
    gun.next_secondary = d;
    // 2.1 stores `d`, but its kung-fu idle (`FUN_1004dc40`) only re-arms that timer, so it times
    // nothing else. The port's kung-fu idle plays 3.0's idle animation on `v_melee.mdl`, which
    // would cut a move's 3.0 view animation short after `d` (0.15 s for a jab): a played move
    // waits 3.0's 2 s (visual compatibility deviation, docs/implementation/ts21.md).
    gun.idle = if go { 2.0 } else { d };
    host.set_close_combat_cooldown(d);
}

/// The attacker's side of a 2.1 block (the server's `melee_block`): its stored hit is dropped;
/// when it was the one blocking (`next_attack > 0`), it is held up and its bar pays.
fn blocked(gun: &mut Gun, host: &mut dyn GunHost, next_attack: f32, bar_cost: f32) {
    gun.melee_damage = 0.0;
    if next_attack > 0.0 {
        gun.next_attack = next_attack;
    }
    if bar_cost > 0.0 {
        let b = gun.melee_bar - bar_cost;
        gun.melee_bar = if b < 0.0 { 0.0 } else { b };
        host.melee_bar_changed(gun.melee_bar);
    }
}

/// `CCombatBlock` (`FUN_1004dc70`), from `Reload` with kung fu only: a blade's reload key does
/// nothing. No bar cost and no animation (2.1 mode plays 3.0's); block for `min(3, 0.3·T)` (until that past the time,
/// [`block_end`]), and so long before the next attack. block.wav when the last block ended over
/// a second ago ([`block_sound`]). Server only in 2.1 mode: client.dll's copy (`FUN_100245b0`,
/// which keeps `pl+0x724` the same way) is never reached, its `ItemPostFrame` (`FUN_10025f40`)
/// having no kung-fu reload key ([`Predicted::kungfu_reload`](::shared::gameplay::Predicted)).
pub fn ccombat_block(gun: &mut Gun, host: &mut dyn GunHost) {
    if gun.has_slot() {
        return;
    }
    if gun.next_attack > 0.0 || gun.next_primary > 0.0 || gun.next_secondary > 0.0 || gun.next_alt > 0.0 || host.iuser4() & 0x70c30 != 0 {
        return;
    }
    let time = host.time();
    if block_sound(time, gun.block_kungfu) {
        host.sound(Sound::Block);
    }
    gun.block_kungfu = block_end(BLOCK_HOLD, t_end(host), time);
    let w = block_window(BLOCK_HOLD, t_of(host));
    gun.next_primary = w;
    gun.next_secondary = w;
    // 2.1 plays no block animation (it has no kung-fu view model). 2.1 mode plays 3.0's
    // `v_melee.mdl` block, set A `[1]` as 3.0's close-combat event does (0x100433bb), sent to the
    // owner because 2.1's client doesn't predict the block. No event: 2.1's (0x100792e0) has no
    // block branch and would play closecombat.wav and trace. The idle waits 3.0's 3 s so the block
    // isn't cut short; 2.1's kung-fu idle (`FUN_1004dc40`) times nothing else (visual
    // compatibility deviation, docs/implementation/ts21.md).
    host.weapon_anim(gun.seq(1), false, 0);
    gun.idle = 3.0;
}

// ---- the stock hit -------------------------------------------------------------------------

/// The stock hit's `ColdCockAttack` (pushes at 0x1004ae80..0x1004aeb9: flags 0, 0, 0, 0, move
/// 0x17, damage, hull 3; reach 38 at 0x1004aee2). `ColdCockAttack` plays the move first, hit or
/// miss (`if (anim != -1) Animate`); 0x17 is the same straight punch in 3.0's ids.
fn stock_hit(weapon: u8, damage: f32) -> MeleeHit {
    hit(MeleeKind::ColdCock, weapon, 38.0, 3, damage, 0x0400_1080, 0x17)
}

/// The stock hit's sound (0x1004af1c): `ColdCockAttack` returns NULL on a miss and also on a
/// kung-fu block (unlike 3.0's, which returns the target), and NULL plays closecombat.wav.
fn stock_hit_sound(r: &MeleeResult) -> Sound {
    if matches!(r, MeleeResult::Hit { .. }) { Sound::KevlarHit } else { Sound::CloseCombatMiss }
}

/// `CTSGun::AlternateAttack` (0x1004ab40) with `ColdCockAttack` (`FUN_100572a0`), on the guns
/// whose 2.1 table entry has the stock flag (`info+0x10`, the M4A1 and the Sawed-off; the table
/// gives them `stock` = 20, [`tables::STOCK_DAMAGE`](super::tables::STOCK_DAMAGE)): 20 damage 38
/// units ahead, no stunting penalty, no disarm or knockback, the straight punch 0x17 on the
/// attacker ([`stock_hit`]); kevlarhit.wav on a hit, closecombat.wav on a miss or a block
/// ([`stock_hit_sound`]).
pub fn cold_cock(gun: &mut Gun, host: &mut dyn GunHost) {
    if !gun.has_slot() || gun.info().stock == 0 {
        return;
    }
    if gun.kind() == kind::GRENADE {
        gun.grenade_idle(true, host);
    }
    gun.stop_reload();
    let d = fixed_delay(0.725, false, slow_pause(host), t_of(host));
    gun.next_primary = d;
    gun.next_secondary = d;
    gun.next_alt = d;
    gun.idle = 2.0;
    let r = host.melee(&stock_hit(gun.current, gun.info().stock as f32), gun);
    host.sound(stock_hit_sound(&r));
    gun.unzoom(host);
    if gun.has_slot() {
        let items = gun.status[gun.current as usize].items as i32;
        host.event(Event::ColdCock, EventArgs { i1: gun.current as i32, i2: items, ..Default::default() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays() {
        assert_eq!(time_scale_inverse(1.0), 1.0);
        assert_eq!(time_scale_inverse(0.5), 2.0);
        assert_eq!(time_scale_inverse(0.01), 10.0, "floored at 0.1");
        // Combat knife (rate 0.4): 0.4 × 0.7 = 0.28 s.
        assert!(((fixed_delay(0.4, false, false, 1.0) as f64 * 0.7) as f32 - 0.28).abs() < 1e-6);
        assert_eq!(fixed_delay(0.4, true, false, 1.0), 0.2, "double fire rate");
        assert!((fixed_delay(0.5, false, true, 1.0) - 3.333_333_3).abs() < 1e-5, "slow pause");
        assert_eq!(whiff_window(1.0), 0.3);
        assert_eq!(whiff_window(20.0), 3.0);
        assert_eq!(block_window(0.6, 1.0), 0.3);
        assert_eq!(block_window(0.5, 100.0), 2.5, "capped at hold × 5");
    }

    /// 2.1's speed and back test per build (issue #171). mp.dll's katana tests the unrounded
    /// root (100.00000000005 is fast; its kung fu and the client store 100.0). The back test: the
    /// first input is backward only on the client (stored components and dot), the second only on
    /// mp.dll (the float `1/len`, an unrounded dot); the port's old `float` arithmetic had neither.
    #[test]
    fn motion_per_build() {
        let f = |b: [u32; 3]| b.map(f32::from_bits);
        let fast = |katana, client| motion_of([100.0, 0.0001, 0.0], [1.0, 0.0, 0.0], katana, client).fast;
        assert!(fast(true, false));
        assert!(!fast(false, false));
        assert!(!fast(true, true));
        let back = |v, w, katana, client| motion_of(f(v), f(w), katana, client).back;
        let (v, w) = ([0xc38b_5c0a, 0xc129_4f6c, 0x3eb2_08b1], [0x3eee_fe5f, 0x3f62_6625, 0]);
        assert_eq!([back(v, w, true, false), back(v, w, false, false), back(v, w, true, true)], [false, false, true]);
        let (v, w) = ([0xc1dc_49f3, 0xc2f5_ab4b, 0xbda6_6318], [0xbf3c_5590, 0x3f2d_6569, 0]);
        assert_eq!([back(v, w, true, false), back(v, w, false, false), back(v, w, true, true)], [true, true, false]);
    }

    #[test]
    fn katana() {
        assert_eq!([35, 70, 90, -100].map(katana_damage), [29, 59, 76, -85]);
        let mut last = 0;
        let s = katana_swing(false, false, true, false, &mut last, &mut || 1);
        assert_eq!((s.base, s.reach, s.hold, s.anim, last), (70, 48.0, 0.5, 33, 2));
        let s = katana_swing(false, false, true, false, &mut last, &mut || 1);
        assert_eq!((s.anim, s.view, last), (34, 0x10, 1), "the ground swings alternate");
        let s = katana_swing(true, true, true, false, &mut last, &mut || 1);
        assert_eq!((s.base, s.reach, s.hold, s.bits), (-100, 54.0, 1.0, 1));
        let s = katana_swing(true, false, true, false, &mut last, &mut || 1);
        assert_eq!(s.base, 35, "moving");
        let s = katana_swing(false, false, true, true, &mut last, &mut || 1);
        assert_eq!((s.base, s.hold, s.bits, s.crouch), (90, 1.0, 5, true));
        let s = katana_swing(false, false, false, true, &mut last, &mut || 1);
        assert_eq!((s.base, s.anim), (70, 36), "airborne before crouched");
    }

    #[test]
    fn kungfu_damage_scales_with_the_bar() {
        assert_eq!(kungfu_damage(10.0, 100.0), (9.0, 9.0));
        assert_eq!(kungfu_damage(30.0, 0.0).0, 0.0, "an empty bar deals nothing");
        let (d, c) = kungfu_damage(20.0, 50.0);
        assert!((d - 9.0).abs() < 1e-5 && c == 18.0);
    }

    fn input(buttons: u32, on_ground: bool, speed: f32, dot: f32) -> KungFuInput {
        KungFuInput { buttons, pressed: buttons, iuser4: 0, on_ground, speed, dot, back: speed > 100.0 && dot <= -0.5 }
    }

    #[test]
    fn punches() {
        let (mut pc, mut kc) = (0, 0);
        let i = input(IN_ATTACK, true, 0.0, 0.0);
        for n in 1..=5 {
            let m = kungfu_move(&i, &mut pc, &mut kc, 0, &mut |_, _| 0);
            assert_eq!((m.move_id, m.damage, m.delay, m.reach, pc), (0x16, 10.0, 0.15, 38.0, n));
        }
        let m = kungfu_move(&input(IN_ATTACK, true, 300.0, 0.0), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.move_id, m.damage, m.delay, pc), (0x17, 20.0, 0.2, 0), "the sixth is the heavy punch");
        let m = kungfu_move(&input(IN_ATTACK, false, 250.0, 0.0), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.move_id, m.damage, m.delay), (0x18, 25.0, 0.3));
    }

    /// The stunting kick multiplies by the `float` 0.1 (0x1004e188, issue #150): at 101.5 u/s that
    /// is 10.150001, where the `double` gives 10.15.
    #[test]
    fn stunting_kick_uses_the_float_tenth() {
        let (mut pc, mut kc) = (0, 0);
        let i = KungFuInput { iuser4: 0x400, ..input(IN_ATTACK2, false, 101.5, 0.0) };
        let m = kungfu_move(&i, &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.atype, m.delay), (2, 1.0));
        assert_eq!(m.damage.to_bits(), 0x4122_6667);
        // The punches keep the `double`: 101.5 · 0.1 = 10.15 (jumping punch).
        let m = kungfu_move(&input(IN_ATTACK, false, 101.5, 0.0), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!(m.damage.to_bits(), 0x4122_6666);
    }

    #[test]
    fn kicks() {
        let (mut pc, mut kc) = (0, 0);
        let m = kungfu_move(&input(IN_ATTACK2 | IN_DUCK, true, 0.0, 0.0), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.move_id, m.damage, m.delay, m.bits), (0x14, 30.0, 1.0, 5), "sweep");
        let m = kungfu_move(&input(IN_ATTACK2, true, 300.0, -0.9), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.atype, m.move_id, m.reach, m.delay), (3, 0x11, 56.0, 0.9));
        assert!((m.damage - 90.0).abs() < 1e-4, "{}", m.damage);
        let m = kungfu_move(&input(IN_ATTACK2 | IN_DUCK, false, 300.0, 0.9), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.move_id, m.bits), (0x10, 4), "flying kick");
        assert!((m.damage - 45.0).abs() < 1e-4);
        // A fresh press on the ground: the heavy kick cycle.
        let m = kungfu_move(&input(IN_ATTACK2, true, 0.0, 0.0), &mut pc, &mut kc, 0xf, &mut |_, _| 0);
        assert_eq!((m.move_id, m.damage, m.delay), (10, 10.0, 0.5));
        let m = kungfu_move(&input(IN_ATTACK2, false, 250.0, 0.0), &mut pc, &mut kc, 0x13, &mut |_, _| 0);
        assert_eq!((m.move_id, m.damage, m.delay), (0x12, 25.0, 0.7), "airborne cycle");
        let m = kungfu_move(&input(0, true, 0.0, 0.0), &mut pc, &mut kc, 0, &mut |_, _| 0);
        assert_eq!((m.atype, m.delay), (0, 0.6), "nothing pressed");
    }

    #[test]
    fn stock_hit_guns() {
        use super::super::tables;
        let stock = |id| tables::tables().weapon(id).map_or(0, |w| w.stock);
        assert_eq!([stock(5), stock(33)], [20, 20], "the M4A1 and the Sawed-off");
        assert_eq!([stock(12), stock(18), stock(13)], [0, 0, 0]);
    }

    /// #149: the stock hit plays 2.1's punch 0x17, and a blocked one sounds like a miss.
    #[test]
    fn stock_hit_anim_and_sound() {
        let h = stock_hit(5, 20.0);
        assert_eq!((h.reach, h.hull, h.damage, h.bits, h.player_anim), (38.0, 3, 20.0, 0x0400_1080, 0x17));
        assert!(!h.disarm && !h.knockback);
        assert!(matches!(stock_hit_sound(&MeleeResult::Hit { player: true }), Sound::KevlarHit));
        assert!(matches!(stock_hit_sound(&MeleeResult::Miss), Sound::CloseCombatMiss));
        assert!(matches!(stock_hit_sound(&MeleeResult::Blocked { next_attack: 0.0, bar_cost: 0.0 }), Sound::CloseCombatMiss));
    }

    #[test]
    fn view_anims() {
        let m = |atype, move_id| KungFuMove { atype, move_id, damage: 0.0, reach: 0.0, delay: 0.0, bits: 0 };
        assert_eq!([view_anim(&m(1, 0x16)), view_anim(&m(1, 0)), view_anim(&m(2, 0)), view_anim(&m(3, 0x11))], [3, 5, 10, 7]);
    }
}
