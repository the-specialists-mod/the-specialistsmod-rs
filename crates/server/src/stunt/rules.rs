//! What a gameplay ruleset can change in the server's stunt code (`CTSStunt`): the values
//! ([`StuntRules`], `ServerRules::stunts`), how the stunt timers keep time ([`StuntClock`],
//! `ServerRules::stunt_clock`) and the slot limits (`ServerRules::can_dive`). Each ruleset's
//! plugin holds its stunt values, slot limits and clock implementation.

use super::StuntState;

/// A dive height: `CanDiveHigh`, `CanDiveMedium`, `CanDiveLow` (0x9a494, 0x9a4ec, 0x9a544).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dive {
    High,
    Medium,
    Low,
}

/// When `CTSAnimation::Animate`'s standing shot (ids 0/8/9, not ducking) plays the `2` variant
/// of `shoot_<ext>` (e.g. `shoot_onehanded2`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FirePose2 {
    /// TS 3.0 (`Animate` 0x6b470): while the hold timer (`+0x08`) is above 0.001, unzoomed
    /// (`fov == 0`), for a one- or two-handed extension, and the shot timer is zeroed. The hold
    /// timer is 3 s after a get-up, a dive end, a roll, a wheel, a wall pose or a melee move,
    /// counted down in the player's time (`UpdateTimers`).
    AfterHold,
    /// For a one-handed extension (`strncmp(ext, "one", 3)`) while the stored horizontal speed is
    /// below 180; the shot timer runs as for any shot. There is no hold timer.
    SlowOneHanded,
}

