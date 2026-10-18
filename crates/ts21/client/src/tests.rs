//! TS 2.1's client hooks against the 2.1 binary's numbers, and the 3.0 plugin's
//! answers to the same questions, which must not change.

use super::*;
use ts_client::menus::buy::{self, BuyMenu, CASH, ITEM_KEVLAR, ITEM_STEALTH, Rng, Rules, page};

fn v21() -> &'static dyn ClientRules {
    PLUGIN.client
}

fn v30() -> &'static dyn ClientRules {
    ts30_client::PLUGIN.client
}

#[test]
fn crosshair_parts() {
    let shown = CrosshairParts { crosshair: true, divesteps: true, divesteps_last: true };
    let hidden = CrosshairParts { crosshair: false, divesteps: false, divesteps_last: true };
    assert_eq!(v21().crosshair_parts(13, 90.0), shown, "AK: crosshair, divesteps on top");
    for w in [0, 24, 25, 34, 35] {
        assert_eq!(v21().crosshair_parts(w, 90.0), hidden, "weapon {w}: nothing, not even divesteps");
    }
    assert_eq!(v21().crosshair_parts(18, 90.0), hidden, "unscoped Barrett");
    assert_eq!(v21().crosshair_parts(18, 40.0), hidden, "40 isn't zoomed");
    assert_eq!(v21().crosshair_parts(18, 39.9), shown, "m_iFOV is an int");
    assert_eq!(v21().crosshair_parts(18, 20.0), shown);

    // TS 3.0: divesteps first for every weapon; only the crosshair goes for melee and the M61.
    let both = CrosshairParts { crosshair: true, divesteps: true, divesteps_last: false };
    assert_eq!(v30().crosshair_parts(18, 90.0), both, "3.0 has a crosshair on the unscoped Barrett");
    assert_eq!(v30().crosshair_parts(25, 90.0), CrosshairParts { crosshair: false, ..both });
}

#[test]
fn crosshair_gap() {
    // M4A1 (2.1 accuracy 0.0075), predicted spread 0.2: the float-rounded sum and `/ 0.1f × 16`
    // land a pixel short of 3.0's 22 + 32.
    assert_eq!(v21().crosshair_gap(22, 0.2, 0.0075), 53);
    assert_eq!(v30().crosshair_gap(22, 0.2, 0.0075), 54);
    assert_eq!(v21().crosshair_gap(22, 0.1, 0.0), 38);
    assert_eq!(v21().crosshair_gap(12, 0.0, 0.032), 12, "no spread: the base (laser on)");
    for s in [f32::NAN, f32::INFINITY, f32::MAX, -f32::MAX] {
        assert!(v21().crosshair_gap(22, s, 0.01).abs() <= 0x10000);
    }
}

#[test]
fn divestep_limits() {
    // 2.1: top ≤ 21 used (and no kevlar in team modes), centre ≤ 41, bottom ≤ 71.
    assert_eq!(v21().divesteps_blocked(21, 0, 0), [false; 3]);
    assert_eq!(v21().divesteps_blocked(22, 0, 0), [true, false, false]);
    assert_eq!(v21().divesteps_blocked(41, 0, 0), [true, false, false]);
    assert_eq!(v21().divesteps_blocked(42, 0, 0), [true, true, false]);
    assert_eq!(v21().divesteps_blocked(71, 0, 0), [true, true, false]);
    assert_eq!(v21().divesteps_blocked(72, 0, 0), [true; 3]);
    assert_eq!(v21().divesteps_blocked(10, 50, 1), [false; 3], "kevlar counts only when m_Teamplay > 1");
    assert_eq!(v21().divesteps_blocked(10, 50, 2), [true, false, false]);
    assert_eq!(v21().divesteps_blocked(-174, 0, 0), [false; 3], "signed: 255 free slots");
    // 3.0: 42 / 52 / 62, no kevlar rule.
    assert_eq!(v30().divesteps_blocked(41, 100, 3), [false; 3]);
    assert_eq!(v30().divesteps_blocked(42, 0, 0), [true, false, false]);
    assert_eq!(v30().divesteps_blocked(52, 0, 0), [true, true, false]);
    assert_eq!(v30().divesteps_blocked(62, 0, 0), [true; 3]);
}

#[test]
fn stunt_cooldown() {
    let c = StuntCooldown { remaining: 0.0, received_at: 10.0, tenths: 5 };
    assert!(v21().stunt_cooldown(c, 10.4));
    assert!(v21().stunt_cooldown(c, 10.5), "red until the expiry has passed");
    assert!(!v21().stunt_cooldown(c, 10.51));
    let reset = StuntCooldown::default();
    assert!(v21().stunt_cooldown(reset, 0.0), "expiry 0 at time 0");
    assert!(!v21().stunt_cooldown(reset, 0.01));
    assert!(v21().stunt_cooldown(reset, f32::NAN), "unordered compare: red");
    // 3.0 runs the countdown down instead.
    assert!(v30().stunt_cooldown(StuntCooldown { remaining: 0.01, ..c }, 99.0));
    assert!(!v30().stunt_cooldown(StuntCooldown { remaining: 0.009, ..c }, 10.0));
}

#[test]
fn fov() {
    assert_eq!(v21().pinned_default_fov(), Some(80.0));
    assert_eq!(v30().pinned_default_fov(), None);
    for (cur, snaps) in [(-1, true), (0, true), (1, false), (20, false), (80, false), (81, true), (110, true)] {
        assert_eq!(v21().fov_snaps(cur), snaps, "2.1, m_iFOV {cur}");
    }
    for (cur, snaps) in [(-1, true), (0, true), (1, false), (80, false), (110, false)] {
        assert_eq!(v30().fov_snaps(cur), snaps, "3.0, m_iFOV {cur}");
    }
}

#[test]
fn input() {
    assert!(!v21().input_gate() && !v21().attack_switch() && !v21().attack_sends_respawn());
    assert!(v30().input_gate() && v30().attack_switch() && v30().attack_sends_respawn());
}

#[test]
fn map_effects_and_icon() {
    assert!(!v21().map_effects() && v30().map_effects());
    assert!(v21().slowmo_icon() && !v30().slowmo_icon());
    // 2.1 has no `cl_slowfade` tint (issue #152); 2.1 mode keeps 3.0's as a visual deviation (#324).
    assert!(v21().slowmo_tint() && v30().slowmo_tint());
}

/// Issues #193, #194: 2.1's weapon `Think` (0x100899f0) has no laser-colour send, no fire-mode click and
/// no heartbeat or breathing; 3.0's (0x1004ce10) has all three.
#[test]
fn weapon_think_parts() {
    assert_eq!(v21().think_parts(), ThinkParts { laser_color: false, firemode_click: false, sounds: false });
    assert_eq!(v30().think_parts(), ThinkParts { laser_color: true, firemode_click: true, sounds: true });
}

#[test]
fn fire_pitch_is_2_1s_helper() {
    // 0x100738a0: 100 + (s == 0 ? 300 : max(s * 300, 150)), s = TSSlowMo / 100, raw.
    assert_eq!(v21().fire_pitch(1.0, 0.0), 400, "normal speed");
    assert_eq!(v21().fire_pitch(0.35, 35.0), 250, "105 <= 150: the floor");
    assert_eq!(v21().fire_pitch(0.5, 50.0), 250, "exactly 150");
    assert_eq!(v21().fire_pitch(0.8, 80.0), 340);
    assert_eq!(v21().fire_pitch(0.01, 1.0), 250, "never below 250");
    assert_eq!(fire_pitch(f32::NAN), 250, "unordered compare: the floor");
    // 3.0 unchanged: 100 - (int)(ts * -300) of the eased scale, the product unrounded (#273):
    // 0.35f × −300 = −104.9999982 and 0.01f × −300 = −2.9999999, so 204 and 102.
    assert_eq!(v30().fire_pitch(1.0, 0.0), 400);
    assert_eq!(v30().fire_pitch(0.35, 35.0), 204);
    assert_eq!(v30().fire_pitch(0.01, 1.0), 102);
}

