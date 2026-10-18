use super::*;
use shared::weapons::gun::{MeleeHit, MeleeResult, Shot, Sound};
use shared::weapons::spread::Vec3;
use shared::weapons::{Status, att, mode_mask, table};
use ts30_gameplay::TS30 as CORE;

/// The gun code reads the active tables: these tests run with 2.1's (and none sets another).
fn setup() {
    table::set_ruleset_tables(tables::tables());
}

#[derive(Default)]
struct Mock {
    time: f32,
    fuser1: f32,
    powerups: u16,
    buttons: u32,
    pressed: u32,
    velocity: Vec3,
    seed: u32,
    client: bool,
    repredict: bool,
    events: Vec<(Event, EventArgs)>,
    shots: Vec<Shot>,
    kicks: Vec<(f32, f64, bool)>,
    /// Run TS 3.0's gun rules instead of 2.1's (on 2.1's tables).
    core: bool,
    melees: u32,
    /// The damage of each melee hit asked for.
    melee_damage: Vec<f32>,
    /// `melee_attack_pre`: the (time, damage) it was asked with, and its answer (`None` lets the
    /// move through unchanged; else the new time and damage, and whether it goes ahead).
    melee_pre: Vec<(f32, f32)>,
    melee_answer: Option<(f32, f32, bool)>,
    /// `melee_hold` bits and `block_player` times.
    holds: Vec<i32>,
    blocks: Vec<f32>,
    blades: u32,
    anims: u32,
    last_anim: (i32, bool, i32),
    /// block.wav plays.
    block_sounds: u32,
    /// The last `set_view_model` and `set_fov` (`None`: never called).
    view_model: Option<Option<u8>>,
    fov: Option<f32>,
}

impl GunHost for Mock {
    fn time(&self) -> f32 {
        self.time
    }
    fn slow_mul(&self) -> f32 {
        shared::weapons::spread::fixed_slow_amt(self.fuser1)
    }
    fn time_scale(&self) -> f32 {
        self.fuser1
    }
    fn prediction(&self) -> bool {
        self.client
    }
    fn first_prediction(&self) -> bool {
        !self.repredict
    }
    fn arith(&self) -> Arith {
        Arith::CLIENT
    }
    fn rules(&self) -> &'static dyn GunRules {
        if self.core { &CORE } else { &crate::TS21 }
    }
    fn powerups(&self) -> u16 {
        self.powerups
    }
    fn kungfu(&self) -> bool {
        false
    }
    fn alive(&self) -> bool {
        true
    }
    fn buttons(&self) -> u32 {
        self.buttons
    }
    fn pressed(&self) -> u32 {
        self.pressed
    }
    fn origin(&self) -> Vec3 {
        [0.0; 3]
    }
    fn view_ofs(&self) -> Vec3 {
        [0.0; 3]
    }
    fn velocity(&self) -> Vec3 {
        self.velocity
    }
    fn v_angle(&self) -> Vec3 {
        [0.0; 3]
    }
    fn flags(&self) -> i32 {
        0x200
    }
    fn waterlevel(&self) -> i32 {
        0
    }
    fn iuser4(&self) -> i32 {
        0
    }
    fn set_iuser4(&mut self, _v: i32) {}
    fn seed(&self) -> u32 {
        self.seed
    }
    fn random_long(&mut self, lo: i32, _hi: i32) -> i32 {
        lo
    }
    fn fov(&self) -> f32 {
        0.0
    }
    fn set_fov(&mut self, fov: f32) {
        self.fov = Some(fov);
    }
    fn set_view_model(&mut self, id: Option<u8>) {
        self.view_model = Some(id);
    }
    fn view_model_hidden(&self) -> bool {
        false
    }
    fn set_weapon_model(&mut self, _model: Option<(u8, bool)>) {}
    fn event(&mut self, ev: Event, args: EventArgs) {
        self.events.push((ev, args));
    }
    fn weapon_anim(&mut self, anim: i32, skip_local: bool, body: i32) {
        self.anims += 1;
        self.last_anim = (anim, skip_local, body);
    }
    fn player_anim(&mut self, _anim: i32) {}
    fn kick(&mut self, pitch: f32, yaw: f64, shake: bool) {
        self.kicks.push((pitch, yaw, shake));
    }
    fn sound(&mut self, s: Sound) {
        if matches!(s, Sound::Block) {
            self.block_sounds += 1;
        }
    }
    fn shoot(&mut self, shot: &Shot, _gun: &mut Gun) {
        self.shots.push(*shot);
    }
    fn melee(&mut self, hit: &MeleeHit, _gun: &mut Gun) -> MeleeResult {
        self.melees += 1;
        self.melee_damage.push(hit.damage);
        MeleeResult::Miss
    }
    fn melee_attack_pre(&mut self, _gun: &mut Gun, time: &mut f32, damage: &mut f32) -> bool {
        self.melee_pre.push((*time, *damage));
        let Some((t, d, go)) = self.melee_answer else { return true };
        (*time, *damage) = (t, d);
        go
    }
    fn melee_hold(&mut self, bits: i32, _recover: f32, _crouch: bool) {
        self.holds.push(bits);
    }
    fn throw_grenade(&mut self, _origin: Vec3, _velocity: Vec3, _roll: bool) {}
    fn throw_blade(&mut self, _btype: i32) {
        self.blades += 1;
    }
    fn slots_changed(&mut self, _delta_free: i32) {}
    fn melee_bar_changed(&mut self, _bar: f32) {}
    fn block_player(&mut self, t: f32) {
        self.blocks.push(t);
    }
}

/// A gun holding weapon `id` (full clip, default mode) and the host, fuser1 1.0 (normal).
fn armed(id: u8) -> (Gun, Mock) {
    setup();
    let mut g = Gun::default();
    let w = info(id).unwrap();
    let mut s = Status { owned: true, ammo: w.ammo, mode: shared::weapons::default_mode(mode_mask(w)), clip: w.clip, ..Default::default() };
    if w.kind == shared::weapons::WeaponKind::AkimboOnly || w.kind == shared::weapons::WeaponKind::Akimbo {
        s.left = w.clip / 2;
        s.right = w.clip - s.left;
    }
    g.status[id as usize] = s;
    g.current = id;
    (g, Mock { time: 10.0, fuser1: 1.0, ..Default::default() })
}

