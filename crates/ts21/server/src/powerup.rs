//! TS 2.1's power-ups and slow motion (issue #100 §3.7; decomp `docs/ts21/rules.md` §7-8).
//! Addresses are 2.1 `mp.dll` VAs.

use goldsrc::server::engine;
use ts_server::rules::powerup::PowerupRules;
use ts_server::rules::{cvars, frame, state};
use ts_server::world::Cx;

/// 2.1's power-up values.
pub const POWERUPS: PowerupRules = PowerupRules {
    // `CTSPowerUp::SetupPowerUp` (0x10059ce0): slow motion 5, infinite clip 3, slow pause 3,
    // double firerate 5. `KeyValue` (0x10059c10) has no `pwupmult`.
    base_secs: [5, 3, 0, 3, 5, 0, 0, 0, 0],
    multiplier: false,
    // `WaitTouch` (0x1005a160): `kind & 0x1e4`, so infinite clip is a stored type.
    instant: 0x1e4,
    // `PlayerAddPowerUp` (0x100599d0) has no infinite-clip case: it is held, used like slow
    // motion, and the gun's fire paths skip `clip--` while it runs (`FUN_10042c20`).
    infinite_clip_instant: false,
    // `WaitTouch`: `held == 1 && kind == 1 && heldsecs < 5`; +use swaps a held slow motion
    // only with `heldsecs > 4`, and swaps whatever the pickup is (no instant-type exclusion:
    // touching an instant power-up with +use drops a copy of the held one, which is kept).
    slowmo_top_up_below: 5,
    slowmo_swap_above: 4,
    swap_on_instant: true,
    // `UpdatePowerUpState` (0x100568e0): `(kind & 1) == 0 || 4 < heldsecs`.
    slowmo_drop_above: 4,
    // `UpdatePowerUpState`: slow motion 0.55 / 0.35 (`pl+0x164/0x168`), slow pause 0.6 / 0.15
    // with the bullet-time flag (`pl+0x15d`).
    slowmo: (0.55, 0.35, false),
    slowpause: (0.6, 0.15, true),
    // `UpdateClientData` (0x100547c0, 0x10054d1e): `TSSlowMo` 0.15 under slow pause, 0.35 under
    // slow motion (`0x100890cc` / `0x100890c8`).
    hud_slowmo: 0.35,
    hud_slowpause: 0.15,
    // `TSInit` (0x100552f0) starts `pl+0x158` at 0 and the last sent at -1, so a 2.1 server sends
    // 0 for normal speed; the client's icon (0x10086651) shows for anything else. The port keeps
    // 3.0's 1 for normal, so 2.1 mode sends that as 0 (issue #324).
    hud_normal_as_zero: true,
    // `PlayerAddPowerUp` (0x100599d0): `PickUpWeapon(0x18, NULL, 0, '\0')`, so the grenades are
    // drawn only from kung fu (`PickUpWeapon` 0x10055830: `select || current == 0`).
    grenade_selects: false,
};

/// `SetupPowerUp`'s type (0x10059ce0): a key below 1 is `1 << RandomLong(0, 8)`, any of the nine
/// types, armour included, whatever the spawnflags; any other key is kept as it is. (The
/// `superjump` cvar `SetupPowerUp` also reads, 0x10093d2c, is never registered, so it is always
/// 0 and its branches, `RandomLong(0, 7)`, are dead.)
pub fn kind(key: i32, spawnflags: i32) -> (u16, bool) {
    let _ = spawnflags;
    if key < 1 { (random_kind(engine().random_long(0, 8)), true) } else { (key as u16, false) }
}

/// `1 << index` (`index & 0x1f`, as the x86 shift masks it).
fn random_kind(index: i32) -> u16 {
    (1u32 << (index & 0x1f)) as u16
}

/// `PlayerAddPowerUp` (0x100599d0), armour: `armorvalue += pu->m_flArmor` (`+0x7c`, 100 from
/// `SetupPowerUp`), at most 100. No slot cost and no sound but the pickup's.
pub fn armor(cx: &mut Cx, amount: f32) {
    let v = cx.pev();
    v.armorvalue = add_armor(v.armorvalue, amount);
}