/// The stunt values a ruleset sets. Seconds are the player's own (slowed) time, given to the
/// [`StuntClock`] as `f64` so a ruleset whose binary used `double` constants gets them exactly;
/// TS 3.0's are `float`s, which these `f64`s convert back to unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StuntRules {
    /// `Landing` (0x9bbf4): the recovery after rolling out of a landing dive.
    pub dive_roll_out_recover: f64,
    /// `HandleWheel` (0x9b820): the recovery after landing a flip.
    pub flip_recover: f64,
    /// `HandleSlide` (0x9b9f8): the recovery after getting up from prone.
    pub get_up_recover: f64,
    /// `HandleWallJump` (0x9b4fc) / `HandleWallClimb` (0x9b644): the recovery after landing a
    /// wall jump or climb, and whether that landing also sets `RECOVER` (slow input).
    pub wall_landing_recover: f64,
    pub wall_landing_sets_recover: bool,
    /// `GoRoll` (0x9cfe8): the roll's recovery.
    pub roll_recover: f64,
    /// `PostThink`'s super-jump landing: the hard landing's recovery, and the weapon delay
    /// (`m_flNextAttack`) after rolling out of it.
    pub super_jump_landing_recover: f64,
    pub super_jump_roll_attack_delay: f64,
    /// The keys that roll out of a landing dive (`Landing`) or get up from prone
    /// (`HandleSlide`): `IN_JUMP | IN_DUCK | IN_ALT1`.
    pub get_up_buttons: i32,
    /// `HandleStunt` (0x9b000): the stunt key with a wall behind dives off it
    /// (`CheckForWallDive` 0x9ad44, `WALLDIVE`), and from a wall climb (`HandleWallClimb`).
    pub wall_dive: bool,
    /// `HandleStunt`: holding jump climbs a wall even while gliding after a super jump.
    pub wall_climb_while_gliding: bool,
    /// `HandleStunt` closes the gun's close-combat window (pl+0x79c..0x7a4).
    pub stunt_ends_close_combat: bool,
    /// `GoWheel` (0x9c9bc) and `GoRoll` wait for the jump kick's cooldown (`+0x08`).
    pub wheel_and_roll_wait_for_kick: bool,
    /// `CloseAttack` (0x9c630): when the jump kick's 0.7 s cooldown is armed and its move drawn
    /// (`RandomLong(0, 2)`). TS 3.0 does both only after a hit, so a held jump keeps trying
    /// every frame; `true` does both before the try, so a miss waits out the cooldown too.
    pub kick_cooldown_on_try: bool,
    /// `CTSAnimation::Animate` (0x6b470) case 0x37: the wall-jump pose refuses other animations
    /// while unfinished, alive and `GO_DUCK`. TS 3.0 holds it only in the air and lets a dive
    /// through, so a wall-jump landing switches to the ground poses at once; `true` holds it on
    /// the ground too (until the recovery's end clears `GO_DUCK`) and against a dive. `Play` ends
    /// a recovery early when the sequence finishes, so this decides when the landing's
    /// recovery (and its `RECOVER` input lock) can end.
    pub wall_jump_pose_holds_until_recovered: bool,
    /// `Animate` case 0x38: the wall-climb pose lets a dive (0x28) through.
    pub wall_climb_pose_lets_dive_through: bool,
    /// `Animate`: a playing `goduck` (5) refuses other animations while unfinished, alive, duck
    /// held and not yet ducking, but for a roll, dive, jump, the super jump and the two ducking
    /// melee moves. TS 3.0 has no such case.
    pub goduck_holds: bool,
    /// `Animate` case 0x2a/0x2b: the prone side roll holds while a strafe key is held
    /// (`IN_MOVELEFT | IN_MOVERIGHT`). TS 3.0 holds it while the velocity isn't square to the
    /// view's right (or is zero).
    pub side_roll_holds_on_strafe_keys: bool,
    /// `Animate`: while a melee move plays, the legs follow (`GaitCloseCombat`, TS 3.0) when its
    /// close-combat flag is clear on the ground; `false` refuses every non-stunt id until it ends.
    pub melee_gait_follows: bool,
    /// `Animate`'s leg tail: standing still, `m_szAnimExtention` (e.g. `knife_idle`) comes before
    /// the torso extension's idle.
    pub gait_idle_anim_extension: bool,
    /// `CTSAnimation`'s "recently shot" (`+0x0c`), grenade (`+0x10`) and bullet-dance timers keep
    /// the end time `time + d` through the [`StuntClock`] instead of counting down in
    /// `UpdateTimers` (the shot timer is tested `time <= t` for the aim poses, `t > time` for the
    /// legs' combat idle and the whole-body follow, the dance `time <= until && next <= time`).
    /// The `+0x08` hold timer still counts down.
    pub anim_end_times: bool,
    /// `HandleSlide` (0x9b9f8): the side roll's stunt direction is `v_forward` normalised
    /// (`x² + y² + z²`) rather than `v_forward` as it is.
    pub side_roll_normalizes_forward: bool,
    /// `Animate`'s fire case: when the standing shot plays its `2` pose, and with it whether the
    /// hold timer exists (it feeds nothing else).
    pub fire_pose_2: FirePose2,
    /// `Animate`'s idle refresh (ids 1/6): a set `m_szAnimExtention` (e.g. `knife_idle`) is the
    /// torso's idle, before the torso extension's poses.
    pub idle_anim_extension: bool,
    /// `Animate`'s idle refresh: zoomed in (`fov != 0`) takes the aim poses as a recent shot does.
    pub aim_pose_while_zoomed: bool,
    /// `CTSAnimation::GaitCloseCombat` (0x6dd14): a melee move's legs follow when ducking or when
    /// the unrounded horizontal speed is above this (a NaN isn't).
    pub melee_gait_speed: f32,
    /// `GoBulletTime` (0x9a5b4): bullets do ×0.7 for this many seconds (absolute, unscaled
    /// time, `+0x34`); `None` sets no dodge window.
    pub bullet_time_dodge: Option<f32>,
    /// `GoBulletTime`: a weapon delay (`m_flNextAttack`, only when it has run out) in the
    /// player's time; `None` leaves the gun alone.
    pub bullet_time_attack_delay: Option<f64>,
    /// `Play` (0x9a014): the `iuser4` bits cleared when the recovery runs out, and whether that
    /// also ends a `BlockPlayer` lock (`pl+0x4dc`).
    pub recover_end_clears: i32,
    pub recover_end_unblocks: bool,

    // Audit round 2: E
    /// `Animate`'s velocity directions (the in-air jump blend, the jump pose, the dive start and
    /// the death pose) under `mp.dll`'s arithmetic: `Vector::Normalize()` stores `1 / len` as a
    /// `float` before the products and tests the zero length IEEE (a NaN length normalises).
    /// TS 3.0's keeps the reciprocal unrounded and takes (0, 0, 1) for a NaN.
    pub anim_normalize_stores_reciprocal: bool,
    /// `CheckForBreakables`' dive under `mp.dll`'s arithmetic: the speed gate and the trace
    /// direction's zero test are IEEE (a NaN speed goes on, a NaN length normalises) and the
    /// direction sums `x² + y² + z²`. TS 3.0's sums `z² + x² + y²` and a NaN stops at the gate.
    pub breakables_ieee_xyz: bool,
}