fn fire_once(g: &mut Gun, h: &mut Mock) {
    crate::TS21.fire(g, h);
}

/// `PrimaryAttack` (0x1004aa50) and `SecondaryAttack` (0x1004a800) slash the knife and swing
/// the katana only on a fresh press; 3.0 attacks while the button is held.
#[test]
fn blades_attack_on_a_fresh_press() {
    ts30_gameplay::tables::make_default();
    const IN_ATTACK: u32 = 1;
    const IN_ATTACK2: u32 = 0x800;
    for knife in [25, 35] {
        assert_eq!(kind_of(knife), kind::KNIFE);
        for (button, secondary) in [(IN_ATTACK, false), (IN_ATTACK2, true)] {
            let attack = |g: &mut Gun, h: &mut Mock| if secondary { g.secondary_attack(h) } else { g.primary_attack(h) };
            let (mut g, mut h) = armed(knife);
            h.buttons = button;
            attack(&mut g, &mut h);
            assert_eq!(g.idle, 0.0, "held: no slash ({knife}, {secondary})");
            h.pressed = button;
            attack(&mut g, &mut h);
            assert_eq!(g.idle, 3.0, "pressed: KnifeFire ({knife}, {secondary})");
            let (mut g, mut h) = armed(knife);
            h.buttons = button;
            h.core = true;
            attack(&mut g, &mut h);
            assert_ne!(g.idle, 0.0, "3.0 slashes while held");
        }
    }
    let (mut g, mut h) = armed(id::KATANA);
    h.buttons = IN_ATTACK;
    g.primary_attack(&mut h);
    assert_eq!(h.melees, 0, "held: no swing");
    h.pressed = IN_ATTACK;
    g.primary_attack(&mut h);
    assert_eq!(h.melees, 1, "pressed: KatanaFire");
}

/// 2.1's `SecondaryAttack` (0x1004a800) has no katana branch: no throw, only the generic
/// path's timers (the katana has no scope and no alt grip). 3.0 throws it.
#[test]
fn katana_attack2_does_not_throw() {
    ts30_gameplay::tables::make_default();
    const IN_ATTACK2: u32 = 0x800;
    let (mut g, mut h) = armed(id::KATANA);
    h.buttons = IN_ATTACK2;
    h.pressed = IN_ATTACK2;
    g.secondary_attack(&mut h);
    assert_eq!(h.blades, 0);
    assert!(g.status[id::KATANA as usize].owned && g.current == id::KATANA);
    assert_eq!((g.next_secondary, g.idle), (0.3, 1.0));
    assert!(!g.status[id::KATANA as usize].alt_grip);
    assert_eq!(h.anims, 0);
    assert_eq!(h.melees, 0);
}

/// 2.1's `WeaponIdle` (0x1004d660) reloads an empty gun under Infinite Clip; 3.0's doesn't.
#[test]
fn idle_reloads_under_infinite_clip() {
    ts30_gameplay::tables::make_default();
    for core in [false, true] {
        let (mut g, mut h) = armed(id::MP5K);
        let ammo = g.status[id::MP5K as usize].ammo as usize;
        g.status[id::MP5K as usize].clip = 0;
        g.reserve[ammo] = 60;
        h.powerups = shared::ts::powerup::INFINITE_CLIP;
        h.core = core;
        g.weapon_idle(&mut h);
        let reloaded = h.events.iter().any(|(e, _)| *e == Event::Reload);
        assert_eq!(reloaded, !core, "core {core}");
    }
}

/// 2.1's HolsterWeapon (`FUN_1004d7a0`) left no view model, but the 3.0 models animate kung fu
/// on `v_melee.mdl`, so both rulesets draw kung fu's view model with its draw animation and event
/// (a visual compatibility deviation).
#[test]
fn holster_draws_the_kungfu_view_model() {
    ts30_gameplay::tables::make_default();
    for core in [false, true] {
        let (mut g, mut h) = armed(id::MP5K);
        h.core = core;
        h.fov = Some(40.0);
        g.holster_weapon(&mut h);
        assert_eq!(g.current, 0);
        assert_eq!(h.fov, Some(0.0));
        assert_eq!(h.view_model, Some(Some(0)), "core {core}");
        assert!(h.events.iter().any(|(e, _)| *e == Event::Draw) && h.anims == 1, "core {core}");
    }
}

/// `GetBetterWeapon` (FUN_1004a410) draws the most expensive owned weapon, an akimbo pair at
/// twice its cost, the lowest id on a tie; 3.0 walks its priority list.
#[test]
fn better_weapon_is_the_most_expensive() {
    ts30_gameplay::tables::make_default();
    use crate::TS21;
    setup();
    let own = |ids: &[u8]| {
        let mut g = Gun::default();
        for &i in ids {
            g.status[i as usize].owned = true;
        }
        g
    };
    let cost = |i: u8| info(i).unwrap().cost;
    // Katana (2900) over the MP5SD (2500), where 3.0's list draws the MP5SD first.
    assert!(cost(34) > cost(6));
    let g = own(&[6, 34]);
    assert_eq!(TS21.better_weapon(&g, 0), 34);
    assert_eq!(CORE.better_weapon(&g, 0), 6);
    // A tie (AK47 and Benelli at 3000): the lowest id.
    assert_eq!(cost(4), cost(13));
    assert_eq!(TS21.better_weapon(&own(&[4, 13]), 0), 4);
    assert_eq!(TS21.better_weapon(&own(&[4, 13]), 4), 13);
    // The akimbo MK23s (700 x 2) beat a weapon costing between one and two of them.
    assert_eq!(kind_of(10), kind::AKIMBO);
    let mid = (1..38u8).find(|&i| info(i).is_some_and(|w| w.cost > cost(10) && w.cost < 2 * cost(10))).unwrap();
    assert_eq!(TS21.better_weapon(&own(&[mid, 10]), 0), 10);
    // Nothing worth more than 0: kung fu.
    assert_eq!(TS21.better_weapon(&own(&[]), 0), 0);
    assert_eq!(TS21.better_weapon(&own(&[6]), 6), 0);
}

