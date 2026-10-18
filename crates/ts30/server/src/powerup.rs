//! TS 3.0's power-ups and slow motion: the power-up values, a map power-up's type, the grenade
//! fuse and the sounds' pitch in slow motion.

use shared::pm::{Arith, Fpu, Num};
use shared::ts::powerup as pw;
use ts_server::rules::powerup::PowerupRules;
use ts_server::rules::state;

/// TS 3.0's power-up values.
pub const POWERUPS: PowerupRules = PowerupRules {
    // slow motion 3, infinite clip 3, slow pause 2, double firerate 3
    base_secs: [3, 3, 0, 2, 3, 0, 0, 0, 0],
    multiplier: true,
    instant: 0x1e6,
    infinite_clip_instant: true,
    slowmo_top_up_below: 3,
    slowmo_swap_above: 2,
    swap_on_instant: false,
    slowmo_drop_above: 2,
    slowmo: (0.6, 0.35, false),
    slowpause: (0.8, 0.225, true),
    hud_slowmo: 0.35,
    hud_slowpause: 0.225,
    hud_normal_as_zero: false,
    grenade_selects: true,
};

/// `SetupPowerUp`'s type (0x98014) for the type key `key` (`pwuptype`) and `spawnflags`, and
/// whether it is drawn again on each respawn: a key below 1 draws from the spawnflags' types
/// (all when none), never armour; armour, and the super jump on a map that gives it to everyone,
/// are drawn from all.
pub fn kind_for(key: i32, spawnflags: i32) -> (u16, bool) {
    let flags = spawnflags as u16;
    let superjump_map = state::with(|g| g.superjump_map);
    if key < 1 {
        let mut k = state::with(|g| g.random_powerup(flags & 0x1ff));
        if k == 0 {
            k = state::with(|g| g.random_powerup(0x1ff));
        }
        (k, true)
    } else {
        let k = key as u16;
        let k = if (superjump_map && k & pw::SUPERJUMP != 0) || k & pw::ARMOR != 0 { state::with(|g| g.random_powerup(0x1ff)) } else { k };
        (k, false)
    }
}

/// One fuse think: `elapsed = GiveSlowMul() * 0.1f + elapsed`, rounded once (`ts_i386.so`
/// 0x7cbbb..0x7cbca, mp.dll 0x10051096..0x100510a5; issue #150).
pub fn fuse_step(arith: Arith, elapsed: f32, slow: f32) -> f32 {
    fn go<F: Fpu>(elapsed: f32, slow: f32) -> f32 {
        (F::x(slow) * F::x(0.1) + F::x(elapsed)).f()
    }
    shared::by_arith!(arith, go(elapsed, slow))
}

/// `EMIT_SOUND_DYN`'s pitch (0xe0aa7): the factor `(1 - s) + s * fuser1` (`s` alone when
/// `fuser1` is 0) is stored as a float, then multiplied by the pitch on the x87 stack and
/// truncated (0xe0b1e). `f64` holds both steps exactly enough to match.
pub fn slow_pitch(pitch: i32, time_scale: f32, s: f32) -> i32 {
    let k = if time_scale != 0.0 { s as f64 * time_scale as f64 } else { s as f64 };
    let factor = ((1.0 - s as f64) + k) as f32;
    (factor as f64 * pitch as f64) as i32
}

/// One try of `GetRandomPosition` (`ts_i386.so` 0x974f4 / objdump 0x874f4, `mp.dll`
/// FUN_10065f40), issue #228: `Vector(cos a, sin a, 0).Normalize()` with `a` the `double`
/// `angle * 0.017453292519943295` (`fcos`/`fsin`, each stored as a `float`), then the trace end
/// `from + dir * dist`.
///
/// * GCC stores the length (`fsqrt; fstp dword` 0x875de) and keeps the x product `dist * dir.x`
///   on the stack for its sum (`fst` 0x87669, `faddp` 0x87680); y and z are stored first.
/// * MSVC keeps the length (`Length` 0x1000e920) and `1 / len` unrounded and stores all three
///   products (0x10066046..0x10066063) before adding `from`.
pub fn random_position_ray<F: Fpu>(angle: i32, dist: i32, from: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let r = F::xi(angle) * F::xd(DEG_TO_RAD);
    let v = [r.cos().f(), r.sin().f(), 0.0];
    let [x, y, z] = v.map(F::x);
    let len = (x * x + y * y + z * z).sqrt();
    let len = if F::MSVC { len } else { F::x(len.f()) };
    let dir = if F::cmp_eq(len, F::ZERO) {
        [0.0, 0.0, 1.0]
    } else {
        let inv = F::x(1.0) / len;
        v.map(|c| (F::x(c) * inv).f())
    };
    let d = F::xi(dist);
    let px = d * F::x(dir[0]);
    let px = if F::MSVC { F::x(px.f()) } else { px };
    let stored = |i: usize| F::x((d * F::x(dir[i])).f());
    let end = [(F::x(from[0]) + px).f(), (F::x(from[1]) + stored(1)).f(), (F::x(from[2]) + stored(2)).f()];
    (dir, end)
}