/// How the stunt timers keep time: the cooldown (`next_stunt`), the recovery (`recover`), the
/// jump kick's cooldown (`close_attack_cd`), the prone lock (`prone_lock`) and `BlockPlayer`'s
/// lock (`block_timer`). The stunt code only stores what [`start`](Self::start) returns and asks
/// [`running`](Self::running) / [`done`](Self::done), so a ruleset can keep them in another form.
///
/// TS 3.0's plugin holds the seconds left of the player's own time (a relative countdown):
/// they count down by `slowmul * frametime` each frame (`CTSStunt::Play` 0x9a014), rest at
/// 0.001 (the recovery, `BlockPlayer`) or 0, and count as over below 0.01. TS 2.1's plugin
/// holds absolute end times.
pub trait StuntClock: Sync {
    /// The value to store for a timer of `secs` seconds of the player's time, started at `now`
    /// while the player's time scale (`pev->fuser1`, 1 = normal) is `time_scale`.
    fn start(&self, now: f32, time_scale: f32, secs: f64) -> f32;

    /// The timer `t` is still running (stunts wait for it).
    fn running(&self, now: f32, t: f32) -> bool;

    /// The timer `t` has run out: the recovery and the locks end, a wall landing may stunt again.
    fn done(&self, now: f32, t: f32) -> bool;

    /// `CTSStunt::Play`'s countdown, `dt` being the frame in the player's time.
    fn tick(&self, s: &mut StuntState, dt: f32);

    /// Port addition (plugin API version 6, `CombatState`): the game time at which timer `t`
    /// runs out, 0 when it isn't running.
    fn ends_at(&self, now: f32, t: f32) -> f32;

    /// `PreThink`'s countdown of `BlockPlayer`'s lock (`dt` is ×1.5 under double firerate).
    fn tick_block(&self, s: &mut StuntState, dt: f32);

    /// `NextStunt`'s byte for the cooldown `t`: tenths of a second.
    fn next_stunt_byte(&self, now: f32, t: f32) -> u8;

    /// `GoWallJump` (0x9a7d4) / `GoWallClimb` (0x9aa1c): the cooldown after a wall stunt with
    /// the chain at `sum`, as the build's x87 holds it (`GoWallJump` under `Msvc` doesn't reload
    /// the stored `float`), and `stored`, the chain as stored (`m_flWallChain`).
    fn wall_chain_cooldown(&self, now: f32, time_scale: f32, sum: f64, stored: f32) -> f32;

    /// `HandleStunt` / `GoWheel`: the "coyote time" an air stunt is still allowed after leaving
    /// the ground at `last_ground`: 0.7 s of the player's time. `slow_mul` is `GiveSlowMul`
    /// (`stunt::slow_mul`: the time scale, at least 0.01, through the plugin hook), called only
    /// by a ruleset that reads it.
    fn air_stunt_allowed(&self, now: f32, time_scale: f32, slow_mul: &mut dyn FnMut() -> f32, last_ground: f32) -> bool;