#[test]
fn the_2_1_table_set() {
    ts30_gameplay::tables::make_default();
    setup();
    let t = tables::tables();
    assert_eq!(t.weapons().count(), 34, "33 weapons and kung fu");
    for gone in [id::BERETTA, id::CONTENDER, id::SKORPION_AKIMBO, 29] {
        assert!(t.weapon(gone).is_none(), "{gone}");
    }
    let tmp = t.weapon(17).unwrap();
    assert_eq!(
        (tmp.name.as_str(), tmp.ammo, tmp.clip, tmp.rate, tmp.reload, tmp.cost, tmp.slots),
        ("STEYR-TMP", 1, 30, 0.05, 1.8, 1600, 20)
    );
    assert_eq!(t.asset(17).unwrap().model, "tmp");
    assert_eq!(t.anims(17), &tables::TMP_SEQUENCES);
    assert_eq!(t.anims(13), ts30_gameplay::tables::tables().anims(13), "3.0's sequences for the 3.0 models");
    // Damage = the ammo's base damage x the weapon's multiplier.
    let dmg = |id: u8| t.weapon(id).unwrap().damage;
    assert_eq!(dmg(id::AK47), 52.5);
    assert_eq!(dmg(id::GLOCK18), 45.0 * 0.675f32);
    assert_eq!(dmg(id::BARRETT), 150.0);
    assert_eq!(dmg(id::RAGING_BULL), 120.0);
    assert_eq!(tables::ammo_damage(12), 160);
    assert_eq!(tables::damage_multiplier(id::MOSSBERG), 0.74);
    assert_eq!(t.ammo(12).unwrap().name, ".454 Casull");
    assert!(t.ammo(11).is_none() && t.ammo(13).is_none());
    assert_eq!(t.ammo(10).unwrap().pierce, 0.05);
    assert_eq!(t.ammo(2).unwrap().max_carry, [200, 48, 16, 0]);
    // The stock hit: a flag, 20 on the M4 and the Sawed-off only.
    let stocks: Vec<u8> = t.weapons().filter(|w| w.stock != 0).map(|w| w.id).collect();
    assert_eq!(stocks, vec![id::M4, id::SAWEDOFF]);
    assert_eq!(t.weapon(id::M4).unwrap().stock, 20);
    // Akimbo Berettas stand alone; the UMP has no burst.
    assert_eq!(t.weapon(8).unwrap().single, -1);
    assert_eq!(mode_mask(t.weapon(id::UMP).unwrap()), 3);
    assert_eq!(t.combat.silencer.spread, 1.2);
    assert_eq!(t.combat.headshot_shotgun, 2.0);
}

#[test]
fn delays_are_fixed_at_fire_time() {
    ts30_gameplay::tables::make_default();
    let delay = |powerups: u16, fuser1: f32, client: bool| {
        let (mut g, mut h) = armed(id::AK47);
        h.powerups = powerups;
        h.fuser1 = fuser1;
        h.client = client;
        fire_once(&mut g, &mut h);
        g.next_primary
    };
    assert_eq!(delay(0, 1.0, false), 0.08);
    assert_eq!(delay(0, 0.0, false), 0.08, "fuser1 0 is normal speed in 2.1");
    assert_eq!(delay(powerup::DOUBLE_FIRERATE, 1.0, false), 0.04);
    assert_eq!(delay(0, 0.5, false), (2.0f64 * 0.08f32 as f64) as f32);
    assert_eq!(delay(0, 0.05, false), (10.0f64 * 0.08f32 as f64) as f32, "the server floors fuser1 at 0.1");
    assert_eq!(delay(0, 0.05, true), (1.0 / 0.05f32 as f64 * 0.08f32 as f64) as f32, "the client doesn't");
    assert_eq!(delay(powerup::SLOWPAUSE, 0.5, false), (0.08f32 as f64 * 6.666_666_5f32 as f64) as f32);
    assert_eq!(delay(powerup::SLOWPAUSE, 0.5, true), (0.08f32 as f64 / 0.15) as f32);
}

#[test]
fn semi_waits_the_rate_and_shotgun_semi_is_0_1() {
    ts30_gameplay::tables::make_default();
    let (mut g, mut h) = armed(id::M4);
    g.status[id::M4 as usize].mode = shared::weapons::mode::SEMI;
    h.powerups = powerup::DOUBLE_FIRERATE;
    fire_once(&mut g, &mut h);
    assert_eq!(g.next_primary, 0.07, "semi: the rate, no 3.0 0.1, no double fire rate");

    let (mut g, mut h) = armed(id::BENELLI);
    g.status[id::BENELLI as usize].mode = shared::weapons::mode::SEMI;
    fire_once(&mut g, &mut h);
    assert_eq!(g.next_primary, 0.1);
    assert_eq!(h.shots.len(), 1);
    // Kick x1.3 in semi: the server applies the kick itself as the pitch.
    assert_eq!(h.kicks[0].0, 1.3f32 * 11.5);
    assert!(g.status[id::BENELLI as usize].toggle, "the pump toggles after every shot");
}

#[test]
fn silencer_widens_the_spread_and_cuts_damage() {
    ts30_gameplay::tables::make_default();
    let (mut g, mut h) = armed(id::MP5K);
    g.status[id::MP5K as usize].items = att::SILENCER;
    fire_once(&mut g, &mut h);
    let shot = h.shots[0];
    // ScaleSpread adds the (empty) penalty: the spread is accuracy x 1.2.
    assert_eq!(shot.spread, (0.029f32 as f64 * 1.2) as f32);
    assert_eq!(shot.damage, 45.0 * (0.7f32 as f64 * 0.8) as f32);
}