fn add_armor(armor: f32, amount: f32) -> f32 {
    let a = amount + armor;
    if 100.0 <= a { 100.0 } else { a }
}

/// `CTSGameRules::Think`'s automatic power-ups (0x10043cc0, the block at 0x10043f90): the first
/// Think looks for a `ts_powerup` and, finding one, turns the spawner off for the map. Then, with
/// `nopowerup` not 1 (an ordered compare: a NaN doesn't stop it), every 20 s (1 s after no clear
/// spot): slow motion when `RandomFloat(0, 1) < 0.25` and `slowmatch` isn't 1, else
/// `1 << RandomLong(0, 8)` (armour possible), through `PutPowerUp` (0x10059840).
pub fn auto_powerups(now: f32) {
    let eng = engine();
    if state::with(|g| g.powerup_spawner == 0xff) {
        let any = !eng.find_by_string(std::ptr::null_mut(), c"classname", c"ts_powerup").is_null();
        state::with(|g| g.powerup_spawner = if any { 0 } else { 1 });
    }
    if cvars::f(c"nopowerup") == 1.0 || state::with(|g| g.powerup_spawner != 1) {
        return;
    }
    let next = state::with(|g| g.next_powerup);
    if now <= next {
        if 21.0 < next - now {
            state::with(|g| g.next_powerup = 0.0);
        }
        return;
    }
    let r = eng.random_float(0.0, 1.0);
    let index = if spawns_slowmo(r, cvars::f(c"slowmatch")) { 0 } else { eng.random_long(0, 8) };
    let ok = frame::put_powerup(random_kind(index));
    state::with(|g| g.next_powerup = now + if ok { 20.0 } else { 1.0 });
}

/// The automatic spawner's slow-motion draw: `r < 0.25` (a `double` compare) and `slowmatch`
/// not 1 (`fcomp; test ah,0x44; jnp`: a NaN isn't 1).
fn spawns_slowmo(r: f32, slowmatch: f32) -> bool {
    f64::from(r) < 0.25 && slowmatch != 1.0
}

/// The end of 2.1's `GoSlow` (0x10055220), issue #112: back to normal speed (2.1's amount 0, the
/// port's 1) clamps `m_flNextAttack` (`pl+0x118`) to 1 s (`1.0 <= t ? 1.0 : t`, a NaN stays), and
/// with the TS gun active its two next-attack times (`gun+0x90`, `gun+0x8c`) take that value. 2.1's
/// weapon timers count down in real time, so a long delay set in slow motion ends within a second
/// of it.
pub fn go_slow_release(next_attack: f32) -> f32 {
    if 1.0 <= next_attack { 1.0 } else { next_attack }
}

/// [`go_slow_release`] on the player ([`ServerRules::on_go_slow`](ts_server::gameplay::ServerRules::on_go_slow)).
pub fn on_go_slow(p: &mut ts_server::player::Player, amount: f32) {
    if amount != 1.0 {
        return;
    }
    let t = go_slow_release(p.inv.gun.next_attack);
    p.inv.gun.next_attack = t;
    if ts_server::weapons::gun_active(p) {
        p.inv.gun.next_secondary = t;
        p.inv.gun.next_primary = t;
    }
}

/// One try of 2.1's `GetRandomPosition` (FUN_10059510, from `PutPowerUp` FUN_10059840), issue
/// #228: `Vector(cos a, sin a, 0).Normalize()` with `a` the `double` `angle * 0.017453292519943295`
/// (0x10080190). Unlike 3.0's `mp.dll`, only the cosine is stored as a `float`
/// (0x1005956f): the sine stays on the x87 for the length (`z² + s² + c²`, 0x10059575..0x1005958d)
/// and for `dir.y` (0x100595bc). The trace end keeps the x product `dist * dir.x` unrounded for
/// its sum (0x10059613); y and z are stored first.
pub fn random_position_ray(angle: i32, dist: i32, from: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let r = f64::from(angle) * 0.017453292519943295;
    let c = f64::from(r.cos() as f32);
    let s = r.sin();
    let z = 0.0f64;
    let len = (z * z + s * s + c * c).sqrt();
    let dir = if len == 0.0 {
        [0.0, 0.0, 1.0]
    } else {
        let inv = 1.0 / len;
        [(inv * c) as f32, (inv * s) as f32, (inv * z) as f32]
    };
    let d = f64::from(dist);
    let stored = |i: usize| f64::from((d * f64::from(dir[i])) as f32);
    let end = [
        (f64::from(from[0]) + d * f64::from(dir[0])) as f32,
        (f64::from(from[1]) + stored(1)) as f32,
        (f64::from(from[2]) + stored(2)) as f32,
    ];
    (dir, end)
}