/// `GetRandomPosition`'s degrees-to-radians `double` (`ts_i386.so` [ebx-0x6c6c0], `mp.dll`
/// 0x10089130).
const DEG_TO_RAD: f64 = 0.017453292519943295;

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(v: [f32; 3]) -> [u32; 3] {
        v.map(f32::to_bits)
    }

    /// Issue #228: `GetRandomPosition`'s direction and trace end in each build (values from an
    /// x87 model of the machine code: GCC's extended precision, MSVC's 53 bits).
    #[test]
    fn random_position_rays() {
        use shared::pm::{Gcc, Msvc};
        let from = [-1234.567, 345.678, 36.03125];
        // 22°, 150 units: the same direction; MSVC rounds `dist * dir.x` before adding `from`.
        let (dm, em) = random_position_ray::<Msvc>(22, 150, from);
        let (dg, eg) = random_position_ray::<Gcc>(22, 150, from);
        assert_eq!(bits(dm), [0x3f6d5bec, 0x3ebfcc6f, 0]);
        assert_eq!(dg, dm);
        assert_eq!(bits(em), [0xc488efaa, 0x43c8ef3b, 0x42102000]);
        assert_eq!(bits(eg), [0xc488efa9, 0x43c8ef3b, 0x42102000]);
        // 198°, 128 units.
        let (d, e) = random_position_ray::<Msvc>(198, 128, from);
        assert_eq!(bits(d), [0xbf737871, 0xbe9e377a, 0]);
        assert_eq!(bits(e), [0xc4a989ac, 0x43990fda, 0x42102000]);
        assert_eq!(random_position_ray::<Gcc>(198, 128, from), (d, e));
        // Straight along x.
        assert_eq!(random_position_ray::<Msvc>(0, 1024, [0.0; 3]), ([1.0, 0.0, 0.0], [1024.0, 0.0, 0.0]));
    }
    use ts_server::entities::ts::powerup::pickup_plan;
    use ts_server::player::damage::{HIT_SLOW, PAIN_SLOW};

    /// TS 3.0's values, as the code had them before the hook (`SetupPowerUp` 0x98014,
    /// `UpdatePowerUpState` 0x95748).
    #[test]
    fn core_values() {
        let r = POWERUPS;
        assert_eq!(
            [pw::SLOWMO, pw::INFINITE_CLIP, pw::KUNGFU, pw::SLOWPAUSE, pw::DOUBLE_FIRERATE, pw::GRENADE, pw::ARMOR].map(|k| r.base(k)),
            [3, 3, 0, 2, 3, 0, 0]
        );
        assert_eq!(r.base(pw::SLOWPAUSE | pw::SLOWMO), 3, "the lowest bit counts");
        assert_eq!(r.base(0x200), 0);
        assert_eq!((r.slowmo, r.slowpause), ((0.6, 0.35, false), (0.8, 0.225, true)));
        assert_eq!((r.hud_slowmo, r.hud_slowpause), (0.35, 0.225));
        assert_eq!((r.instant, r.slowmo_top_up_below, r.slowmo_swap_above, r.slowmo_drop_above), (0x1e6, 3, 2, 2));
        assert!(r.multiplier && r.infinite_clip_instant && !r.swap_on_instant);
        assert!(r.grenade_selects, "PickUpWeapon(0x18, NULL, 0, 1, 0)");
    }

    /// `WaitTouch` (0x98600..) with TS 3.0's values.
    #[test]
    fn pickup_rules() {
        let r = &POWERUPS;
        let plan = |kind, held, secs, active, using| pickup_plan(r, kind, held, secs, active, using);
        // An empty slot takes anything; a held one refuses a second stored type.
        assert_eq!(plan(pw::SLOWPAUSE, 0, 0, 0, false), Some(0));
        assert_eq!(plan(pw::SLOWPAUSE, pw::SLOWMO, 2, 0, false), None);
        // Slow motion tops up a held one below 3 s.
        assert_eq!(plan(pw::SLOWMO, pw::SLOWMO, 2, 0, false), Some(0));
        assert_eq!(plan(pw::SLOWMO, pw::SLOWMO, 3, 0, false), None);
        // +use swaps, slow motion only above 2 s; never while one runs.
        assert_eq!(plan(pw::SLOWPAUSE, pw::DOUBLE_FIRERATE, 3, 0, true), Some(pw::DOUBLE_FIRERATE));
        assert_eq!(plan(pw::SLOWPAUSE, pw::SLOWMO, 2, 0, true), Some(0));
        assert_eq!(plan(pw::SLOWPAUSE, pw::SLOWMO, 3, 0, true), Some(pw::SLOWMO));
        assert_eq!(plan(pw::SLOWPAUSE, 0, 0, pw::SLOWMO, true), None);
        // Instant types: always, and +use doesn't drop the held one. Infinite clip is one.
        for k in [pw::INFINITE_CLIP, pw::KUNGFU, pw::GRENADE, pw::HEALTH, pw::ARMOR, pw::SUPERJUMP] {
            assert_eq!(plan(k, pw::SLOWMO, 3, pw::SLOWMO, true), Some(0), "{k:#x}");
        }
    }

    /// The fuse step rounds once (issue #150); `elapsed + 0.1 * slow` in `float`s gave 0x3fa64a5a.
    #[test]
    fn fuse_step_rounds_once() {
        let (e, s) = (f32::from_bits(0x3f9d_1368), f32::from_bits(0x3f38_4af2));
        for arith in [Arith::Gcc, Arith::Msvc] {
            assert_eq!(fuse_step(arith, e, s).to_bits(), 0x3fa6_4a5b);
        }
    }

    #[test]
    fn slow_pitch_truncates() {
        assert_eq!(slow_pitch(100, 0.0, 0.5), 100, "no time scale");
        assert_eq!(slow_pitch(100, 1.0, 0.5), 100);
        assert_eq!(slow_pitch(100, 0.5, 0.5), 75);
        assert_eq!(slow_pitch(101, 0.5, 0.5), 75, "75.75 truncates");
        assert_eq!(slow_pitch(126, 0.25, 0.5), 78, "78.75 truncates");
    }

    /// The weapon and item sounds' pitch in slow motion (issue #32): `(1 - 0.5) + 0.5 * fuser1`
    /// of TS's pitch (the weapons' slow factor 0.5), truncated; unchanged at normal speed or with
    /// no time scale.
    #[test]
    fn weapon_sounds_drop_pitch_with_the_time_scale() {
        const SLOW: f32 = 0.5;
        assert_eq!(slow_pitch(100, 1.0, SLOW), 100, "normal speed");
        assert_eq!(slow_pitch(100, 0.0, SLOW), 100, "no time scale");
        assert_eq!(slow_pitch(100, 0.5, SLOW), 75, "draw.wav, items, drops at half speed");
        assert_eq!(slow_pitch(101, 0.5, SLOW), 75, "switch.wav's top pitch: 75.75 truncates");
        assert_eq!(slow_pitch(110, 0.2, SLOW), 66, "weapondrop1.wav's top pitch at 0.2");
        assert_eq!(slow_pitch(90, 0.5, SLOW), 67, "kevlarhit.wav's top pitch: 67.5 truncates");
    }

    /// `TraceAttack`'s sounds in slow motion (issue #33): the hit sound drops by
    /// `0.5 + 0.5 * fuser1`, the pain sound only by `0.85 + 0.15 * fuser1`.
    #[test]
    fn hit_and_pain_sounds_drop_pitch_with_the_time_scale() {
        assert_eq!((HIT_SLOW, PAIN_SLOW), (0.5, 0.15));
        assert_eq!(slow_pitch(100, 1.0, PAIN_SLOW), 100, "normal speed");
        assert_eq!(slow_pitch(100, 0.0, PAIN_SLOW), 100, "no time scale");
        assert_eq!(slow_pitch(100, 0.5, PAIN_SLOW), 92, "pain at half speed: 92.5 truncates");
        // 0.85 + 0.15 * 0.2 rounds to the float just under 0.88, so TS's `fistp` gives 87.
        assert_eq!(slow_pitch(100, 0.2, PAIN_SLOW), 87, "pain at 0.2");
        assert_eq!(slow_pitch(100, 0.5, HIT_SLOW), 75, "hit at half speed");
        assert_eq!(slow_pitch(126, 0.5, HIT_SLOW), 94, "the hit's top pitch (95 + 31): 94.5 truncates");
    }
}