#[test]
fn moving_barrett_and_m60_set_the_penalty() {
    ts30_gameplay::tables::make_default();
    for (wid, speed, want) in [(id::BARRETT, 81.0, 0.1), (id::BARRETT, 79.0, 0.0), (id::M60, 100.0, 0.1), (id::AK47, 300.0, 0.0)] {
        let (mut g, mut h) = armed(wid);
        h.velocity = [speed, 0.0, 0.0];
        h.repredict = true; // keep ScaleSpread from growing the penalty further
        fire_once(&mut g, &mut h);
        assert_eq!(g.spread_penalty, want, "weapon {wid} at {speed}");
    }
}

#[test]
fn scale_spread_grows_by_elapsed_over_t() {
    ts30_gameplay::tables::make_default();
    let (mut g, mut h) = armed(id::AK47);
    g.last_shot = 9.9;
    g.spread_penalty = 0.01;
    let mut spread = 0.003f32;
    scale_spread(&mut g, &mut spread, 0.02, &h);
    assert_eq!(spread, 0.003 + 0.01);
    // k = 2 without double fire rate, no silencer factor: (1 - 0.1) * 2 * 0.02 + 0.01.
    let dt = 10.0f32 - 9.9f32;
    assert_eq!(g.spread_penalty, ((1.0 - dt as f64) * 2.0 * 0.02f32 as f64 + 0.01f32 as f64) as f32);
    assert_eq!(g.last_shot, 10.0);
    // A shot at the same instant still grows it (2.1 needs dt >= 0, 3.0 dt >= 0.001).
    let before = g.spread_penalty;
    scale_spread(&mut g, &mut spread, 0.02, &h);
    assert!(g.spread_penalty > before);
    // akick 0 leaves everything alone; so does a client re-prediction.
    h.repredict = true;
    let p = g.spread_penalty;
    scale_spread(&mut g, &mut spread, 0.02, &h);
    assert_eq!(g.spread_penalty, p);
}

#[test]
fn view_kick_is_random_and_sided() {
    ts30_gameplay::tables::make_default();
    for seed in [0u32, 7, 123, 31999] {
        let (g, mut h) = armed(id::AK47);
        let _ = g;
        h.seed = seed;
        vkickback(6.0, kind::RIFLE, false, &mut h);
        h.client = true;
        vkickback(6.0, kind::RIFLE, false, &mut h);
        let (sp, sy, sshake) = h.kicks[0];
        let (cp, cy, cshake) = h.kicks[1];
        assert!(!sshake && !cshake, "no view shake in 2.1");
        assert_eq!(sp, 6.0, "the server (bots) kicks by the full kick");
        // p = SR(seed+1, 3, 6): the range truncates to 3.
        assert!((3.0..6.0).contains(&cp), "seed {seed}: pitch {cp}");
        // r2 = SR(seed+2, 0, 0.75) truncates to 0: yaw = (u ± 0.5) * 6 * 0.65, |u ± 0.5| in [0.5, 1).
        let u = shared::weapons::random::shared_random_float(seed, -0.5, 0.5);
        let side = if u < 0.0 { -0.5 } else { 0.5 };
        // The unrounded product: 2.1 adds it to the angle on the x87 (server `0x10049c7c`).
        assert_eq!(sy, (u as f64 + side) * 6.0 * 0.65);
        assert!((cy - sy).abs() < 1e-5);
        assert!(sy.abs() >= 0.5 * 6.0 * 0.65 - 1e-5 && sy.abs() < 6.0 * 0.65);
    }
    // The client applies no kick for a kick of 0.
    let (_, mut h) = armed(id::AK47);
    h.client = true;
    vkickback(0.0, kind::RIFLE, false, &mut h);
    assert!(h.kicks.is_empty());
}

#[test]
fn akimbo_shots_are_4_units_off_centre() {
    ts30_gameplay::tables::make_default();
    let (mut g, mut h) = armed(id::MK23_AKIMBO);
    g.status[id::MK23_AKIMBO as usize].mode = shared::weapons::mode::AKIMBO_SEMI;
    h.buttons = IN_ATTACK | IN_ATTACK2;
    h.pressed = IN_ATTACK | IN_ATTACK2;
    h.seed = 5;
    fire_once(&mut g, &mut h);
    let toggle = g.status[id::MK23_AKIMBO as usize].toggle;
    let first = if toggle { -4.0 } else { 4.0 };
    assert_eq!(h.shots.iter().map(|s| (s.right_offset, s.seed)).collect::<Vec<_>>(), vec![(first, 5), (-first, 5 + 0x21)]);
    assert_eq!(g.next_primary, 0.1);
    assert_eq!(g.next_secondary, 0.1);

    let (mut g, mut h) = armed(id::MK23_AKIMBO);
    g.status[id::MK23_AKIMBO as usize].mode = shared::weapons::mode::AKIMBO_SEMI;
    h.buttons = IN_ATTACK2;
    h.pressed = IN_ATTACK2;
    fire_once(&mut g, &mut h);
    assert!(g.status[id::MK23_AKIMBO as usize].toggle);
    assert_eq!(h.shots[0].right_offset, 4.0);
    assert_eq!(crate::TS21.akimbo_event_offsets(false, true), (4.0, None));
    assert_eq!(crate::TS21.akimbo_event_offsets(true, false), (4.0, Some(-4.0)));
}

#[test]
fn infinite_clip_saves_the_round_but_an_empty_clip_clicks() {
    ts30_gameplay::tables::make_default();
    let (mut g, mut h) = armed(id::AK47);
    h.powerups = powerup::INFINITE_CLIP;
    fire_once(&mut g, &mut h);
    assert_eq!(g.status[id::AK47 as usize].clip, 30);
    g.status[id::AK47 as usize].clip = 0;
    g.next_primary = 0.0;
    h.shots.clear();
    fire_once(&mut g, &mut h);
    assert!(h.shots.is_empty());
    assert_eq!(h.events.last().unwrap().0, Event::Empty);
    assert_eq!(g.next_primary, 0.1f64 as f32);
    assert_eq!(g.idle, 0.5);
}

