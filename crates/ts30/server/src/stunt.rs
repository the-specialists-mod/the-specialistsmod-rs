//! TS 3.0's stunts (decomp `server/decomp/CTSStunt.c`): the stunt values, the relative stunt
//! clock and the slot limits.

use goldsrc::sys::{IN_DUCK, IN_JUMP};
use shared::ts::stunt::{DEAD_LOCK, FREEZE_ANGLES, GO_DUCK, RECOVER, ROLLING};
use ts_server::player::Player;
use ts_server::stunt::StuntState;
use ts_server::stunt::rules::{Dive, FirePose2, StuntClock, StuntRules, coyote_allows};

/// `CanDiveHigh/Medium/Low` (0x9a494...): used slots (`81 - TSSpace`) must be below 42/52/62
/// ("40 for high, 50 for medium, 60 for low"); The One (pl+0x7b9, `Player::ts.is_the_one`, set
/// by `SetPlayerTheOne`) ignores the limits. `SlotBonus()` is 0.
pub fn can_dive(p: &Player, dive: Dive) -> bool {
    let limit = match dive {
        Dive::High => 42,
        Dive::Medium => 52,
        Dive::Low => 62,
    };
    p.ts.is_the_one || p.slots_used < limit
}

/// TS 3.0's stunt values.
pub const STUNTS: StuntRules = StuntRules {
    dive_roll_out_recover: 0.5625,
    flip_recover: 0.375,
    get_up_recover: 0.75,
    wall_landing_recover: 0.2625,
    wall_landing_sets_recover: false,
    roll_recover: 0.75,
    super_jump_landing_recover: 0.75,
    super_jump_roll_attack_delay: 0.75,
    get_up_buttons: shared::ts::IN_ALT1 as i32 | (IN_DUCK | IN_JUMP) as i32,
    wall_dive: true,
    wall_climb_while_gliding: true,
    stunt_ends_close_combat: true,
    wheel_and_roll_wait_for_kick: true,
    kick_cooldown_on_try: false,
    wall_jump_pose_holds_until_recovered: false,
    wall_climb_pose_lets_dive_through: true,
    goduck_holds: false,
    side_roll_holds_on_strafe_keys: false,
    melee_gait_follows: true,
    gait_idle_anim_extension: true,
    anim_end_times: false,
    side_roll_normalizes_forward: false,
    fire_pose_2: FirePose2::AfterHold,
    idle_anim_extension: true,
    aim_pose_while_zoomed: true,
    melee_gait_speed: 120.0,
    bullet_time_dodge: Some(0.55),
    bullet_time_attack_delay: None,
    recover_end_clears: ROLLING | FREEZE_ANGLES | GO_DUCK | RECOVER | DEAD_LOCK,
    recover_end_unblocks: true, // Audit round 2: E
    // Animate's directions: mp.dll's out-of-line Normalize (0x100148f0) keeps 1/len unrounded.
    anim_normalize_stores_reciprocal: false,
    // CheckForBreakables: z, x, y (0x1006a100..0x1006a121), C0/C3 tests.
    breakables_ieee_xyz: false,
};

/// TS 3.0's [`StuntClock`]: relative countdowns of the player's own time, counted down each
/// frame by `slowmul * frametime` (`CTSStunt::Play` 0x9a014). The recovery and `BlockPlayer`'s
/// lock rest at 0.001; every timer counts as over below 0.01.
pub struct Relative;

impl StuntClock for Relative {
    /// `secs` seconds of the player's time: stored as a `float` countdown.
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

    /// `CTSStunt::Play`'s countdown: each timer ticks down by `dt` (the frame in the player's
    /// time) and rests at its floor.
    fn tick(&self, s: &mut StuntState, dt: f32) {
        if s.recover != 0.0 {
            s.recover = (s.recover - dt).max(0.001);
        }
        s.next_stunt = (s.next_stunt - dt).max(0.0);
        s.close_attack_cd = (s.close_attack_cd - dt).max(0.0);
        s.prone_lock = (s.prone_lock - dt).max(0.0);
    }

    /// `now + t`: the seconds left plus the current time, 0 when over.
    fn ends_at(&self, now: f32, t: f32) -> f32 {
        if self.done(now, t) { 0.0 } else { now + t }
    }

    fn tick_block(&self, s: &mut StuntState, dt: f32) {
        if s.block_timer != 0.0 {
            s.block_timer = (s.block_timer - dt).max(0.001);
        }
    }

    /// `(int)(t * 10)` (truncated).
    fn next_stunt_byte(&self, now: f32, t: f32) -> u8 {
        let _ = now;
        shared::protocol::stunt::encode_next_stunt(t)
    }

    /// `max(0.5, sum)` stored as a `float`.
    fn wall_chain_cooldown(&self, now: f32, time_scale: f32, sum: f64, stored: f32) -> f32 {
        let _ = (now, time_scale, stored);
        sum.max(0.5) as f32
    }

    /// [`coyote_allows`] in the server's arithmetic.
    fn air_stunt_allowed(&self, now: f32, time_scale: f32, slow_mul: &mut dyn FnMut() -> f32, last_ground: f32) -> bool {
        let _ = time_scale;
        coyote_allows(ts_server::rules::cvars::pm_arith(), now, slow_mul(), last_ground)
    }

    fn attack_delay(&self, time_scale: f32, secs: f64) -> f32 {
        let _ = time_scale;
        secs as f32
    }