#[test]
fn kungfu_bar() {
    assert!(!v21().kungfu_exhausted(0.0));
    assert!(v30().kungfu_exhausted(0.0) && v30().kungfu_exhausted(0.01) && !v30().kungfu_exhausted(0.02));
    assert_eq!(v21().kungfu_power_on_reset(), Some(100.0));
    assert_eq!(v30().kungfu_power_on_reset(), None);

    let r = KungfuRegen { power: 50.0, step: 0.5, now: 11.0, last: 10.0, slowmo_factor100: 0.0 };
    assert_eq!(v21().kungfu_regen(r), 65.0, "15/s of real time at normal speed");
    assert_eq!(v21().kungfu_regen(KungfuRegen { slowmo_factor100: 35.0, ..r }), 55.25, "× 0.35 slowed");
    assert_eq!(v21().kungfu_regen(KungfuRegen { slowmo_factor100: 5.0, ..r }), 51.5, "floor 0.1");
    assert_eq!(v21().kungfu_regen(KungfuRegen { power: 99.0, ..r }), 100.0);
    assert_eq!(v21().kungfu_regen(KungfuRegen { power: 100.0, ..r }), 100.0);
    assert_eq!(v21().kungfu_regen(KungfuRegen { power: -5.0, ..r }), 10.0, "no 1/s branch below 0");
    assert!(v21().kungfu_regen(KungfuRegen { power: f32::NAN, ..r }).is_nan(), "NaN isn't below 100");
    // 3.0: 15 per step (frame time × time scale), 1 per step while negative.
    assert_eq!(v30().kungfu_regen(r), 57.5);
    assert_eq!(v30().kungfu_regen(KungfuRegen { power: -5.0, ..r }), -4.5);
}

/// Picks from a fixed list of `long` results.
struct Seq(Vec<i32>);

impl Rng for Seq {
    fn long(&mut self, lo: i32, hi: i32) -> i32 {
        if self.0.is_empty() { lo } else { self.0.remove(0).clamp(lo, hi) }
    }
    fn float(&mut self, lo: f32, _hi: f32) -> f32 {
        lo
    }
}

fn rules(ruleset: &dyn ClientRules, gamemode: i32) -> Rules {
    Rules {
        gamemode,
        restriction: 0x1f,
        cash: buy::cash(ruleset.buy_cash_based(gamemode, 0), 1234),
        menu: ruleset.buy_menu(),
        ..Rules::default()
    }
}

#[test]
fn buy_cash() {
    assert!(!v21().buy_cash_based(0, 0), "DM without usecash: 30000");
    assert!(v21().buy_cash_based(0, 1), "usecash (SrvSett 1)");
    assert!(v21().buy_cash_based(2, 0), "round modes");
    assert!(!v30().buy_cash_based(3, 1), "3.0's CashBased() is always off");
    assert_eq!(buy::cash(true, 4000), 4000);
    assert_eq!(buy::cash(false, 4000), CASH);
}

#[test]
fn buy_menu_items() {
    let r = rules(v21(), 0);
    let mut m = BuyMenu::default();
    // Kevlar takes no slots, and the slot check doesn't apply to items.
    m.current.slots = 81;
    assert!(m.add(ITEM_KEVLAR, 0, false, true, &r));
    assert_eq!((m.current.slots, m.current.total), (81, 1900));
    m.remove(0, &r);
    assert_eq!(m.current.slots, 81);
    assert_eq!(buy::slots_for(ITEM_KEVLAR, false, true, &r), 0);
    // Stealth shoes aren't restricted...
    assert!(!buy::restricted(ITEM_STEALTH, true, &r));
    // ...but the items page only opens when m_Teamplay > 1.
    m.page = page::CATEGORY;
    m.handle_key(page::CATEGORY, 6, &r, &mut Seq(vec![]));
    assert_eq!(m.page, page::CATEGORY);
    m.handle_key(page::CATEGORY, 6, &rules(v21(), 2), &mut Seq(vec![]));
    assert_eq!(m.page, page::ITEMS);

    // TS 3.0: kevlar is 10 slots, stealth shoes restricted outside team modes, page always open.
    let r = rules(v30(), 0);
    assert_eq!(buy::slots_for(ITEM_KEVLAR, false, true, &r), 10);
    assert!(buy::restricted(ITEM_STEALTH, true, &r));
    let mut m = BuyMenu { page: page::CATEGORY, ..BuyMenu::default() };
    m.handle_key(page::CATEGORY, 6, &r, &mut Seq(vec![]));
    assert_eq!(m.page, page::ITEMS);
}

#[test]
fn buy_menu_akimbo_keys_and_lists() {
    ts30_gameplay::tables::make_default();
    // No akimbo fix-up: a Desert Eagle (no akimbo version) bought with key 6 is "akimbo".
    let r = rules(v21(), 0);
    let mut m = BuyMenu::default();
    assert!(m.add(12, 0, true, false, &r));
    let e = m.current.entries[0];
    assert!(e.akimbo);
    assert_eq!(e.cost, buy::cost(12, 0, true, false));
    let r30 = rules(v30(), 0);
    let mut m = BuyMenu::default();
    assert!(m.add(12, 0, true, false, &r30));
    assert!(!m.current.entries[0].akimbo, "3.0 buys it single");

    // Keys 1..10 only (no `a` row).
    let mut m = BuyMenu { page: page::WEAPONS, ..BuyMenu::default() };
    m.handle_key(page::WEAPONS, 11, &r, &mut Seq(vec![]));
    assert_eq!(m.page, page::WEAPONS);

    // The handguns: no 2 and no 36, and 8 in their place.
    let col: Vec<i32> = buy::table(&r.menu).iter().map(|row| row[0]).filter(|&id| id != -1).collect();
    assert_eq!(col, [1, 9, 12, 14, 8, 21, 22, 28, 31]);
    let col30: Vec<i32> = buy::table(&r30.menu).iter().map(|row| row[0]).filter(|&id| id != -1).collect();
    assert_eq!(col30, [1, 9, 12, 14, 2, 21, 22, 28, 31, 36]);
}

#[test]
fn buy_menu_reset_and_random() {
    ts30_gameplay::tables::make_default();
    // No `cl_buykevlar`: a reset config is empty even with the cvar on.
    let r = Rules { buy_kevlar: true, cash: CASH, ..rules(v21(), 2) };
    let mut m = BuyMenu::default();
    m.reset_config(&r);
    assert!(m.current.entries.iter().all(|e| e.id == 0));
    let r30 = Rules { buy_kevlar: true, cash: CASH, ..rules(v30(), 2) };
    m.reset_config(&r30);
    assert_eq!(m.current.entries[0].id, ITEM_KEVLAR);

    // Random config, p = 0.5 (a pistol and an SMG): melee rand(0,5) = 0 → combat knife, its
    // rand(0,2) = 1 → single; pistol rand(1,9) = 5 → the akimbo Berettas (8); SMG rand(1,6) = 2
    // → MP5SD (6); then attachment rolls.
    let mut m = BuyMenu::default();
    m.random_config(0.5, &r, &mut Seq(vec![0, 1, 5, 0, 0, 0, 2, 0, 0, 0]));
    let ids: Vec<i32> = m.current.entries.iter().map(|e| e.id).filter(|&id| id != 0).collect();
    assert_eq!(ids, [25, 8, 6]);
    // The same rolls in 3.0: rand(0,9) = 5 is the single Beretta (2), and kevlar comes first.
    let mut m = BuyMenu::default();
    m.random_config(0.5, &r30, &mut Seq(vec![0, 1, 5, 0, 0, 0, 0, 2, 0, 0, 0]));
    let ids: Vec<i32> = m.current.entries.iter().map(|e| e.id).filter(|&id| id != 0).collect();
    assert_eq!(ids[..2], [ITEM_KEVLAR, 25]);
    assert_eq!(ids[2], 2);
}

/// 2.1's weapon `VidInit` (`0x100894c0`) re-runs its buy menu `Init` (`0x1006aa70`) as 3.0 does
/// (issue #189): after it the next open rolls a 2.1 random loadout instead of the last config.
#[test]
fn buy_menu_init_rerolls_in_2_1() {
    ts30_gameplay::tables::make_default();
    let r = Rules { cash: CASH, ..rules(v21(), 2) };
    let mut m = BuyMenu::default();
    m.open(&r, &mut Seq(vec![0, 1, 5, 0, 0, 0, 2, 0, 0, 0]));
    m.close();
    m.last = m.current.clone();
    let mut m = BuyMenu::new(m.saved.clone());
    assert!(m.random_first && m.last.entries.iter().all(|e| e.id == 0));
    m.open(&r, &mut Seq(vec![0, 1, 5, 0, 0, 0, 2, 0, 0, 0]));
    let ids: Vec<i32> = m.current.entries.iter().map(|e| e.id).filter(|&id| id != 0).collect();
    assert_eq!(ids, [25, 8, 6], "2.1's random config again (p = 0.5)");
}