#[test]
fn triangle_pellets() {
    for seed in [0u32, 1, 77, 31999] {
        for i in 1..=8 {
            let (x, y) = triangle(seed, i);
            assert!((-1.0..1.0).contains(&x) && (-1.0..1.0).contains(&y));
            let u = |k: u32| shared::weapons::random::shared_random_float(seed + i + k, -0.5, 0.5);
            assert_eq!((x, y), (u(0) + u(1), u(2) + u(3)));
            assert_eq!(crate::TS21.pellet_offset(seed, i, None), Some((x, y)));
        }
    }
    // A bot's pellets: engine random floats, redrawn outside the unit circle.
    let mut draws = [0.5f32, 0.49, 0.5, 0.49, 0.1, 0.2, -0.3, 0.0].into_iter();
    let mut rf = |_lo: f32, _hi: f32| draws.next().unwrap();
    assert_eq!(crate::TS21.pellet_offset(3, 1, Some(&mut rf)), Some((0.3, -0.3)));
}

/// 2.1's server and client sum a pellet's direction in their own orders (issue #161); the port's
/// old all-`float` direction here was `[0x3f2dd873, 0x3f3b97f2, 0xbd34a96f]`.
#[test]
fn pellet_dir_per_side() {
    use shared::pm::Arith;
    use shared::weapons::spread::{PelletOffset, PelletSide, angle_vectors};
    let (aim, right, up) = angle_vectors([3.0, 47.0, 0.0]);
    let (x, y) = triangle(7, 1);
    assert_eq!((x.to_bits(), y.to_bits()), (0xbd9f_3000, 0x3e52_f400));
    let dir = |side| crate::TS21.pellet_dir(side, aim, right, up, [0.04, 0.04], PelletOffset::from((x, y))).map(f32::to_bits);
    assert_eq!(dir(PelletSide::Server(Arith::Msvc)), [0x3f2d_d872, 0x3f3b_97f2, 0xbd34_a96f]);
    assert_eq!(dir(PelletSide::Server(Arith::Gcc)), dir(PelletSide::Server(Arith::Msvc)), "2.1 had no Linux server");
    assert_eq!(dir(PelletSide::Client), [0x3f2d_d872, 0x3f3b_97f1, 0xbd34_a96f]);
}

#[test]
fn timers_run_on_raw_time() {
    ts30_gameplay::tables::make_default();
    setup();
    let r = &crate::TS21;
    assert_eq!(r.timer_step(Arith::CLIENT, 0.01, 0.35, powerup::DOUBLE_FIRERATE), 0.01);
    assert_eq!(r.command_frame_time(7), 7.0 / 1000.0);
    // The client subtracts the unrounded quotient.
    let mut g = Gun { next_secondary: 0.3, ..Gun::default() };
    g.decrement_timers(r.command_frame_time(7));
    assert_eq!(g.next_secondary, (0.3f32 as f64 - 0.007) as f32);
}

#[test]
fn spread_decays_by_elapsed_time() {
    ts30_gameplay::tables::make_default();
    setup();
    let r = &crate::TS21;
    let mut g = Gun { spread_penalty: 0.05, decay_time: 10.0, ..Gun::default() };
    let d = |time: f32, time_scale: f32, alive: bool| SpreadDecay { arith: Arith::CLIENT, step: 123.0, time, time_scale, alive };
    r.decay_spread(&mut g, &d(10.1, 1.0, true));
    assert_eq!(g.spread_penalty, (0.05f32 as f64 - (10.1f32 - 10.0f32) as f64 * 0.1) as f32);
    assert_eq!(g.decay_time, 10.1);
    // In slow motion it decays by game time x fuser1.
    let p = g.spread_penalty;
    r.decay_spread(&mut g, &d(10.2, 0.5, true));
    assert_eq!(g.spread_penalty, (p as f64 - (10.2f32 - 10.1f32) as f64 / 2.0 * 0.1) as f32);
    // Dead: no decay, but the time moves on; and it stops at 0.
    let p = g.spread_penalty;
    r.decay_spread(&mut g, &d(20.0, 1.0, false));
    assert_eq!((g.spread_penalty, g.decay_time), (p, 20.0));
    r.decay_spread(&mut g, &d(30.0, 1.0, true));
    assert_eq!(g.spread_penalty, 0.0);
}

fn pre_think_at(time: f32, frametime: f32, fuser1: f32, powerups: u16, alive: bool) -> WeaponPreThink {
    WeaponPreThink { arith: Arith::Msvc, frametime, time, fuser1, powerups, alive }
}

/// Issue #201: 2.1's `PreThink` (0x100537e0) regenerates the bar by `(float)(time − last)` over
/// the slow divisor × 15, with no double-fire-rate factor; TS 3.0's by `frametime × fuser1`,
/// ×1.5 with double fire rate.
#[test]
fn pre_think_bar_regen_on_elapsed_game_time() {
    ts30_gameplay::tables::make_default();
    setup();
    let r = &crate::TS21;
    let (last, now) = (100.0f32, 100.1f32);
    let elapsed = now - last;
    for (fuser1, div) in [(1.0f32, 1.0f64), (0.5, 2.0), (0.05, 10.0)] {
        for powerups in [0, powerup::DOUBLE_FIRERATE] {
            let mut g = Gun { melee_bar: 50.0, decay_time: last, ..Gun::default() };
            let mut disarm = 0.0;
            // A frame time unlike the elapsed game time: 2.1 doesn't read it.
            r.pre_think(&mut g, &mut disarm, &pre_think_at(now, 0.25, fuser1, powerups, true));
            assert_eq!(g.melee_bar, (elapsed as f64 / div * 15.0 + 50.0) as f32, "fuser1 {fuser1} powerups {powerups}");
            assert_eq!(g.decay_time, now);
        }
    }
    // TS 3.0: the frame time, slowed and sped up by double fire rate.
    let bar = |fuser1: f32, powerups: u16| {
        let mut g = Gun { melee_bar: 50.0, decay_time: last, ..Gun::default() };
        CORE.pre_think(&mut g, &mut 0.0, &pre_think_at(now, 0.1, fuser1, powerups, true));
        g.melee_bar
    };
    assert_eq!(bar(1.0, 0), 51.5);
    assert_eq!(bar(1.0, powerup::DOUBLE_FIRERATE), (0.1f64 as f32 as f64 * 1.5 * 15.0 + 50.0) as f32);
    assert_eq!(bar(0.5, 0), (0.05f32 as f64 * 15.0 + 50.0) as f32);
    let mut g = Gun { melee_bar: 50.0, decay_time: last, ..Gun::default() };
    r.pre_think(&mut g, &mut 0.0, &pre_think_at(now, 0.1, 1.0, powerup::DOUBLE_FIRERATE, true));
    assert!(g.melee_bar < bar(1.0, powerup::DOUBLE_FIRERATE));
}