    /// [`start`](Self::start)'s timer of `t` seconds of the player's time.
    fn block_end(&self, now: f32, time_scale: f32, t: f32) -> f32 {
        self.start(now, time_scale, f64::from(t))
    }
}

pub static RELATIVE: Relative = Relative;

#[cfg(test)]
mod tests {
    use super::*;
    use ts_server::stunt::StuntState;

    /// The `f64` values convert back to the `float`s TS 3.0 (and the port before the hooks)
    /// used.
    #[test]
    fn core_values_are_the_floats() {
        let c = STUNTS;
        assert_eq!(c.dive_roll_out_recover as f32, 0.5625f32);
        assert_eq!(c.flip_recover as f32, 0.375f32);
        assert_eq!(c.get_up_recover as f32, 0.75f32);
        assert_eq!(c.wall_landing_recover as f32, 0.2625f32);
        assert_eq!(c.roll_recover as f32, 0.75f32);
        assert_eq!(c.super_jump_landing_recover as f32, 0.75f32);
        assert_eq!(c.super_jump_roll_attack_delay as f32, 0.75f32);
        assert_eq!(0.55f64 as f32, 0.55f32);
        assert_eq!(0.7f64 as f32, 0.7f32);
        assert_eq!(c.get_up_buttons, 0x4006);
        assert_eq!(c.recover_end_clears, 0x40000 | 0x80000 | 0x4 | 0x1 | 0x2);
        assert!(c.wall_dive && c.wall_climb_while_gliding && c.stunt_ends_close_combat && c.wheel_and_roll_wait_for_kick);
        assert!(!c.wall_landing_sets_recover && c.recover_end_unblocks && !c.kick_cooldown_on_try);
        assert!(!c.wall_jump_pose_holds_until_recovered);
        assert!(c.wall_climb_pose_lets_dive_through && c.melee_gait_follows && c.gait_idle_anim_extension);
        assert!(!c.goduck_holds && !c.side_roll_holds_on_strafe_keys && !c.anim_end_times && !c.side_roll_normalizes_forward);
        assert_eq!((c.bullet_time_dodge, c.bullet_time_attack_delay), (Some(0.55), None));
        assert_eq!(c.fire_pose_2, FirePose2::AfterHold);
        assert!(c.idle_anim_extension && c.aim_pose_while_zoomed);
        assert_eq!(c.melee_gait_speed, 120.0);
    }

    /// The 3.0 clock is the relative countdown the stunt code had before the hooks.
    #[test]
    fn relative_clock() {
        let c = &RELATIVE;
        assert_eq!(c.start(100.0, 0.35, 0.75), 0.75);
        assert!(c.running(100.0, 0.001) && !c.running(100.0, 0.0));
        assert!(c.done(100.0, 0.001) && !c.done(100.0, 0.01));
        let mut s = StuntState::default();
        (s.recover, s.next_stunt, s.close_attack_cd, s.prone_lock) = (0.1, 0.05, 0.2, 0.0);
        c.tick(&mut s, 0.1);
        assert_eq!((s.recover, s.next_stunt, s.close_attack_cd, s.prone_lock), (0.001, 0.0, 0.1, 0.0));
        s.block_timer = 0.0;
        c.tick_block(&mut s, 0.1);
        assert_eq!(s.block_timer, 0.0, "a lock that isn't set stays unset");
        s.block_timer = 0.05;
        c.tick_block(&mut s, 0.1);
        assert_eq!(s.block_timer, 0.001);
        assert_eq!(c.next_stunt_byte(100.0, 0.55), 5, "truncated, as mp.dll and ts_i386.so");
        assert_eq!(c.wall_chain_cooldown(0.0, 1.0, 0.35, 0.35), 0.5);
        // The 53-bit sum mp.dll's GoWallJump keeps is stored as the float the chain holds.
        assert_eq!(c.wall_chain_cooldown(0.0, 1.0, f64::from(0.3f32) + 0.35, 0.65).to_bits(), 0x3f26_6667);
        assert_eq!(c.wall_chain_cooldown(0.0, 1.0, 30.0, 30.0), 30.0, "no cap");
        assert!(c.air_stunt_allowed(10.0, 1.0, &mut || 1.0, 9.4) && !c.air_stunt_allowed(10.0, 1.0, &mut || 1.0, 9.2));
        // Slowed to 0.5, the window is 1.4 s of real time; the window comes from GiveSlowMul.
        assert!(c.air_stunt_allowed(10.0, 0.5, &mut || 0.5, 8.7));
        assert!(!c.air_stunt_allowed(10.0, 0.5, &mut || 1.0, 8.7), "GiveSlowMul, not fuser1");
        assert_eq!(c.attack_delay(0.35, 0.75), 0.75);
    }

    #[test]
    fn can_dive_limits_and_the_one() {
        let mut p = Player { slots_used: 60, ..Player::default() };
        assert!(!can_dive(&p, Dive::High) && !can_dive(&p, Dive::Medium) && can_dive(&p, Dive::Low));
        p.slots_used = 41;
        assert!(can_dive(&p, Dive::High));
        p.slots_used = 81;
        p.ts.is_the_one = true;
        assert!(can_dive(&p, Dive::High) && can_dive(&p, Dive::Medium) && can_dive(&p, Dive::Low));
    }
}