fn bob_input(time: f32, speed: f32, slowmo_factor100: f32) -> ts_client::view::BobInput {
    ts_client::view::BobInput {
        on_ground: true,
        time,
        frametime: 0.01,
        viewheight: 28.0,
        simvel: [speed, 0.0, 0.0],
        ts: 1.0,
        cl_bob: 4.0,
        slowmo_factor100,
        forward: [1.0, 0.0, 0.0],
        right: [0.0, -1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        ..Default::default()
    }
}

fn near(a: f32, b: f64) -> bool {
    (a as f64 - b).abs() < 1e-5
}

/// 2.1's `V_CalcBob` (0x100b5c80): the phase rate ignores the speed, there are no `cl_bob` levels
/// or bob pitch, and it fills the camera bob that `V_CalcNormalRefdef` adds to `vieworg`.
#[test]
fn view_bob() {
    let mut b = ts_client::view::Bob::default();
    let amp = v21().view_bob(&mut b, &bob_input(1.0, 200.0, 0.0));
    assert_eq!((b.phase, amp, b.peak, b.pitch), (3.5, 2.0, 2.0, 0.0), "ft × 3.5 × 100; |v| × 0.01");
    let (s2, s1) = ((7.0f64.to_radians()).sin() * 2.0, (3.5f64.to_radians()).sin() * 2.0);
    let eye = b.eye.expect("2.1 has a camera bob");
    assert!(eye[0] == 0.0 && near(eye[1], s1 * 0.9) && near(eye[2], s2 * -0.9 * 1.6), "{eye:?}");
    assert!(near(b.ofs[0], -0.25) && near(b.ofs[1], -s1 * 0.3), "{:?}", b.ofs);
    // z: `s2·0.11 + 0.5 − 0.65·amp`, smoothed from 0 by `d × 0.2 × ft × 100`.
    assert!(near(b.ofs[2], (s2 * 0.11 + 0.5 - 1.3) * 0.2), "{:?}", b.ofs);
    let mut lvl0 = ts_client::view::Bob::default();
    v21().view_bob(&mut lvl0, &ts_client::view::BobInput { cl_bob: 0.0, ..bob_input(1.0, 200.0, 0.0) });
    assert_eq!(lvl0, b, "no cl_bob levels");

    // Half speed slow motion: half the phase step.
    v21().view_bob(&mut b, &bob_input(1.01, 200.0, 50.0));
    assert_eq!(b.phase, 5.25);
    let eye = b.eye;
    // In the air the amplitude halves and the camera bob keeps its last value.
    let air = ts_client::view::BobInput { on_ground: false, ..bob_input(1.02, 200.0, 0.0) };
    v21().view_bob(&mut b, &air);
    assert_eq!((b.amp, b.eye), (1.0, eye), "the camera bob is stale, not cleared");
    // Back on the ground below the peak: `(|v| × 0.01 + amp) × 0.5`.
    assert_eq!(v21().view_bob(&mut b, &bob_input(1.03, 200.0, 0.0)), 1.5);
    assert_eq!(b.peak, 2.0);

    // TS 3.0's bob: speed-scaled phase, a bob pitch, no camera bob.
    let mut c = ts_client::view::Bob::default();
    v30().view_bob(&mut c, &bob_input(1.0, 200.0, 0.0));
    assert_eq!(c.eye, None);
    assert!(c.phase != 3.5 && c.pitch != 0.0, "{c:?}");
    let mut d = ts_client::view::Bob::default();
    ts30_client::view::bob(&mut d, &bob_input(1.0, 200.0, 0.0));
    assert_eq!(c, d, "the 3.0 plugin runs its bob");
}

/// 2.1's client bullets (issue #165): `40 × v × s` with the raw factor (÷5 in bullet time), one
/// fixed step every 0.1 s, drawn while a factor is set; 3.0's curve, stepping and gates stay.
#[test]
fn client_bullet_speed_and_stepping() {
    use ts_client::events::BulletStep;
    let x = [1.0, 0.0, 0.0];
    let at = |factor100: f32, bullet_time: bool, dt: f32, ts: f32| BulletStep {
        velocity: 330.0,
        time_scale: ts,
        bullet_time,
        slowmo_factor100: factor100,
        dt,
    };
    // A fixed step, whatever the time since the last.
    assert_eq!(v21().client_bullet_step(x, &at(0.0, false, 0.05, 1.0)), [1320.0, 0.0, 0.0]);
    assert_eq!(v21().client_bullet_step(x, &at(30.0, false, 0.1, 0.3))[0], 1320.0 * (30.0f32 / 100.0));
    assert!((v21().client_bullet_step(x, &at(30.0, true, 0.1, 0.3))[0] - 79.2).abs() < 1e-4, "bullet time ÷5");
    // 3.0: velocity × 40 × dt × GetBulletVelocityMult on the eased scale.
    assert_eq!(v30().client_bullet_step(x, &at(0.0, false, 0.1, 1.0))[0], 1320.0);
    assert!((v30().client_bullet_step(x, &at(30.0, false, 0.1, 0.3))[0] - 396.0).abs() < 1e-3);
    assert!((v30().client_bullet_step(x, &at(30.0, true, 0.1, 0.3))[0] - 1320.0 * 0.3 / 9.1).abs() < 1e-3);

    assert_eq!(v21().client_bullet_next_update(10.0, 1), (10.0f64 + 0.1) as f32, "0.1 s, realbullet or not");
    assert_eq!(v21().client_bullet_next_update(10.0, 5), (10.0f64 + 0.1) as f32);
    assert_eq!(v30().client_bullet_next_update(10.0, 1), 0.05 + 10.0);
    assert_eq!(v30().client_bullet_next_update(10.0, 5), 10.0, "3.0: every frame");

    // Queueing: `realbullet == 1 || TSSlowMo != 0` (3.0: `realbullet != 0 || ts < 0.99`).
    assert!(v21().client_bullets_queue(1, 1.0, 0.0) && v30().client_bullets_queue(1, 1.0, 0.0));
    assert!(!v21().client_bullets_queue(2, 1.0, 0.0) && v30().client_bullets_queue(2, 1.0, 0.0));
    assert!(!v21().client_bullets_queue(0, 0.5, 0.0) && v30().client_bullets_queue(0, 0.5, 0.0), "easing back");
    assert!(v21().client_bullets_queue(0, 1.0, 30.0) && !v30().client_bullets_queue(0, 1.0, 30.0));
    assert!(v21().client_bullet_draws(1.0, 30.0) && !v21().client_bullet_draws(0.5, 0.0));
    assert!(v30().client_bullet_draws(0.5, 0.0) && !v30().client_bullet_draws(1.0, 30.0));

    // Range falloff: the quotient goes through a float.
    assert_eq!(v21().client_bullet_range_index(800.0, 80.0, false), 10);
    assert_eq!(v21().client_bullet_range_index(1234.5, 90.0, false), 13);
    assert_eq!(v21().client_bullet_range_index(330.0, 80.0, true), 11);
    assert_eq!(v30().client_bullet_range_index(1234.5, 90.0, false), 13);
    assert_eq!(v30().client_bullet_range_index(330.0, 80.0, true), 11);
}

/// 2.1's draw (0x10069540): the bullet models and the `cl_wavedetail` trails.
#[test]
fn client_bullet_looks() {
    use crate::bullets::*;
    assert_eq!(
        [model_for(3), model_for(4), model_for(6), model_for(8), model_for(1)],
        ["models/pellet.mdl", "models/long_bullet.mdl", "models/long_bullet.mdl", "models/long_bullet.mdl", "models/bullet.mdl"]
    );
    assert_eq!([lod_body(50.0, 90), lod_body(100.0, 90), lod_body(200.0, 90), lod_body(300.0, 90)], [3, 2, 1, 0]);
    assert_eq!([lod_body(1000.0, 10), lod_body(400.0, 40), lod_body(400.0, 41)], [3, 2, 0], "scoped: more detail further out");
    // The add (0x10069450): body 0 at low effects detail (`0x1031b819`), at any distance (#259).
    assert_eq!([model_body(true, 50.0, 90), model_body(true, 1000.0, 10)], [0, 0]);
    assert_eq!([model_body(false, 50.0, 90), model_body(false, 1000.0, 10)], [3, 3]);

    assert_eq!(trails(0.0, true), Trails::None);
    assert_eq!(trails(1.0, true), Trails::Trail);
    assert_eq!(trails(2.0, false), Trails::Trail, "the wave needs bullet time");
    assert_eq!(trails(2.0, true), Trails::TrailAndWave);
    assert_eq!(trails(3.0, true), Trails::Spiral);
    assert_eq!(trails(f32::NAN, true), Trails::Spiral, "unordered compares count as greater");

    assert_eq!(
        [spin_step(0.0), spin_step(30.0), spin_step(10.0), spin_step(5.0)],
        [90.0, ((30.0f32 / 100.0) as f64 * 90.0) as f32, 22.5, 22.5]
    );
    assert_eq!(draw_step([1.0, 0.0, 0.0], 330.0, 0.3, 0.01)[0], 330.0f32 / 0.25 * 0.3 * 0.01 * 10.0);

    let up = [0.0, 0.0, 1.0];
    let t = trail_particle(7, [10.0, 0.0, 0.0], [0.0, 0.0, 0.0], 0.5, 0.5, up);
    assert_eq!((t.color, t.fadespeed, t.radius, t.life), (0.5, 0.02, 0.5, 0.0), "fixed width, not 3.0's 1 − ts²");
    assert_eq!(t.corners, Some([[0.0, 0.0, -0.5], [0.0, 0.0, 0.5], [10.0, 0.0, 0.5], [10.0, 0.0, -0.5]]));
    let w = wave_particle(7, [1.0, 2.0, 3.0], 1.5);
    assert_eq!((w.color, w.fadespeed, w.radius, w.corners), (0.6, 0.035, 1.5, None));

    let s3 = spiral_particles(7, [0.0; 3], [10.0, 0.0, 0.0], [0.0; 3], 1.0, 2.0, 3.0);
    assert_eq!(s3.len(), 3);
    assert_eq!(spiral_particles(7, [0.0; 3], [10.0, 0.0, 0.0], [0.0; 3], 1.0, 2.0, 3.5).len(), 5);
    let p = &s3[0];
    assert_eq!((p.color, p.fadespeed, p.radius), (0.7, 0.035, 1.0));
    let c = p.corners.unwrap();
    // Facing +x: right is −y, so t = 0 starts a radius out on −y.
    assert_eq!(c[0], [0.0, -1.0, 0.0]);
    assert_eq!(c[3], [10.0, -2.0, 0.0]);
    assert_eq!(p.uv.unwrap()[1][0], 1.0 / 3.0);
    assert_eq!(s3[2].uv.unwrap()[1][0], 1.0);
}

/// 2.1's weapon events (issue #160): builder parameters, reaches, the block branch, pitches and
/// attenuations; the 3.0 plugin keeps 3.0's.
#[test]
fn weapon_events() {
    use crate::events::*;
    use ts_client::gameplay::MeleeReach as M;
    let fx30 = &ts30_client::fx::FX;
    assert_eq!(v30().fx_style(), fx30);
    let fx = v21().fx_style();
    assert_eq!((fx.flash_color, fx.flash_fade, fx.flash_scale_speed, fx.third_person_flash), (1.0, 0.15, 0.1, 1.0));
    assert!(!fx.muzzle_blast && fx.muzzle_spark && fx30.muzzle_blast && !fx30.muzzle_spark);
    assert_eq!((fx.silenced_count, fx.silenced_color, fx.silenced_fade, fx.silenced_spin), (1.0, 1.0 / 640.0, Some(0.13), 60));
    assert_eq!((fx30.silenced_count, fx30.silenced_color), (0.7, 0.3 / 512.0));
    assert_eq!((fx.smoke_gravity, fx.drag, fx.debris_react), (0.1, 0.0, 0.0));
    assert_eq!((fx30.smoke_gravity, fx30.drag, fx30.debris_react), (0.01, 3.3, 0.3));
    assert!(fx.smoke_puffs_by_cl_fx && !fx.stretch && fx.debris_fade_any_mode && !fx.debris_count_by_quality);
    // Round 4: 2.1's silenced flash has no rise (0x10081324..0x100814a0), its spark moves with
    // the gun (0x1008118a), its cooldowns stretch by the raw factor (0x100962e0) and its impact
    // smoke flies at 16 (0x1007ea4b).
    assert_eq!((fx.silenced_jitter, fx.spark_moves, fx.raw_slow_cooldowns, fx.impact_smoke_speed), (false, true, true, Some(16.0)));
    assert_eq!((fx30.silenced_jitter, fx30.spark_moves, fx30.raw_slow_cooldowns, fx30.impact_smoke_speed), (true, false, false, None));

    // Reaches: close combat -56/48/38, the coldcock by its weapon id the same way, knife 38,
    // katana -54/48 (3.0: -55/45/35, 30, 35, -55/50).
    let reach = |r: &dyn ClientRules| {
        [
            M::CloseCombat(3),
            M::CloseCombat(2),
            M::CloseCombat(1),
            M::Coldcock(3),
            M::Coldcock(2),
            M::Coldcock(9),
            M::Knife,
            M::Katana(15),
            M::Katana(16),
        ]
        .map(|m| r.melee_reach(m))
    };
    assert_eq!(reach(v21()), [-56.0, 48.0, 38.0, -56.0, 48.0, 38.0, 38.0, -54.0, 48.0]);
    assert_eq!(reach(v30()), [-55.0, 45.0, 35.0, 30.0, 30.0, 30.0, 35.0, -55.0, 50.0]);

    assert!(!v21().close_combat_blocks() && v30().close_combat_blocks(), "2.1's event has no block branch");
    assert!(!v21().reload_sound_phases() && v30().reload_sound_phases());
    assert!(!v21().bullet_whizz_and_push() && v30().bullet_whizz_and_push());
    assert_eq!((v21().pellet_spray_gravity(), v30().pellet_spray_gravity()), (2.0, 0.0));
    assert_eq!((v21().shell_sound(11), v30().shell_sound(11), v30().shell_sound(5)), (1, 2, 1));
    // The pump fire's shells: 2.1 60-80 / 50-100 (0x10077031, 0x10077043), 3.0 20-40 / 0-20.
    assert_eq!((v21().pump_shell_speeds(), v30().pump_shell_speeds()), ([(60, 80), (50, 100)], [(20, 40), (0, 20)]));

    // The .454 Casull is 2.1's ammo 12 (0.3); 3.0's 12 is the .32 ACP (0.5).
    assert_eq!((v21().ammo_attenuation(12), v30().ammo_attenuation(12)), (0.3, 0.5));
    assert_eq!((v21().ammo_attenuation(6), v30().ammo_attenuation(6)), (0.2, 0.2));
    assert_eq!((v21().ammo_attenuation(40), v30().ammo_attenuation(40)), (0.4, 0.4));

    // The ricochet: `100 + 300 s` on the raw factor (400 at normal speed).
    assert_eq!([ricochet_pitch(0.0), ricochet_pitch(50.0), ricochet_pitch(10.0)], [400, 250, 130]);
    // The product is stored to a float first (0x1007ba52, issue #251): factor 29 is 86.9999975,
    // stored as 87.0; factor 1 gives 3.0 the same way.
    assert_eq!([ricochet_pitch(29.0), ricochet_pitch(1.0)], [187, 103]);

    assert_eq!(b"CMTPYVW".map(material_volume), [0.9, 0.9, 0.8, 0.8, 0.8, 0.5, 0.9].map(Some));
    assert_eq!(material_volume(b'F'), None, "flesh plays nothing");
}

/// The eye (issue #167): 2.1 adds the raw view height on every axis and follows the eye z at
/// `10·ft` (0x100b6b80), in the air too; 3.0 eases the view height (z only) and smooths stairs.
#[test]
fn eye_origin() {
    use ts_client::view::EyeZ;
    let (simorg, vh) = ([1.0, 2.0, 100.0], [0.5, 0.0, 28.0]);

    let mut e = EyeZ::default();
    let org = v21().eye_origin(&mut e, simorg, vh, true, 0.01);
    // From the static's 0: d = −128, step = −128 × 0.1 × ft × 100 on the x87.
    let step = (-128.0f64 * 0.1 * 0.01f32 as f64 * 100.0) as f32;
    assert_eq!(org, [1.5, 2.0, 0.0 - step]);
    assert!((org[2] - 12.8).abs() < 1e-5);
    assert_eq!(e.follow_z, org[2], "the static keeps the eased z");
    // No ground test: the same frame in the air eases the same.
    let mut air = EyeZ::default();
    assert_eq!(v21().eye_origin(&mut air, simorg, vh, false, 0.01), org);
    // A slow frame (step past d) lands on the target.
    let mut slow = EyeZ::default();
    assert_eq!(v21().eye_origin(&mut slow, simorg, vh, true, 1.5), [1.5, 2.0, 128.0]);
    // The body cam runs the same follow on the same state.
    let z = v21().body_cam_z(&mut slow, 120.0, false, 0.01);
    let step = ((128.0f64 - 120.0) * 0.1 * 0.01f32 as f64 * 100.0) as f32;
    assert_eq!((z, slow.follow_z), (128.0 - step, 128.0 - step));

    // TS 3.0: the view height on z only, eased at 15·ft (4.2), then the stair smoothing from z 0.
    let mut e = EyeZ::default();
    let org = v30().eye_origin(&mut e, simorg, vh, true, 0.01);
    assert_eq!((org[0], org[1]), (1.0, 2.0));
    assert!((org[2] - 104.2 * 0.15).abs() < 1e-4, "{}", org[2]);
    assert_eq!(e.follow_z, 0.0, "3.0 doesn't follow");
}

/// Engine titles that know nothing: the 2.1 menu mustn't need them.
struct NoTitles;
impl ts_client::hud::text::Titles for NoTitles {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

/// Issue #111: 2.1's buy menu shows its hard-coded English, whatever `titles.txt` says.
#[test]
fn buy_menu_text() {
    let m21 = v21().buy_menu();
    let t = |msg: &str| buy::text(&m21, &NoTitles, msg);
    // Every row, label and message the port's menu draws or prints.
    let used = [
        "0 #Buy_Cancel",
        "0 #Buy_GoBack",
        "0 #Chg_Cancel",
        "1 #Buy_Buy",
        "1 #Buy_EmptyConfig",
        "1 #Buy_UseLastConfig",
        "1 #Buy_UseNewConfig",
        "1 #Chg_Join Specialists",
        "2 #Chg_Join Mercenaries",
        "2 #Buy_Buy",
        "3 #Buy_RemoveItem",
        "4 #Buy_ResetConfig",
        "5 #Buy_SaveConfig",
        "6 #Buy_Akimbo",
        "6 #Buy_FullLoad",
        "6 #Buy_LoadConfig",
        "6 #Buy_PlayerAccessory",
        "7 #Buy_Random",
        "#Buy_ConfigEmpty",
        "#Buy_ConfigLoaded",
        "#Buy_ConfigReset",
        "#Buy_Credits",
        "#Buy_Fla",
        "#Buy_FreeSlots",
        "#Buy_Handguns",
        "#Buy_Las",
        "#Buy_LoadEquipment",
        "#Buy_NoCash",
        "#Buy_Removed",
        "#Buy_Rifles",
        "#Buy_SavedConfig",
        "#Buy_SaveEquipment",
        "#Buy_Sco",
        "#Buy_Shotguns",
        "#Buy_Sil",
        "#Buy_SlotAvailable",
        "#Buy_SlotEmpty",
        "#Buy_Slots",
        "#Buy_SMGs",
        "#Buy_SpecialPurpose",
        "#Buy_Total",
        "#Buy_Total_CashBased",
        "#Buy_UsingLastConfig",
        "#Buy_UsingNewConfig",
        "#Chg_ChangeTeam",
        "2 #HUD_Silencer  900",
        "3 #HUD_Lasersight  800",
        "4 #HUD_Flashlight  500",
        "5 #HUD_Scope  1800",
    ];
    for msg in used {
        let s = t(msg);
        assert!(!s.contains("#Buy_") && !s.contains("#Chg_") && !s.contains("#HUD_"), "{msg}: {s}");
    }
    // 2.1's own words (0x1026ac08, 0x1026b0e8) where 3.0's titles differ.
    assert_eq!(t("7 #Buy_Random"), "7 Buy Random");
    assert_eq!(t("#Buy_Total_CashBased"), "Remain Cash : %i (Total: %i)");
    assert_eq!(t("1 #Chg_Join Specialists"), "1 Join Specialists");
    assert_eq!(t("#Buy_SlotEmpty"), "Slot #%i (Empty)");
    assert_eq!(t("2 #HUD_Silencer  900"), "2 Silencer  900");
    // TS 3.0 localises through the titles.
    assert_eq!(buy::text(&v30().buy_menu(), &NoTitles, "7 #Buy_Random"), "7 #Buy_Random");
}

/// Issue #111: the items page red by money only, the category mouse rows by `m_Teamplay`, and
/// the hole message; TS 3.0's are unchanged.
#[test]
fn buy_menu_look() {
    let (m21, m30) = (v21().buy_menu(), v30().buy_menu());
    assert!(m21.items_red_by_cash_only && m21.category_rows_by_teamplay);
    assert_eq!(m21.hole_message, Some("A hole in the menu! TELL VINO IMMEDIATELY!"));
    assert!(!m30.items_red_by_cash_only && !m30.category_rows_by_teamplay && m30.hole_message.is_none() && m30.text.is_none());
}

/// Issue #111: `WeaponInfo` for kung fu sets 2.1's bar from its clip byte, clamped at 100
/// (0x1008d35e..0x1008d3ca); 3.0 leaves it to `KFuPower`.
#[test]
fn kungfu_bar_from_weapon_info() {
    assert_eq!(v21().kungfu_power_from_weapon_info(55), Some(55.0));
    assert_eq!(v21().kungfu_power_from_weapon_info(0), Some(0.0));
    assert_eq!(v21().kungfu_power_from_weapon_info(200), Some(100.0));
    assert_eq!(v30().kungfu_power_from_weapon_info(55), None);
}

/// Issue #178: 2.1's `WeaponInfo` for kung fu zeroes the slot menu's selection (0x1008d361,
/// 0x1008d36e); 3.0's leaves it.
#[test]
fn kungfu_weapon_info_resets_menu_selection() {
    assert!(v21().kungfu_weapon_info_resets_menu_selection());
    assert!(!v30().kungfu_weapon_info_resets_menu_selection());
}

/// Issue #176: 2.1's weapon panel (0x1008a320) on a 640 x 480 screen, HUD colour (255, 160, 0):
/// an M4A1-style gun with the silencer and an active laser, an empty reserve, semi-auto.
#[test]
fn weapon_panel_look() {
    use ts_client::hud::draw::HudOp::{Fill, Label, Number, Sprite, Text};
    let hud = [255, 160, 0];
    let p = WeaponPanel {
        w: 640,
        h: 480,
        hud,
        owned: 0b11,
        active: 0b10,
        name: "M4A1",
        ammo: 2,
        stack: false,
        ammo_name: "5.56 Nato",
        clip: 30,
        reserve: 0,
        firemode: 1,
        fov: 90,
    };
    let half = [127, 80, 0];
    let soft = [170, 106, 0]; // ftol(c / 1.5)
    let item = ts_client::hud::draw::HudSprite::ItemInstalled;
    let n = ts_client::hud::draw::num::DIGITS_3 | ts_client::hud::draw::num::DRAW_ZERO;
    let ops = v21().weapon_panel(&p).unwrap();
    assert_eq!(
        ops,
        [
            Fill { x: 504, y: 452, w: 128, h: 19, rgb: hud, a: 15 },
            Text { x: 506, y: 452, text: "Silencer", rgb: half },
            Sprite { sprite: item, x: 616, y: 452, rgb: half },
            Fill { x: 504, y: 432, w: 128, h: 19, rgb: hud, a: 45 },
            Text { x: 506, y: 432, text: "Lasersight", rgb: half },
            Sprite { sprite: item, x: 616, y: 432, rgb: hud },
            Fill { x: 504, y: 412, w: 128, h: 19, rgb: hud, a: 15 },
            Number { x: 506, y: 412, flags: n, n: 30, rgb: soft },
            Text { x: 568, y: 412, text: "/", rgb: [63, 40, 0] },
            Number { x: 598, y: 412, flags: n, n: 0, rgb: [195, 45, 45] },
            Fill { x: 504, y: 392, w: 128, h: 19, rgb: hud, a: 15 },
            Text { x: 506, y: 392, text: "Semi-auto", rgb: soft },
            Fill { x: 504, y: 372, w: 128, h: 19, rgb: hud, a: 15 },
            Label { x: 506, y: 372, text: "5.56 Nato".into(), rgb: soft },
            Fill { x: 504, y: 352, w: 128, h: 19, rgb: hud, a: 0x23 },
            Label { x: 506, y: 352, text: "M4A1".into(), rgb: soft },
        ]
    );
    // No ammo type: the stack count alone (at the reserve's place, same row), no kung-fu bar;
    // scoped, the zoom factor.
    let knives = WeaponPanel { owned: 0, ammo: 0, stack: true, clip: 5, fov: 20, name: "Knife", ..p.clone() };
    assert_eq!(
        v21().weapon_panel(&knives).unwrap(),
        [
            Fill { x: 504, y: 452, w: 128, h: 19, rgb: hud, a: 15 },
            Number { x: 598, y: 452, flags: n, n: 5, rgb: soft },
            Fill { x: 504, y: 432, w: 128, h: 19, rgb: hud, a: 0x23 },
            Label { x: 506, y: 432, text: "Knife".into(), rgb: soft },
            Label { x: 24, y: 24, text: "4x".into(), rgb: hud },
        ]
    );
    let melee = WeaponPanel { stack: false, ..knives.clone() };
    assert_eq!(v21().weapon_panel(&melee).unwrap().len(), 4, "no count without the stack flag");
    assert_eq!(firemode_name(0), "Full-auto");
    assert_eq!(firemode_name(6), "Full-auto");
    assert!(v30().weapon_panel(&p).is_none(), "3.0 draws its own DrawWeaponInfo");
}

/// Issue #111: 2.1's kung-fu bar (0x1008a0f0) on a 640 x 480 screen.
#[test]
fn kungfu_bar_look() {
    use ts_client::hud::draw::HudOp::{Fill, Text};
    let hud = [255, 160, 0];
    let ops = v21().kungfu_label(50.0, hud, 640, 480).unwrap();
    // ftol(1.28f x 50) = 63 (1.2799999713897705 x 50 = 63.99...).
    assert_eq!(
        ops,
        [
            Fill { x: 504, y: 452, w: 63, h: 19, rgb: hud, a: 0x40 },
            Fill { x: 567, y: 452, w: 65, h: 19, rgb: hud, a: 0x1e },
            Fill { x: 504, y: 432, w: 128, h: 19, rgb: hud, a: 0x23 },
            Text { x: 506, y: 432, text: "Kung-Fu", rgb: [170, 106, 0] },
        ]
    );
    // A full bar has no empty part.
    let ops = v21().kungfu_label(100.0, hud, 640, 480).unwrap();
    assert_eq!(ops[0], Fill { x: 504, y: 452, w: 127, h: 19, rgb: hud, a: 0x40 });
    assert_eq!(ops.len(), 4, "127 < 128: the 1-pixel rest is still drawn");
    assert!(v30().kungfu_label(50.0, hud, 640, 480).is_none(), "3.0 draws its own");
}

/// Issue #111: 2.1's status panel look (0x10085d90); 3.0's is the default.
#[test]
fn status_panel_look() {
    use ts_client::hud::draw::StatusLook;
    let l = v21().status_look();
    assert!(!l.stance_health_fill && l.armor_number && l.icons_divide);
    assert_eq!((l.team_bar_scale, l.labels), (Some(200), Some(("free slots", "Credits"))));
    assert_eq!(v30().status_look(), StatusLook::default());
    let d = StatusLook::default();
    assert!(d.stance_health_fill && !d.armor_number && !d.icons_divide && d.team_bar_scale.is_none() && d.labels.is_none());
}

/// The lasers' look (issue #204): 2.1's flickering `laserbeam.spr`, the red dot faded out by
/// 4096 units, other players' white dots (no beam) and flashlights from `origin + 24`, and the
/// unfaded TransTexture shadows; 3.0 keeps all of its own.
#[test]
fn laser_style() {
    use ts_client::effects::LaserStyle;
    assert_eq!(
        *Ts21Client.laser_style(),
        LaserStyle {
            animated_beam: false,
            dot_range: Some(4096.0),
            others_from_origin: true,
            other_beams: false,
            shadow_fade: false,
            sprite_tint: [1.0, 0.0, 0.0]
        }
    );
    assert_eq!(
        *ts30_client::Ts30Client.laser_style(),
        LaserStyle {
            animated_beam: true,
            dot_range: None,
            others_from_origin: false,
            other_beams: true,
            shadow_fade: true,
            sprite_tint: [1.0; 3]
        }
    );
}

#[test]
fn client_spectator_in_eye_pitch() {
    // #205: 2.1's client `PM_SpectatorMove` (FUN_1005c820, `OBS_IN_EYE`): pitch × -2.0, yaw and
    // roll copied. 3.0's (0x1002c790) copies all three.
    let a = [-4.5, 30.0, 2.0];
    assert_eq!(ts_client::spectator::in_eye_angles(&ts21_gameplay::TS21, a), [9.0, 30.0, 2.0]);
    assert_eq!(ts_client::spectator::in_eye_angles(&ts30_gameplay::TS30, a), a);
}

#[test]
fn weapon_slot_menu() {
    use ts_client::gameplay::SlotMenu;
    use ts_client::hud::state::{Hud, Slot};
    register();
    // #207: 2.1's list draws only while open, without clip text or bars (0x10089d90,
    // 0x1008af40); its SelectNextPrev reloads every press and sends ids > -1 (0x1008cc40).
    let s21 = SlotMenu { closed_list: false, clip_info: false, next_prev_reloads: true, next_prev_sends_kungfu: true };
    assert_eq!(v21().slot_menu(), s21);
    assert_eq!(v30().slot_menu(), SlotMenu { closed_list: true, clip_info: true, next_prev_reloads: false, next_prev_sends_kungfu: false });
    let mut h = Hud::default();
    assert_eq!(v30().select_slot(&mut h, 2, false), None, "3.0: the core's SelectSlot");

    let sel = |h: &mut Hud, n, fast| v21().select_slot(h, n, fast).expect("2.1 has its own SelectSlot");
    h.grid = [[ts_client::hud::state::EMPTY; ts_client::hud::state::GRID_BUCKETS]; ts_client::hud::state::GRID_ROWS];
    h.grid[0][1] = Slot { id: 0, clip: 0 }; // kung fu
    h.grid[2][1] = Slot { id: 34, clip: 1 };
    h.grid[4][1] = Slot { id: 25, clip: 1 };
    h.grid[1][2] = Slot { id: 1, clip: 5 };

    // Slot 10 (0x1008c6f0): weapon_0 from a closed menu, and (1, 1) every time.
    assert_eq!(sel(&mut h, 10, false).as_deref(), Some("weapon_0"));
    assert_eq!((h.menu_open, h.menu_row, h.menu_bucket), (false, 1, 1));
    h.menu_open = true;
    h.menu_row = 3;
    assert_eq!(sel(&mut h, 10, false), None, "an open menu just closes");
    assert_eq!((h.menu_row, h.menu_bucket), (1, 1));
    // Slot 1 after 10 is the same bucket: the walk starts at row 2.
    assert_eq!(sel(&mut h, 1, false), None);
    assert_eq!((h.menu_open, h.menu_row, h.menu_bucket), (true, 2, 1));
    assert_eq!(sel(&mut h, 1, false), None);
    assert_eq!(h.menu_row, 4);
    assert_eq!(sel(&mut h, 1, false), None);
    assert_eq!(h.menu_row, 2, "wraps to row 1 and on to the knife; row 0 (kung fu) never");
    // A new bucket searches from row 1; an empty one leaves -1.
    assert_eq!(sel(&mut h, 2, false), None);
    assert_eq!((h.menu_row, h.menu_bucket), (1, 2));
    assert_eq!(sel(&mut h, 3, false), None);
    assert_eq!((h.menu_row, h.menu_bucket), (-1, 3));
    // The same empty bucket again: past the grid, 17.
    assert_eq!(sel(&mut h, 3, false), None);
    assert_eq!(h.menu_row, 17);

    // Fast switch to bucket 1 from another bucket starts at row 0: kung fu.
    h.menu_bucket = 2;
    h.menu_row = 1;
    assert_eq!(sel(&mut h, 1, true).as_deref(), Some("weapon_0"));
    assert_eq!((h.menu_open, h.menu_row, h.menu_bucket), (false, 0, 2), "the bucket isn't changed");
    // To bucket 2 (not 1): row + 1 = 1, the Glock.
    h.menu_row = 0;
    assert_eq!(sel(&mut h, 2, true).as_deref(), Some("weapon_1"));
    // An empty bucket ends where it started; nothing sent, even from past the grid.
    for row in [3, 17] {
        h.menu_row = row;
        assert_eq!(sel(&mut h, 5, true), None);
    }
    assert_eq!(sel(&mut h, 7, false), None, "slots 6..9 do nothing");
}

/// `Hud::default` and the HUD handlers ask the HUD's rules: the client library's registry, 3.0
/// first, as `client-dll` registers them.
fn register() {
    ts_client::gameplay::register(ts30_gameplay::ID, &ts30_client::PLUGIN);
    ts_client::gameplay::register(ts21_gameplay::ID, &PLUGIN);
}

struct Titles;
impl ts_client::hud::text::Titles for Titles {
    fn get(&self, key: &str) -> Option<String> {
        (key == "HUD_Credits").then(|| "Credits".to_owned())
    }
}

/// A HUD on `ruleset` with the 640 × 480 test screen (8 pixels a character) and the local player
/// "me".
fn hud_on(ruleset: &str) -> ts_client::hud::state::Hud {
    register();
    ts_client::hud::state::Hud { gameplay: ruleset.parse().ok(), ..Default::default() }
}

#[test]
fn spectator_third_person() {
    // #230 §1: 2.1's `Spectator` (0x100b28c0) leaves third person entering or leaving observer
    // mode; it has no cl_thirdperson. 3.0 (0x10065250) goes back to third person on a respawn
    // with cl_thirdperson 1.
    for observer in [false, true] {
        for cl in [0.0, 1.0, 2.0] {
            assert!(v21().spectator_toggles_third_person(observer, true, cl));
            assert!(!v21().spectator_toggles_third_person(observer, false, cl));
        }
    }
    assert!(v30().spectator_toggles_third_person(false, false, 1.0), "3.0: back to third person");
    assert!(!v30().spectator_toggles_third_person(false, true, 1.0));
    assert!(v30().spectator_toggles_third_person(false, true, 0.0));
    assert!(!v30().spectator_toggles_third_person(false, true, 2.0), "3.0: other values keep it");
    assert!(v30().spectator_toggles_third_person(true, true, 1.0));
}

#[test]
fn ts_message_layout() {
    use shared::protocol::Msg;
    use shared::protocol::hud::TsMessage;
    // #230 §2: 2.1's AddTSMessage (0x100873d0): no LookupString or grey, 63 characters, bottom
    // at h - 0x20, 6 pixels a character, (float)t / 10.0 + time.
    let screen = goldsrc::ext::hud::Screen::default();
    let ctx = ts_client::hud::state::Ctx {
        now: 10.0,
        screen: &screen,
        titles: &Titles,
        local_index: 1,
        local_name: "me".into(),
        third_person: false,
        thirdperson_switch: false,
        cl_thirdperson: 0.0,
        saytext_time: 5.0,
        deathnotice_time: 5.0,
        key_binding: &ts_client::hud::state::no_key_binding,
    };
    let send = |h: &mut ts_client::hud::state::Hud, flags: u8, time: u8, text: &str| {
        let m = TsMessage { r: 1, g: 2, b: 3, flags, time, text: text.into() };
        assert!(h.apply(Msg::TsMessage, &m.encode(), &ctx, &mut Vec::new()));
        h.messages.iter().find(|l| l.expire > ctx.now && l.flags == flags).cloned().unwrap()
    };
    let mut h = hud_on(ts21_gameplay::ID);
    let l = send(&mut h, 8, 30, "#HUD_Credits for me");
    assert_eq!(l.text, "#HUD_Credits for me", "no LookupString");
    assert_eq!(l.rgb, [1, 2, 3], "no grey for the local name");
    assert_eq!(l.y, 480 - 0x20);
    assert_eq!(l.x, 320 + (19 >> 1) * -6);
    assert_eq!(l.expire, (30f32 as f64 / 10.0 + 10.0) as f32);
    let l = send(&mut h, 4, 30, "abcde");
    assert_eq!(l.x, 640 - 5 * 6 - 32);
    let long = "x".repeat(100);
    assert_eq!(send(&mut h, 1, 30, &long).text.len(), 63);

    let mut h = hud_on(ts30_gameplay::ID);
    let l = send(&mut h, 8, 30, "#HUD_Credits for me");
    assert_eq!((l.text.as_str(), l.rgb, l.y), ("Credits for me", [155; 3], 480 - 64));
    assert_eq!(l.x, 320 - 14 * 8 / 2, "3.0: the drawn width");
    assert_eq!(send(&mut h, 1, 30, &long).text.len(), 100);
}

#[test]
fn powerup_labels_and_award_titles() {
    // #230 §3: DrawPowerup (0x10085c00) writes the names out.
    let names = [(1, "Slow-Motion"), (2, "Infinite-Ammo"), (8, "Slow-Pause"), (0x10, "Double-FireRate")];
    for (bit, name) in names {
        assert_eq!(v21().powerup_label(bit), Some(name));
        assert_eq!(v30().powerup_label(bit), None, "3.0: titles.txt");
    }
    // #230 §4: the scoreboard's status column (0x100a4670).
    assert_eq!(v21().award_title(2), Some("KILLER"));
    assert_eq!(v21().award_title(3), Some("DEMOLITION"));
    for a in [0, 1, 4, 5, 10, 11] {
        assert_eq!(v21().award_title(a), None, "award {a} is 3.0's");
    }
}

#[test]
fn go_slow_sound() {
    // #230 comment: 2.1's TSSlowMo (0x100881e0) tests the previous raw factor; 3.0 (0x1004bef0)
    // the eased scale. A new slow motion while easing back to 1 after a `TSSlowMo 0`:
    assert!(v21().go_slow_sound(0.6, 0.5, 0.0, 50.0));
    assert!(!v30().go_slow_sound(0.6, 0.5, 0.0, 50.0));
    assert!(!v21().go_slow_sound(1.0, 0.25, 50.0, 25.0), "2.1: already slowed");
    assert!(v30().go_slow_sound(1.0, 0.25, 50.0, 25.0));
    assert!(!v21().go_slow_sound(1.0, 1.0, 0.0, 0.0));
}

#[test]
fn third_person_camera_offsets() {
    use ts_client::view::{CamInput, CamOffsets};
    // #240 §1/§2: 2.1 (0x100b7700..0x100b7ab9) has no cl_camtype; the SDK cvars' defaults
    // (90, 0, 12) give (-90, 0, 12) whatever cl_camtype says. 3.0's cl_camtype 0: (-48, 16, 12).
    let c = CamInput { camtype: 0, cam_back: 90.0, cam_right: 0.0, cam_up: 12.0, ducked: false, alive: true, stunt: false, ft: 0.25 };
    let mut o = CamOffsets::default();
    assert_eq!(v21().camera_offsets(&mut o, &c), (-90.0, 0.0, 12.0));
    assert_eq!(v21().camera_offsets(&mut o, &CamInput { camtype: 1, ..c }), (-90.0, 0.0, 12.0));
    assert_eq!(v30().camera_offsets(&mut CamOffsets::default(), &c), (-48.0, 16.0, 12.0));
    let wild = CamInput { cam_back: 500.0, cam_right: -50.0, cam_up: 50.0, ..c };
    assert_eq!(v21().camera_offsets(&mut CamOffsets::default(), &wild), (-128.0, -24.0, 24.0));
    assert_eq!(v21().camera_offsets(&mut CamOffsets::default(), &CamInput { cam_back: -50.0, ..c }).0, 24.0);

    // Integer steps of ft × 100 (25 here), set to the target once at or past it.
    let mut o = CamOffsets::default();
    let ducked = CamInput { ducked: true, ..c };
    assert_eq!(v21().camera_offsets(&mut o, &ducked).2, 12.0 + 25.0, "one step past 32 - 12 = 20");
    assert_eq!(v21().camera_offsets(&mut o, &ducked).2, 32.0);
    assert_eq!(v21().camera_offsets(&mut o, &c).2, 12.0 + 20.0 - 25.0, "back down by a step");
    assert_eq!(v21().camera_offsets(&mut o, &c).2, 12.0, "0 at once from 0 or below");
    let dead = CamInput { alive: false, ..ducked };
    let backs: Vec<f32> = (0..5).map(|_| v21().camera_offsets(&mut o, &dead).0).collect();
    assert_eq!(backs, [-115.0, -140.0, -165.0, -190.0, -180.0], "toward -180 - back = -90");
    assert_eq!(v21().camera_offsets(&mut o, &dead).2, 12.0, "dead: no up offset");
    let mut o = CamOffsets::default();
    let stunt = CamInput { stunt: true, ..c };
    assert_eq!(v21().camera_offsets(&mut o, &stunt).0, -90.0 - 25.0);
    assert_eq!(v21().camera_offsets(&mut o, &stunt).0, -92.0, "the target -2 is above: taken at once");
    assert_eq!(v21().camera_offsets(&mut o, &c).0, -90.0 + 23.0, "rises past 0 by a step");
    assert_eq!(v21().camera_offsets(&mut o, &c).0, -90.0, "then 0 at once");
    // Above 100 fps, ftol(ft × 100) is 0: the offsets don't move.
    let mut o = CamOffsets::default();
    assert_eq!(v21().camera_offsets(&mut o, &CamInput { ft: 0.01, ..ducked }).2, 12.0);
    assert_eq!(o, CamOffsets::default());
}

#[test]
fn view_roll() {
    use goldsrc::sys::ref_params_t;
    use ts_client::view::RollEntity;
    // #240 §3: 2.1's V_CalcViewRoll (0x100b6910) adds V_CalcRoll(angles, simvel, 0.5 or 30, 10)
    // at once, on the ground too; observer and dive bits are the local player's.
    let ent = RollEntity::default();
    let mut p = ref_params_t { frametime: 0.01, onground: 1, health: 100, simvel: [0.0, -300.0, 0.0], ..Default::default() };
    let mut roll = 0.0;
    v21().view_roll(&mut roll, &mut p, Some(ent), Some(ent));
    assert_eq!(p.viewangles[2], 0.15, "|side| 300: 3 × 0.5 / 10, on the ground");
    v30().view_roll(&mut roll, &mut p, Some(ent), Some(ent));
    assert_eq!(p.viewangles[2], 0.15, "3.0: no ground roll");
    p.viewangles[2] = 0.0;
    let dive = RollEntity { iuser4: shared::ts::stunt::DIVING, ..ent };
    v21().view_roll(&mut roll, &mut p, Some(ent), Some(dive));
    assert_eq!(p.viewangles[2], 9.0, "a dive: 3 × 30 / 10, at once");
    p.viewangles[2] = 0.0;
    v21().view_roll(&mut roll, &mut p, Some(dive), Some(ent));
    assert_eq!(p.viewangles[2], 0.15, "the view entity's stunt bits aren't read");
    p.simvel = [0.0, 300.0, 0.0];
    p.viewangles[2] = 0.0;
    v21().view_roll(&mut roll, &mut p, Some(ent), Some(ent));
    assert_eq!(p.viewangles[2], -0.15);
    p.viewangles[2] = 3.0;
    v21().view_roll(&mut roll, &mut p, Some(ent), Some(RollEntity { iuser1: 1, ..ent }));
    assert_eq!(p.viewangles[2], 0.0, "observer");
    (p.health, p.viewheight) = (0, [0.0, 0.0, 8.0]);
    v21().view_roll(&mut roll, &mut p, Some(ent), Some(ent));
    assert_eq!(p.viewangles[2], 80.0);
    p.viewangles[2] = 3.0;
    v21().view_roll(&mut roll, &mut p, None, Some(ent));
    assert_eq!(p.viewangles[2], 3.0, "no view entity");
    assert_eq!(roll, 0.0, "2.1 has no eased roll");

    // #240 §4: no local translucency.
    assert!(!v21().local_translucency() && v30().local_translucency());
}

// Audit round 3: hud

/// #260: 2.1's `SayTextPrint` (0x10060890) has no mute check and its `Draw` (0x10060540) sets no
/// colour for a plain line; 3.0's (0x1002ed00, 0x1002eb40) mutes and sets (1, 0.7, 0).
#[test]
fn say_text_style() {
    use ts_client::hud::saytext::{SAY_COLOR, SayTextStyle};
    assert_eq!(v21().say_text_style(), SayTextStyle { mute_blocked: false, plain_color: None });
    assert_eq!(v30().say_text_style(), SayTextStyle { mute_blocked: true, plain_color: Some(SAY_COLOR) });
    assert_eq!(SAY_COLOR, [1.0, 0.7, 0.0], "g_ColorYellow 0x1009359c (2.1's 0x10289f9c is the same)");
}

/// #261: 2.1's particles run the core's `Decelerate` update (0x10091020); 3.0's the `Drag` one
/// (0x100520d0).
#[test]
fn particle_model() {
    use ts_client::effects::psys::ParticleModel;
    assert_eq!(v21().particle_model(), ParticleModel::Decelerate);
    assert_eq!(v30().particle_model(), ParticleModel::Drag);
}

// Audit round 3: gui

/// Issue #262: 2.1's summary (0x1006b9a0) writes Kevlar and Stealth Shoes in the halved HUD
/// colour and never red; 3.0's (0x10036af0) in the full colour, red when restricted.
#[test]
fn buy_summary_item_colour() {
    let hud = [255, 160, 0];
    let r = rules(v21(), 0);
    assert_eq!(buy::summary_item_rgb(ITEM_KEVLAR, &r, hud), [127, 80, 0]);
    assert_eq!(buy::summary_item_rgb(ITEM_STEALTH, &r, hud), [127, 80, 0], "never red, whatever the mode");
    let r30 = rules(v30(), 0);
    assert_eq!(buy::summary_item_rgb(ITEM_KEVLAR, &r30, hud), hud);
    assert_eq!(buy::summary_item_rgb(ITEM_STEALTH, &r30, hud), [195, 45, 45], "restricted outside team modes");
    assert_eq!(buy::summary_item_rgb(ITEM_STEALTH, &rules(v30(), 2), hud), hud);
}

/// Issue #263: 3.0's `Open` (0x10036670) returns during demo playback; 2.1's (0x1006b3f0)
/// doesn't check.
#[test]
fn buy_menu_opens_in_demos() {
    assert!(buy::opens(&v21().buy_menu(), true));
    assert!(!buy::opens(&v30().buy_menu(), true));
    assert!(buy::opens(&v30().buy_menu(), false) && buy::opens(&v21().buy_menu(), false));
}

/// Issue #266: 2.1's VGUI code divides (`ScreenHeight / 480.0f * n`, e.g. 0x100a0ea1) with
/// nothing spilled; 3.0's multiplies by the float reciprocals (0x1007d43c, 0x1007d438).
#[test]
fn vgui_scale() {
    use ts_client::gameplay::DllBuild;
    use ts_client::hud::vgui;
    let at = |s| vgui::with_scale(s, || (vgui::xs(640), vgui::ys(480), vgui::spill(1.0 + 1e-12)));
    assert_eq!(at(DllBuild::Debug), (1.0, 1.0, 1.0 + 1e-12));
    let (x, y, sp) = at(DllBuild::Optimised);
    assert!(x > 1.0 && y > 1.0 && sp == 1.0, "3.0's reciprocals sit a hair above; its spills round to float");
    // MOTD scroll panel height at 640×480: 312 − (48 + 60) exactly in 2.1, a hair under in 3.0.
    let h = |s| {
        vgui::with_scale(s, || vgui::ftol(vgui::ftol(vgui::ys(480) * 312.0) as f64 - (vgui::ys(480) * 48.0 + vgui::ys(480) * 30.0 * 2.0)))
    };
    assert_eq!((h(DllBuild::Debug), h(DllBuild::Optimised)), (204, 203));
}

/// Issue #281: 2.1's `client.dll` imports nothing from `opengl32.dll` (3.0: `V_SetFog`
/// 0x10056c20, `glDisable(GL_FOG)` 0x10056e5e).
#[test]
fn gl_fog() {
    assert!(!v21().gl_fog() && v30().gl_fog());
}

/// 2.1's `client.dll` is an unoptimised build that divides and stores every float assignment
/// (`MessageScanNextChar` 0x10054e3b, `DrawTSMessages` 0x1008700d; the VGUI `/ 480.0f`
/// 0x100a0ea1, #266; the renderer's `fdiv QWORD 255.0` 0x10064e46 and `/0.1` 0x10064575, #276;
/// the overview's `(90 − pitch) / 90` 0x1004c475, `Length` 0x10097fc0, `AngleMatrix`
/// 0x10061ee0, #271, #272); 3.0's is optimised: reciprocals on the x87 (0x10079e68, `×10` at
/// 0x10030c76, 0x1007d43c, 0x10023c90, 0x10024160).
#[test]
fn dll_build() {
    use ts_client::gameplay::DllBuild;
    assert_eq!((v21().dll_build(), v30().dll_build()), (DllBuild::Debug, DllBuild::Optimised));
}