/// 2.1's regeneration: capped at 100 from the unrounded sum, none at 100 or NaN, no 1/s branch
/// below 0, time going back drains it, and none while dead (the time still moves on).
#[test]
fn pre_think_bar_regen_edges() {
    assert_eq!(regen_melee_bar(99.0, 1.0, 1.0), 100.0);
    assert_eq!(regen_melee_bar(100.0, 1.0, 1.0), 100.0);
    assert!(regen_melee_bar(f32::NAN, 1.0, 1.0).is_nan());
    assert_eq!(regen_melee_bar(-5.0, 0.5, 1.0), 2.5);
    assert_eq!(regen_melee_bar(50.0, -1.0, 1.0), 35.0);
    // `fuser1` 0 (2.1's normal speed) divides by 1.
    assert_eq!(regen_melee_bar(50.0, 1.0, 0.0), 65.0);
    let mut g = Gun { melee_bar: 50.0, decay_time: 10.0, ..Gun::default() };
    crate::TS21.pre_think(&mut g, &mut 0.0, &pre_think_at(11.0, 1.0, 1.0, 0, false));
    assert_eq!((g.melee_bar, g.decay_time), (50.0, 11.0));
}

/// 2.1's melee window and block timers are end times (#202): PreThink closes the window once its
/// end is behind the time (still open at `end == time`) or its damage is spent, leaves the block
/// timers alone and doesn't close the kung-fu block. 2.1 has no disarm count to decay.
#[test]
fn pre_think_melee_timers() {
    let r = &crate::TS21;
    let mut g = Gun {
        melee_window: 10.3,
        melee_damage: 15.0,
        melee_source: 1,
        block_kungfu: 10.3,
        block_knife: 10.1,
        block_katana: 10.2,
        decay_time: 10.0,
        ..Gun::default()
    };
    let mut disarm = 7.0;
    r.pre_think(&mut g, &mut disarm, &pre_think_at(10.25, 0.01, 0.5, powerup::DOUBLE_FIRERATE, true));
    assert_eq!((g.melee_window, g.melee_damage, g.melee_source, disarm), (10.3, 15.0, 1, 7.0));
    assert_eq!((g.block_kungfu, g.block_knife, g.block_katana), (10.3, 10.1, 10.2), "end times, untouched");
    // Past its end: the window, damage and source go; the blocks stay as they are.
    r.pre_think(&mut g, &mut disarm, &pre_think_at(10.5, 0.01, 1.0, 0, true));
    assert_eq!((g.melee_window, g.melee_damage, g.melee_source, g.block_kungfu), (0.0, 0.0, 0, 10.3));
    // Exactly at its end it stays open for this think; a spent window closes at once.
    let mut g = Gun { melee_window: 10.5, melee_damage: 15.0, decay_time: 10.0, ..Gun::default() };
    r.pre_think(&mut g, &mut disarm, &pre_think_at(10.5, 0.01, 1.0, 0, true));
    assert_eq!((g.melee_window, g.melee_damage), (10.5, 15.0));
    r.pre_think(&mut g, &mut disarm, &pre_think_at(10.51, 0.01, 1.0, 0, true));
    assert_eq!(g.melee_window, 0.0);
    let mut g = Gun { melee_window: 10.5, melee_damage: 0.0, melee_source: 2, decay_time: 10.0, ..Gun::default() };
    r.pre_think(&mut g, &mut disarm, &pre_think_at(10.01, 0.01, 1.0, 0, true));
    assert_eq!((g.melee_window, g.melee_source), (0.0, 0));
    // The end doesn't drift with how often PreThink runs: many short thinks close it at the
    // same think as one long one (a countdown of `float` steps could end a think apart).
    let mut g = Gun { melee_window: 10.3, melee_damage: 15.0, decay_time: 10.0, ..Gun::default() };
    let mut time = 10.0f32;
    while g.melee_window != 0.0 {
        time += 0.01;
        r.pre_think(&mut g, &mut disarm, &pre_think_at(time, 0.01, 1.0, 0, true));
    }
    assert!(time > 10.3 && time - 0.01 <= 10.3);
    // TS 3.0 closes the kung-fu block while a window is open and decays the disarm count (before
    // `Play`, #249); 2.1 does nothing before `Play`.
    let mut g = Gun { melee_window: 0.3, melee_damage: 15.0, block_kungfu: 0.3, ..Gun::default() };
    let mut disarm = 7.0;
    let t = pre_think_at(10.0, 0.1, 1.0, 0, true);
    CORE.pre_think_timers(&mut g, &mut disarm, &t);
    CORE.pre_think(&mut g, &mut disarm, &t);
    assert_eq!(g.block_kungfu, 0.0);
    assert_eq!(disarm, 7.0 - 0.1 * 5.0);
    let mut g = Gun { melee_window: 10.3, melee_damage: 15.0, block_kungfu: 10.3, decay_time: 10.0, ..Gun::default() };
    let (g0, mut disarm) = (g.clone(), 7.0);
    r.pre_think_timers(&mut g, &mut disarm, &pre_think_at(10.5, 0.01, 1.0, 0, true));
    assert_eq!((g, disarm), (g0, 7.0));
}