    /// A weapon delay (`m_flNextAttack`, a relative timer of the gun) a stunt sets, of `secs`
    /// seconds of the player's time: the gun counts it down in the player's time.
    fn attack_delay(&self, time_scale: f32, secs: f64) -> f32;

    // Audit round 2: C

    /// `BlockPlayer(t)`'s lock (`StuntState::block_timer`) for the `t` a ruleset's weapon passed
    /// (`GunHost::block_player`), at `now` with the time scale `time_scale`.
    fn block_end(&self, now: f32, time_scale: f32, t: f32) -> f32;
}

/// TS 3.0's coyote-time test: refused when `0.7 / slowmul < time - lastGroundTime`, the
/// difference stored as a `double` and the `double` 0.7 divided on the x87. `mp.dll`
/// (`HandleStunt` 0x10069419..0x10069437, `fdivr`) divides at 53 bits and refuses on C0 (a NaN
/// too); `ts_i386.so` (0x8b12b..0x8b155, the same in `GoWheel`) divides in extended precision
/// and refuses only when the difference is above the quotient (`test ah,0x41; je`).
// The negated compare is the x87 branch's: a NaN (unordered) takes the other way.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn coyote_allows(arith: shared::pm::Arith, now: f32, slow_mul: f32, last_ground: f32) -> bool {
    use shared::pm::{Arith, Ext, below87};
    let diff = f64::from(now) - f64::from(last_ground);
    match arith {
        Arith::Msvc => !below87(0.7 / f64::from(slow_mul), diff),
        Arith::Gcc => !(Ext::from_f64(diff) > Ext::from_f64(0.7) / Ext::from_f32(slow_mul)),
    }
}

/// A test helper: TS 3.0's relative countdown clock, for core tests that need a [`StuntClock`]
/// without depending on the 3.0 plugin (which depends on this crate).
#[cfg(test)]
pub(crate) mod countdown {
    use super::*;

    pub struct Countdown;

    impl StuntClock for Countdown {
        fn start(&self, now: f32, time_scale: f32, secs: f64) -> f32 {
            let _ = (now, time_scale);
            secs as f32
        }
        fn running(&self, now: f32, t: f32) -> bool {
            let _ = now;
            t > 0.0
        }
        fn done(&self, now: f32, t: f32) -> bool {
            let _ = now;
            t < 0.01
        }
        fn tick(&self, s: &mut StuntState, dt: f32) {
            if s.recover != 0.0 {
                s.recover = (s.recover - dt).max(0.001);
            }
            s.next_stunt = (s.next_stunt - dt).max(0.0);
            s.close_attack_cd = (s.close_attack_cd - dt).max(0.0);
            s.prone_lock = (s.prone_lock - dt).max(0.0);
        }
        fn ends_at(&self, now: f32, t: f32) -> f32 {
            if self.done(now, t) { 0.0 } else { now + t }
        }
        fn tick_block(&self, s: &mut StuntState, dt: f32) {
            if s.block_timer != 0.0 {
                s.block_timer = (s.block_timer - dt).max(0.001);
            }
        }
        fn next_stunt_byte(&self, now: f32, t: f32) -> u8 {
            let _ = now;
            shared::protocol::stunt::encode_next_stunt(t)
        }
        fn wall_chain_cooldown(&self, now: f32, time_scale: f32, sum: f64, stored: f32) -> f32 {
            let _ = (now, time_scale, stored);
            sum.max(0.5) as f32
        }
        fn air_stunt_allowed(&self, now: f32, time_scale: f32, slow_mul: &mut dyn FnMut() -> f32, last_ground: f32) -> bool {
            let _ = time_scale;
            coyote_allows(crate::rules::cvars::pm_arith(), now, slow_mul(), last_ground)
        }
        fn attack_delay(&self, time_scale: f32, secs: f64) -> f32 {
            let _ = time_scale;
            secs as f32
        }
        fn block_end(&self, now: f32, time_scale: f32, t: f32) -> f32 {
            self.start(now, time_scale, f64::from(t))
        }
    }
}