/// `kind` is one of the types the spawner and `SetupPowerUp` draw.
#[cfg(test)]
fn is_type(kind: u16) -> bool {
    use shared::ts::powerup as pw;
    kind.count_ones() == 1 && kind <= pw::SUPERJUMP
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #228: 2.1's `GetRandomPosition` (FUN_10059510) keeps the sine unrounded, so its
    /// direction can differ from 3.0's `mp.dll` by an ulp (198°: 3.0's y is 0xbe9e377a), and it
    /// adds the unrounded x product (22°, 150 units: 3.0's `mp.dll` gives 0xc488efaa).
    #[test]
    fn random_position_rays() {
        let bits = |v: [f32; 3]| v.map(f32::to_bits);
        let from = [-1234.567, 345.678, 36.03125];
        let (d, e) = random_position_ray(198, 128, from);
        assert_eq!(bits(d), [0xbf737871, 0xbe9e3779, 0]);
        assert_eq!(bits(e), [0xc4a989ac, 0x43990fda, 0x42102000]);
        let (d, e) = random_position_ray(22, 150, from);
        assert_eq!(bits(d), [0x3f6d5bec, 0x3ebfcc6f, 0]);
        assert_eq!(bits(e), [0xc488efa9, 0x43c8ef3b, 0x42102000]);
    }
    use shared::ts::powerup as pw;
    use ts_server::entities::ts::powerup::pickup_plan;

    #[test]
    fn go_slow_release_clamps_to_a_second() {
        assert_eq!(go_slow_release(3.5), 1.0);
        assert_eq!(go_slow_release(1.0), 1.0);
        assert_eq!(go_slow_release(0.4), 0.4);
        assert_eq!(go_slow_release(-0.001), -0.001);
        assert!(go_slow_release(f32::NAN).is_nan(), "a NaN stays");
    }

    /// Issue #223: a player the flow slowed is released through `GoSlow` too (FUN_10057610 →
    /// FUN_10055220), so a victim holding a long slow-motion delay keeps at most 1 s of it.
    #[test]
    fn released_victim_keeps_at_most_a_second() {
        let mut p = ts_server::player::Player::default();
        p.inv.sv.has_gun = true;
        (p.inv.gun.next_attack, p.inv.gun.next_primary, p.inv.gun.next_secondary) = (2.5, 4.0, 4.0);
        on_go_slow(&mut p, 0.35);
        assert_eq!(p.inv.gun.next_attack, 2.5, "slowing down leaves the delays");
        on_go_slow(&mut p, 1.0);
        assert_eq!((p.inv.gun.next_attack, p.inv.gun.next_primary, p.inv.gun.next_secondary), (1.0, 1.0, 1.0));
    }

    #[test]
    fn durations() {
        let r = POWERUPS;
        assert_eq!(r.base(pw::SLOWMO), 5);
        assert_eq!(r.base(pw::SLOWPAUSE), 3);
        assert_eq!(r.base(pw::DOUBLE_FIRERATE), 5);
        assert_eq!(r.base(pw::INFINITE_CLIP), 3);
        for k in [pw::KUNGFU, pw::GRENADE, pw::HEALTH, pw::ARMOR, pw::SUPERJUMP] {
            assert_eq!(r.base(k), 0, "{k:#x}");
        }
        assert!(!r.multiplier, "no pwupmult");
        assert!(!r.grenade_selects, "the grenade power-up keeps the held weapon");
        let core: &dyn ts_server::gameplay::ServerRules = &ts30_server::Ts30Server;
        assert!(core.powerups().grenade_selects, "3.0 draws the grenades");
    }

    #[test]
    fn slow_amounts() {
        let r = POWERUPS;
        assert_eq!(r.slowmo, (0.55, 0.35, false));
        assert_eq!(r.slowpause, (0.6, 0.15, true));
        assert_eq!((r.hud_slowmo, r.hud_slowpause), (0.35, 0.15));
        let core: &dyn ts_server::gameplay::ServerRules = &ts30_server::Ts30Server;
        assert!(r.hud_normal_as_zero && !core.powerups().hud_normal_as_zero, "2.1 sends 0 at normal speed");
        // The 3.0 values are untouched.
        assert_eq!(ts30_server::powerup::POWERUPS.slowpause, (0.8, 0.225, true));
    }

    /// The draw covers all nine types, armour (index 7) included.
    #[test]
    fn random_types() {
        let kinds: Vec<u16> = (0..=8).map(random_kind).collect();
        assert_eq!(kinds, [1, 2, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x100]);
        assert!(kinds.iter().all(|&k| is_type(k)));
        assert!(kinds.contains(&pw::ARMOR));
    }

    #[test]
    fn spawner_slowmo_draw() {
        assert!(spawns_slowmo(0.2, 0.0));
        assert!(!spawns_slowmo(0.25, 0.0));
        assert!(!spawns_slowmo(0.2, 1.0), "slowmatch 1 draws from all");
        assert!(spawns_slowmo(0.2, f32::NAN), "a NaN slowmatch isn't 1");
    }

    /// `PlayerAddPowerUp`'s armour: added, at most 100.
    #[test]
    fn armor_adds_up_to_100() {
        assert_eq!(add_armor(0.0, 100.0), 100.0);
        assert_eq!(add_armor(30.0, 50.0), 80.0);
        assert_eq!(add_armor(60.0, 50.0), 100.0);
        assert_eq!(add_armor(-20.0, 50.0), 30.0);
    }

    /// `WaitTouch` (0x1005a160) with 2.1's values.
    #[test]
    fn pickup_rules() {
        let r = &POWERUPS;
        let plan = |kind, held, secs, active, using| pickup_plan(r, kind, held, secs, active, using);
        // Slow motion tops up a held one below 5 s.
        assert_eq!(plan(pw::SLOWMO, pw::SLOWMO, 4, 0, false), Some(0));
        assert_eq!(plan(pw::SLOWMO, pw::SLOWMO, 5, 0, false), None);
        // +use swaps a held slow motion above 4 s only.
        assert_eq!(plan(pw::SLOWPAUSE, pw::SLOWMO, 4, 0, true), Some(0));
        assert_eq!(plan(pw::SLOWPAUSE, pw::SLOWMO, 5, 0, true), Some(pw::SLOWMO));
        // Infinite clip is stored: refused while one is held, unless +use swaps.
        assert_eq!(plan(pw::INFINITE_CLIP, pw::SLOWPAUSE, 3, 0, false), None);
        assert_eq!(plan(pw::INFINITE_CLIP, pw::SLOWPAUSE, 3, 0, true), Some(pw::SLOWPAUSE));
        // The swap bug: +use on an instant power-up drops a copy of the held one.
        assert_eq!(plan(pw::HEALTH, pw::DOUBLE_FIRERATE, 5, 0, true), Some(pw::DOUBLE_FIRERATE));
        assert_eq!(plan(pw::ARMOR, pw::SLOWMO, 5, pw::SLOWMO, true), Some(pw::SLOWMO));
        assert_eq!(plan(pw::HEALTH, pw::DOUBLE_FIRERATE, 5, 0, false), Some(0));
        // ... and 3.0 doesn't.
        let core = &ts30_server::powerup::POWERUPS;
        assert_eq!(pickup_plan(core, pw::HEALTH, pw::DOUBLE_FIRERATE, 5, 0, true), Some(0));
    }
}