/// #202: a missed swing's window and a swing's block are `(float)(d + time)` with `d` unrounded
/// (here `T = 1 / 0.45`: 1.6666666 at 1 s, where `d` stored first gives 1.6666667).
#[test]
fn melee_windows_are_end_times() {
    ts30_gameplay::tables::make_default();
    let mut h = Mock { time: 1.0, fuser1: 0.45, pressed: 1, buttons: 1, ..Mock::default() };
    let mut g = Gun { melee_bar: 100.0, ..Gun::default() };
    crate::shared::melee::ccombat_attack(&mut g, &mut h);
    assert_eq!(h.melees, 1);
    assert_eq!((g.melee_window, g.melee_source), (1.666_666_6, 0));
    assert_eq!(crate::shared::melee::whiff_end(slow_div(0.45), 1.0), 1.666_666_6);
    assert_eq!(crate::shared::melee::whiff_end(100.0, 5.0), 8.0, "capped at 3 s");
    assert_eq!(crate::shared::melee::block_end(0.5, 1.0, 10.0), 10.25);
    assert_eq!(crate::shared::melee::block_end(0.5, 100.0, 10.0), 12.5, "capped at hold × 5");
}

/// `CCombatBlock` (#202): the kung-fu block ends at `time + min(3, 0.3·T)`; block.wav plays when
/// the last block ended over a second ago (also at the first, 0), not just when none is running.
#[test]
fn kungfu_block_end_and_sound() {
    use crate::shared::melee::{block_sound, ccombat_block};
    ts30_gameplay::tables::make_default();
    let mut h = Mock { time: 20.0, fuser1: 1.0, ..Mock::default() };
    let mut g = Gun::default();
    ccombat_block(&mut g, &mut h);
    assert!((g.block_kungfu - 20.3).abs() < 1e-5 && g.next_primary == 0.3);
    assert_eq!(h.block_sounds, 1, "the first block");
    // 2.1 mode: 3.0's block view animation to the owner (not predicted), no event (#324).
    assert_eq!((h.anims, h.last_anim, h.events.len()), (1, (g.seq(1), false, 0), 0));
    assert_eq!(g.seq(1), 4, "v_melee.mdl's block");
    assert_eq!(g.idle, 3.0);
    // The block ended 0.9 s ago: no sound, though none is running.
    (g.next_primary, g.next_secondary) = (0.0, 0.0);
    h.time = g.block_kungfu + 0.9;
    ccombat_block(&mut g, &mut h);
    assert_eq!(h.block_sounds, 1);
    // Over a second after the last one ended: block.wav.
    (g.next_primary, g.next_secondary) = (0.0, 0.0);
    h.time = g.block_kungfu + 1.5;
    ccombat_block(&mut g, &mut h);
    assert_eq!(h.block_sounds, 2);
    assert!(!block_sound(11.0, 10.0) && block_sound(11.000_001, 10.0) && block_sound(1.5, 0.0));
    // `(float)(min(3, T·0.6f·0.5) + time)`, `T` unrounded.
    let mut h = Mock { time: 1.0, fuser1: 0.45, ..Mock::default() };
    let mut g = Gun::default();
    ccombat_block(&mut g, &mut h);
    assert_eq!(g.block_kungfu, (slow_div(0.45) * 0.6f32 as f64 * 0.5 + 1.0) as f32);
    assert_eq!(g.block_kungfu, 1.666_666_7);
}

/// Issue #111: the crosshair's copy (client `0x1031bf04`) is written only in the decay branch
/// (`0x10018dae`): not at a penalty of 0 or NaN, not when the time went back, not while dead.
#[test]
fn crosshair_copy_only_when_decaying() {
    ts30_gameplay::tables::make_default();
    setup();
    let r = &crate::TS21;
    let d = |time: f32, alive: bool| SpreadDecay { arith: Arith::CLIENT, step: 0.01, time, time_scale: 1.0, alive };
    let mut g = Gun { spread_penalty: 0.05, decay_time: 10.0, ..Gun::default() };
    assert!(r.decay_spread(&mut g, &d(10.01, true)), "a positive penalty decays and is copied");
    g.decay_time = 11.0;
    assert!(!r.decay_spread(&mut g, &d(10.5, true)), "time went back");
    assert!(!r.decay_spread(&mut g, &d(12.0, false)), "dead");
    for pen in [0.0, -0.0, f32::NAN] {
        g.spread_penalty = pen;
        assert!(!r.decay_spread(&mut g, &d(13.0, true)), "{pen}");
    }
    // TS 3.0 copies on every first prediction.
    let mut g = Gun::default();
    assert!(CORE.decay_spread(&mut g, &d(13.0, true)));
}

#[test]
fn prediction_and_gate() {
    ts30_gameplay::tables::make_default();
    setup();
    let r = &crate::TS21;
    assert_eq!(r.predicted(), Predicted { reload_finish: false, alt_attack: false, kungfu_reload: false });
    // The client's frame doesn't run the stock hit; +alt2 still lets the server's frame run in a
    // reload of a gun without one.
    let ak = info(id::AK47).unwrap();
    assert!(r.cancel_runs_frame_in_reload(ak));
    assert!(!CORE.cancel_runs_frame_in_reload(ak));
    let mut g = Gun { current: id::AK47, next_attack: 1.0, ..Gun::default() };
    g.status[id::AK47 as usize] = Status { owned: true, in_reload: 1, ..Default::default() };
    assert!(g.post_frame_ready(0x40, r));
    assert!(!g.post_frame_ready(0x40, &CORE));
}

/// 2.1's Barrett/M60 gate (issue #171): both of its MSVC builds compare the unrounded root, so
/// `80² + 0.0001²` is over 80 (the port's old `float` length was 80.0).
#[test]
fn speed_gate_keeps_the_root_unrounded() {
    use shared::weapons::gun::speed_over;
    assert!(speed_over::<shared::pm::Msvc>([80.0, 0.0001, 0.0], 80.0));
    assert!(!speed_over::<shared::pm::Msvc>([80.0, 0.0, 0.0], 80.0));
}

/// #198 (plugin API version 5, `melee_attack`): 2.1's kung fu asks the hook as 3.0's does: it sees
/// the move's delay (before the time-scale factor) and the damage a hit deals (2.1's bar scaling
/// applied), its changes reach the hit and the timers, and a block leaves only the delay: no
/// hold, bar cost, hit, `BlockPlayer` or event.
#[test]
fn kungfu_melee_attack_hook() {
    ts30_gameplay::tables::make_default();
    setup();
    const IN_ATTACK: u32 = 1;
    const IN_DUCK: u32 = 4;
    const IN_ATTACK2: u32 = 0x800;
    let mut g = Gun { melee_bar: 100.0, ..Gun::default() };
    let mut h = Mock { fuser1: 1.0, ..Default::default() };
    let attack = |g: &mut Gun, h: &mut Mock, buttons: u32| {
        (g.next_attack, g.next_primary, g.next_secondary) = (0.0, 0.0, 0.0);
        (h.buttons, h.pressed) = (buttons, buttons);
        h.events.clear();
        g.ccombat_attack(h);
    };
    // A straight punch: 10 × 0.9 at a full bar, 0.15 s.
    attack(&mut g, &mut h, IN_ATTACK);
    assert_eq!(h.melee_pre, vec![(0.15, 9.0)]);
    assert_eq!((h.melee_damage.clone(), g.next_primary, g.melee_bar), (vec![9.0], 0.15, 91.0));
    // Changed: the hit deals the new damage; the delay is the new time, `BlockPlayer` 0.9 of it.
    h.melee_answer = Some((1.25, 55.0, true));
    attack(&mut g, &mut h, IN_ATTACK);
    assert_eq!(h.melee_damage.last(), Some(&55.0));
    assert_eq!((g.next_primary, g.next_secondary, g.idle), (1.25, 1.25, 2.0), "the idle animation waits 3.0's 2 s");
    assert_eq!(h.blocks.last(), Some(&((1.25f64 * 0.9) as f32)));
    // #242: the `float` 0.9, and the slowed duration (`FixedSlowMul() × delay × 0.9f`) rounded to
    // a `float`: 0.75 × 0.9 would be 0x3f2ccccd.
    h.melee_answer = Some((0.75, 55.0, true));
    attack(&mut g, &mut h, IN_ATTACK);
    assert_eq!(h.blocks.last().map(|b| b.to_bits()), Some(0x3f2c_cccc));
    h.fuser1 = 0.35;
    attack(&mut g, &mut h, IN_ATTACK);
    let slowed = (1.0 / f64::from(0.35f32) * 0.75 * f64::from(0.9f32)) as f32;
    assert_eq!(h.blocks.last(), Some(&slowed));
    h.fuser1 = 1.0;
    // Blocked sweep: its own values asked; nothing but the delay.
    let (hits, blocks, bar, last) = (h.melees, h.blocks.len(), g.melee_bar, g.last_melee_anim);
    h.melee_answer = Some((0.8, 1.0, false));
    attack(&mut g, &mut h, IN_ATTACK2 | IN_DUCK);
    assert_eq!(h.melee_pre.last(), Some(&(1.0, 27.0 * bar * 0.01)), "the sweep's delay and damage");
    assert_eq!((h.melees, h.blocks.len(), g.melee_bar, g.last_melee_anim), (hits, blocks, bar, last));
    assert!(h.holds.is_empty() && h.events.is_empty());
    assert_eq!((g.next_primary, g.next_secondary), (0.8, 0.8));
    // Let through, the sweep holds the player.
    h.melee_answer = None;
    attack(&mut g, &mut h, IN_ATTACK2 | IN_DUCK);
    assert_eq!(h.holds, vec![4 | 1]);
    assert_eq!(h.events.len(), 1);
}

/// `GrenadeIdle` (0x1004d260): the throw adds x's and y's `speed·forward` unrounded (3.0's mp.dll
/// rounds y's first), with 3.0's eye; the roll keeps `k·f.x` unrounded and stores y's and z's
/// (3.0's mp.dll: x and y stored, z not). Same inputs as 3.0's `grenade_throw_rounds_as_ts`.
#[test]
fn grenade_throw_rounds_as_2_1() {
    use shared::pm::Arith;
    let p = f32::from_bits(0xc2b0_6666);
    let r = Ts21.grenade_rounding(Arith::Gcc);
    assert_eq!(r, Ts21.grenade_rounding(Arith::Msvc), "one build");
    let throw = |r: &GrenadeRounding, roll| {
        Gun::grenade_throw(r, [100.3, -20.7, 64.1], [0.0, 0.0, 28.0], [120.5, -33.3, 7.7], [p, 30.0, 0.0], roll)
    };
    let (src, vel) = throw(&r, false);
    assert_eq!(src.map(f32::to_bits), [0x42c9_5fb1, 0xc1a3_d022, 0x42d8_3001]);
    assert_eq!(vel.map(f32::to_bits), [0x430a_a2bc, 0xc1b6_a289, 0x443d_5a16]);
    let v30 = CORE.grenade_rounding(Arith::Msvc);
    assert_eq!(throw(&v30, false).1[1].to_bits(), 0xc1b6_a288, "3.0's mp.dll stores y's product");
    // The roll: compare per component with sums done by hand.
    let (src, vel) = throw(&r, true);
    let (f, _, _) = shared::weapons::spread::angle_vectors([(p as f64 * (8.0 / 9.0) - 10.0) as f32, 30.0, 0.0]);
    let speed = (((90.0 - (p as f64 * (8.0 / 9.0) - 10.0) as f32 as f64) * 7.0) as f32).min(750.0);
    let k = speed as f64 * (2.0 / 3.0);
    let v = [120.5f32, -33.3, 7.7];
    let want = [
        (k * f[0] as f64 + v[0] as f64) as f32,
        ((k * f[1] as f64) as f32 as f64 + v[1] as f64) as f32,
        ((k * f[2] as f64) as f32 as f64 + v[2] as f64) as f32,
    ];
    assert_eq!(vel, want);
    assert_eq!(src, shared::weapons::spread::ma([100.3, -20.7, 64.1], 16.0, f));
}
