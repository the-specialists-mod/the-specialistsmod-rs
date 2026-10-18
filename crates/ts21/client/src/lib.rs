//! TS 2.1's client rules: HUD and input (issue #100 §4), and the plugin object `client-dll`
//! registers. Addresses are TS 2.1 `client.dll` VAs (decomp `ts21/client`); see
//! `docs/implementation/ts21.md` ("Client: HUD and input").

#![deny(unsafe_code)]

use goldsrc::Global;
use goldsrc::ext::render::{Vec3, random_long};
use std::cell::RefCell;
use std::cmp::Ordering;
use ts_client::effects::fx::FxStyle;
use ts_client::effects::psys::{PSys, ParticleModel};
use ts_client::events::{BulletDraw, BulletStep, ClientBullet};
use ts_client::gameplay::MeleeReach;
use ts_client::gameplay::{
    ClientPlugin, ClientRules, CrosshairParts, KungfuRegen, SlotMenu, SpectatorTail, StuntCooldown, ThinkParts, TsMessageLayout,
};
use ts_client::hud::draw::{HudOp, HudSprite, StatusLook, WeaponPanel, ftol, num};
use ts_client::hud::saytext::SayTextStyle;
use ts_client::menus::buy::MenuRules;
use ts_client::spectator::HltvHost;
use ts_client::view::{Bob, BobInput, EyeZ};
use ts21_gameplay::TS21;

pub mod bullets;
pub mod effects;
pub mod events;
pub mod hud;
pub mod spectator;
pub mod studio;
pub mod view;

/// 2.1's attachment rows, as written (`0x10271a8c`, `0x10271a7c`, `0x1026e778`, `0x10271a74`).
const ATTACHMENTS: [&str; 4] = ["Silencer", "Lasersight", "Flashlight", "Scope"];

/// 2.1's fire-mode names (`switch (mode)` at `0x1008a9dc`; anything but 1..5 is full-auto).
fn firemode_name(mode: u8) -> &'static str {
    match mode {
        1 => "Semi-auto",
        2 => "Burst",
        3 => "Pump-action",
        4 => "Free Semi",
        5 => "Free Full",
        _ => "Full-auto",
    }
}

/// 2.1's weapon panel (`FUN_1008a320`, from the weapon element's `Draw` at `0x10089ed3`): 128 ×
/// 19 rows 20 apart, bottom-up from `(w − 0x88, h − 8)`, in the HUD colour (the `ftol`'d colour
/// cvars):
///
/// * each owned attachment (`+0x798` bit) at alpha 15, or `15 + 30` while active (`+0x14`), its
///   name as written in the colour / 2, and `item_installed.spr` at `x + 0x70` in the colour, or
///   / 2 while inactive;
/// * the ammo row at alpha 15: with an ammo type, the clip (`x + 2`), "/" at `x + 0x40` in the
///   colour / 4 and the reserve at `ftol(x + 94.8148…)` (`0x10271a68`), each `DHN_3DIGITS |
///   DHN_DRAWZERO` in the colour / 1.5 (`0x10270800`), red (195, 45, 45) at 0 or below; then the
///   fire-mode and ammo-name rows (alpha 15, colour / 1.5). Without an ammo type only a stacking
///   weapon's count, at the reserve's place on the same row, and no kung-fu bar;
/// * the name row at alpha 0x23 (colour / 1.5);
/// * scoped (`m_iFOV < 40`), `"%ix"` of `80 / m_iFOV` at (24, 24) in the colour.
pub fn weapon_panel(p: &WeaponPanel) -> Vec<HudOp> {
    const ROW: i32 = 0x14;
    let hud = p.hud;
    let div = |d: i32| hud.map(|c| c / d);
    let soft = hud.map(|c| ftol(c as f64 / 1.5));
    let count = |n: i32| if n > 0 { soft } else { [195, 45, 45] };
    let right = ftol(p.w as f64 - 0x88 as f64 + 94.814_814_814_814_81);
    let flags = num::DIGITS_3 | num::DRAW_ZERO;
    let x = p.w - 0x88;
    let mut y = p.h - 8;
    let mut ops = Vec::new();
    let fill = |ops: &mut Vec<HudOp>, y: i32, a: i32| ops.push(HudOp::Fill { x, y, w: 0x80, h: ROW - 1, rgb: hud, a });
    for (bit, label) in ATTACHMENTS.iter().enumerate() {
        let m = 1u32 << bit;
        if p.owned & m == 0 {
            continue;
        }
        y -= ROW;
        let on = p.active & m != 0;
        fill(&mut ops, y, if on { 15 + 30 } else { 15 });
        ops.push(HudOp::Text { x: x + 2, y, text: label, rgb: div(2) });
        ops.push(HudOp::Sprite { sprite: HudSprite::ItemInstalled, x: x + 0x70, y, rgb: if on { hud } else { div(2) } });
    }
    y -= ROW;
    fill(&mut ops, y, 15);
    if p.ammo == 0 {
        if p.stack {
            ops.push(HudOp::Number { x: right, y, flags, n: p.clip, rgb: count(p.clip) });
        }
    } else {
        ops.push(HudOp::Number { x: x + 2, y, flags, n: p.clip, rgb: count(p.clip) });
        ops.push(HudOp::Text { x: x + 0x40, y, text: "/", rgb: div(4) });
        ops.push(HudOp::Number { x: right, y, flags, n: p.reserve, rgb: count(p.reserve) });
        y -= ROW;
        fill(&mut ops, y, 15);
        ops.push(HudOp::Text { x: x + 2, y, text: firemode_name(p.firemode), rgb: soft });
        y -= ROW;
        fill(&mut ops, y, 15);
        ops.push(HudOp::Label { x: x + 2, y, text: p.ammo_name.to_string(), rgb: soft });
    }
    y -= ROW;
    fill(&mut ops, y, 0x23);
    ops.push(HudOp::Label { x: x + 2, y, text: p.name.to_string(), rgb: soft });
    // TS divides by `m_iFOV` unguarded (`idiv` at `0x1008ac57`); the port skips 0 rather than
    // fault.
    if p.fov < 40 && p.fov != 0 {
        ops.push(HudOp::Label { x: 0x18, y: 0x18, text: format!("{}x", 80i32.wrapping_div(p.fov)), rgb: hud });
    }
    ops
}

/// 2.1's own spectator state ([`spectator::Hltv`]).
static HLTV: Global<RefCell<spectator::Hltv>> = Global(RefCell::new(spectator::Hltv::NEW));

/// TS 2.1's client-only rules.
pub struct Ts21Client;

/// The plugin on the client: TS 2.1's shared hooks and its client hooks.
pub static PLUGIN: ClientPlugin = ClientPlugin { pm: &TS21, gun: &TS21, client: &Ts21Client };

/// 2.1's buy-menu handguns (`CTSBuyMenu` Init 0x1006aa70, table `+0x2d6c`): no single Beretta (2)
/// and no Contender (36); id 8 is the akimbo-only Berettas.
const HANDGUNS: [i32; 9] = [1, 9, 12, 14, 8, 21, 22, 28, 31];

/// `RandomConfig` (0x10070860)'s pistols, picked with `rand(1, 9)`; the MK23 and the Five-seveN may
/// be akimbo (`rand(0, 1)`).
const RANDOM_PISTOLS: [(i32, bool); 9] =
    [(1, false), (9, true), (12, false), (14, true), (8, false), (21, false), (22, false), (28, false), (31, false)];

/// `slowdiv` (0x10095800 over 0x100957c0): 1 at normal speed (`TSSlowMo` 0), else
/// `1 / max(s, 0.1f)`; a NaN factor stays NaN (the x87 compare is unordered).
pub(crate) fn slowdiv(s: f32) -> f64 {
    if s == 0.0 {
        return 1.0;
    }
    let floor = 0.1f32;
    let m = if s < floor { floor } else { s };
    1.0 / m as f64
}

/// The fire events' pitch (0x100738a0) for the slow-motion factor `s` (`TSSlowMo / 100`, 0 =
/// normal speed). `s * 300` is compared at x87 precision (`<= 150`, or unordered for a NaN,
/// gives 150) and stored as a float otherwise; `__ftol` truncates `100 + that`.
#[allow(clippy::neg_cmp_op_on_partial_ord)] // the x87 `fcomp`/`and 0x4100` test: a NaN takes 150
pub fn fire_pitch(s: f32) -> i32 {
    let x = if s == 0.0 {
        300.0f32
    } else {
        let p = s as f64 * 300.0;
        if !(p > 150.0) { 150.0 } else { p as f32 }
    };
    (100.0 + x as f64) as i64 as i32
}

impl ClientRules for Ts21Client {
    fn ruleset_name(&self) -> &'static str {
        "TS 2.1"
    }

    /// The weapons element's crosshair draw (0x1008b960) is one gate: nothing at all (no
    /// divesteps, no laser number) under `cl_realhud`, for kung fu, the M61, the knives and the
    /// katana, and for the Barrett unless `m_iFOV < 40` (tested before third person clears the
    /// scoped flag). Otherwise the crosshair, then the divesteps on top (0x1008c1c0).
    fn crosshair_parts(&self, weapon: i32, fov: f32) -> CrosshairParts {
        let hidden = matches!(weapon, 0 | 24 | 25 | 34 | 35) || (weapon == 18 && (fov as i32) >= 40);
        CrosshairParts { crosshair: !hidden, divesteps: !hidden, divesteps_last: true }
    }

    /// 0x1008bb67: `ftol(base + ((float)(acc + spread) − acc) / 0.1f × 16)`. Unlike 3.0, the sum is
    /// stored to a float first, and `/ 0.1f × 16` is a hair under × 160, so a spread that should
    /// give a whole number of pixels can come out one short. f64 stands in for the x87 stack.
    /// Guarded as 3.0's: the gap is clamped to ±0x10000.
    fn crosshair_gap(&self, base: i32, spread: f32, accuracy: f32) -> i32 {
        let sum = (accuracy as f64 + spread as f64) as f32;
        let gap = (sum as f64 - accuracy as f64) / 0.1f32 as f64 * 16.0 + base as f64;
        (gap as i32).clamp(-0x10000, 0x10000)
    }

    /// 0x1008c0f0/0x1008c160/0x1008c190: the top dive needs `used ≤ 21` and, when `m_Teamplay > 1`,
    /// no kevlar; the centre `used ≤ 41`, the bottom `used ≤ 71` (signed compares).
    fn divesteps_blocked(&self, used_slots: i32, armor: u8, teamplay: i32) -> [bool; 3] {
        let top = used_slots <= 21 && (teamplay < 2 || armor == 0);
        [!top, used_slots > 41, used_slots > 71]
    }

    /// `NextStunt` (0x1008d6a0) stores `m_flTime + (float)tenths / 10` and the divesteps
    /// (0x1008c1cf) are red unless that time has passed (red when it's NaN too). Real time:
    /// slow motion doesn't stretch it, unlike 3.0's countdown.
    fn stunt_cooldown(&self, c: StuntCooldown, now: f32) -> bool {
        let expiry = c.received_at + c.tenths as f32 / 10.0;
        expiry.partial_cmp(&now) != Some(Ordering::Less)
    }

    /// `CHud::Think` (0x10084aa0) writes 80.0 into `default_fov`'s value every frame, so the
    /// player's `default_fov` never takes effect.
    fn pinned_default_fov(&self) -> Option<f32> {
        Some(80.0)
    }

    /// The FOV easer (0x10084800) has 3.0's step, but jumps whenever `m_iFOV` is outside 1..80.
    fn fov_snaps(&self, cur: i32) -> bool {
        !(1..=80).contains(&cur)
    }

    /// The `+` commands (`+forward` 0x10050490, `+attack` 0x10050b40, ...; `InitInput` 0x10051b80)
    /// press their buttons unconditionally, so `alias`/`wait` scripts work.
    fn input_gate(&self) -> bool {
        false
    }

    /// No `cl_attackswitch`: `+attack2` is always `IN_ATTACK2` and `+alt2` `IN_ALT2`.
    fn attack_switch(&self) -> bool {
        false
    }

    /// The key hook (0x1004b200) is the SDK's HLTV `HandleButtonsDown`, which never sends
    /// `respawn`: a 2.1 observer respawns with `+attack` on the server.
    fn attack_sends_respawn(&self) -> bool {
        false
    }

    /// The 2.1 client reads the entity lump only for the spectator's `info_player_start`
    /// (0x1004a500): no `ts_particle`, `ts_emitter` or `ts_fog`.
    fn map_effects(&self) -> bool {
        false
    }

    /// The bar (0x1008a0f0) has no exhausted state: an empty bar is just empty.
    fn kungfu_exhausted(&self, _power: f32) -> bool {
        false
    }

    /// The weapons element's `Think` (0x100899f0): below 100, `power += (float)(now − last) /
    /// slowdiv(TSSlowMo) × 15`, at most 100, where `slowdiv` uses the raw factor (no easing);
    /// `last` is the previous `Think`'s `m_flTime`. f64 stands in for the x87 stack.
    fn kungfu_regen(&self, r: KungfuRegen) -> f32 {
        if r.power.partial_cmp(&100.0) != Some(Ordering::Less) {
            return r.power;
        }
        let dt = r.now - r.last;
        let s = r.slowmo_factor100 / 100.0;
        let power = (dt as f64 / slowdiv(s) * 15.0 + r.power as f64) as f32;
        if power > 100.0 { 100.0 } else { power }
    }

    /// The weapons element's `Think` (0x100899f0) is the `+use` power-up press, the kung-fu
    /// regeneration and the weapon menu's attack/attack2 pick (`weapon_%i`/`drop_%i`), nothing
    /// else: no `cl_lasercolor` or `lasclr_`, no fire-mode `weapons/switch.wav` (the client's
    /// only use of it is the predicted attachment toggle, 0x1001fe20; the fire-mode cycle
    /// 0x100200f0 is silent) and no heartbeat or breathing (no `sound/player/` string at all;
    /// `VidInit` 0x100894c0 and `Reset` 0x10089890 have no sound state) (issues #193, #194).
    fn think_parts(&self) -> ThinkParts {
        ThinkParts { laser_color: false, firemode_click: false, sounds: false }
    }

    /// The weapons element's `Reset` (0x10089890) refills the bar on every `ResetHUD`.
    fn kungfu_power_on_reset(&self) -> Option<f32> {
        Some(100.0)
    }

    /// The status `Draw` (0x10085d90) ends with `sprites/slowmotion.spr` while slowed.
    fn slowmo_icon(&self) -> bool {
        true
    }

    /// Deviation: 2.1 has no `cl_slowfade` and writes no screen fade for slow motion (the icon is
    /// its only cue, issue #152). 2.1 mode keeps 3.0's tint as a visual-only cue on top of the icon
    /// (issue #324); `cl_slowfade 0` turns it off.
    fn slowmo_tint(&self) -> bool {
        true
    }

    /// The fire events' pitch helper (0x100738a0, through thunk 0x1000367f from 0x10073b70,
    /// 0x10075080 and 0x10076ce0): `s = TSSlowMo / 100` as the handler stores it (0x1031bee4,
    /// not eased), `ftol(100 + (s == 0 ? 300 : (s * 300 <= 150 ? 150 : (float)(s * 300))))`.
    /// 400 at normal speed, never below 250. The fire sound (0x10073a40) adds
    /// `RandomLong(-5, 5)`, as 3.0's.
    fn fire_pitch(&self, _time_scale: f32, slowmo_factor100: f32) -> i32 {
        fire_pitch(slowmo_factor100 / 100.0)
    }

    /// `V_CalcBob` (0x100b5c80) with its camera bob ([`view::calc`]).
    fn view_bob(&self, bob: &mut Bob, input: &BobInput) -> f32 {
        view::calc(bob, input)
    }

    /// The raw view height and the eye z follow (0x100b6b80, [`view::eye_origin`]).
    fn eye_origin(&self, eye: &mut EyeZ, simorg: [f32; 3], viewheight: [f32; 3], _on_ground: bool, ft: f32) -> [f32; 3] {
        view::eye_origin(eye, simorg, viewheight, ft)
    }

    /// The same follow (0x100b6b80, [`view::follow_z`]).
    fn body_cam_z(&self, eye: &mut EyeZ, z: f32, _on_ground: bool, ft: f32) -> f32 {
        view::follow_z(eye, z, ft)
    }

    /// `EV_TS_FireBullets` (0x1007c5e0): `SrvSett 0 == 1 || TSSlowMo != 0` ([`bullets::queues`]).
    fn client_bullets_queue(&self, realbullet: i32, _time_scale: f32, slowmo_factor100: f32) -> bool {
        bullets::queues(realbullet, slowmo_factor100)
    }

    /// Every 0.1 s ([`bullets::next_update`]).
    fn client_bullet_next_update(&self, now: f32, _realbullet: i32) -> f32 {
        bullets::next_update(now)
    }

    /// A fixed `dir × velocity / 0.25 × s` per step ([`bullets::step`]).
    fn client_bullet_step(&self, dir: [f32; 3], s: &BulletStep) -> [f32; 3] {
        bullets::step(dir, s)
    }

    /// The float-stored quotient ([`bullets::range_index`]).
    fn client_bullet_range_index(&self, dist: f64, unit: f32, exit: bool) -> i32 {
        bullets::range_index(dist, unit, exit)
    }

    /// Drawn while a `TSSlowMo` factor is set (0x1006a13b), on the raw factor.
    fn client_bullet_draws(&self, _time_scale: f32, slowmo_factor100: f32) -> bool {
        slowmo_factor100 / 100.0 != 0.0
    }

    /// 2.1's models and trails ([`bullets::draw`]).
    fn client_bullet_draw(&self, bullet: &mut ClientBullet, d: &mut BulletDraw) -> bool {
        bullets::draw(bullet, d);
        true
    }

    /// 2.1's builders ([`events::FX`]).
    fn fx_style(&self) -> &'static FxStyle {
        &events::FX
    }

    /// 0x10084240 ([`events::impact`]).
    fn impact_particles(&self, ps: &mut PSys, now: f64, pos: Vec3, n: Vec3, tex: u8, bullet_type: i32) -> bool {
        events::impact(ps, now, pos, n, tex, bullet_type);
        true
    }

    /// [`events::gun_smoke_rate`].
    fn gun_smoke_rate(&self, id: i32) -> i32 {
        events::gun_smoke_rate(id)
    }

    /// The standard and pump fire pass 2 as the spray's gravity (0x1007427b, 0x10077265).
    fn pellet_spray_gravity(&self) -> f32 {
        2.0
    }

    /// `RandomLong(120, 140)` for every gun (0x10073ec4).
    fn shell_side_speed(&self, _id: i32) -> i32 {
        random_long(120, 140)
    }

    /// Always 1 in the standard and akimbo fire (the pump fire's is 2, as 3.0's).
    fn shell_sound(&self, _id: i32) -> i32 {
        1
    }

    /// 60-80 and 50-100 (0x10077031, 0x10077043).
    fn pump_shell_speeds(&self) -> [(i32, i32); 2] {
        [(60, 80), (50, 100)]
    }

    /// [`events::ammo_attenuation`].
    fn ammo_attenuation(&self, ammo: usize) -> f32 {
        events::ammo_attenuation(ammo)
    }

    /// `EV_TS_FireBullets` (0x1007c5e0) has neither.
    fn bullet_whizz_and_push(&self) -> bool {
        false
    }

    /// 0x1007b820: attenuation 0.97, pitch `96 + RandomLong(0, 15)`.
    fn bullet_material_sound(&self) -> (f32, i32) {
        (0.97, 96 + random_long(0, 15))
    }

    /// [`events::ricochet_pitch`].
    fn ricochet_pitch(&self, _time_scale: f32, slowmo_factor100: f32) -> i32 {
        events::ricochet_pitch(slowmo_factor100)
    }

    /// [`events::event_sound`].
    fn event_sound(&self) -> (f32, f32, i32) {
        events::event_sound()
    }

    /// [`events::melee_material_sound`].
    fn melee_material_sound(&self, tex: u8) -> Option<(f32, f32, i32)> {
        events::melee_material_sound(tex)
    }

    /// [`events::melee_reach`].
    fn melee_reach(&self, m: MeleeReach) -> f32 {
        events::melee_reach(m)
    }

    /// The close-combat event (0x100792e0) has no block branch: it always plays
    /// `closecombat.wav` and traces (2.1's prediction plays `block.wav`, 0x100245b0).
    fn close_combat_blocks(&self) -> bool {
        false
    }

    /// [`events::blade_hit`].
    fn blade_hit_particles(&self, ps: &mut PSys, now: f64, pos: Vec3, n: Vec3, katana: bool) -> bool {
        events::blade_hit(ps, now, pos, n, katana);
        true
    }

    /// The reload event (0x10078940) ignores `fparam1`; 2.1's prediction plays `insert-shell.wav`
    /// (0x10023660). 2.1 played the reload view animation from `ReloadWeapon`'s `SendWeaponAnim`
    /// (mp.dll 0x1004ce20, the same sequence slots); the 3.0 view models take it from the event,
    /// so the event still plays it (a visual compatibility deviation).
    fn reload_sound_phases(&self) -> bool {
        false
    }

    /// `MsgFunc_WeaponInfo` (0x1008d1a0) for kung fu (id 0, 0x1008d35e): the bar becomes the clip
    /// byte, clamped to 0..100 (issue #111). The server sends kung fu's `WeaponInfo` with the bar
    /// as its clip when the weapon changes, so switching to kung fu resyncs the bar the client
    /// regenerates on its own.
    fn kungfu_power_from_weapon_info(&self, clip: u8) -> Option<f32> {
        Some((clip as f32).min(100.0))
    }

    /// `MsgFunc_WeaponInfo` for kung fu (`id` `+0x7b8` == 0, `0x1008d239` → `0x1008d35e`) zeroes
    /// the weapons element's `+0x740` and `+0x744`, the slot menu's grid indices
    /// (`grid[+0x740][+0x744]`, read at `0x10089b98..0x10089bd9`) but not its open flag `+0x73c`
    /// (issue #178).
    fn kungfu_weapon_info_resets_menu_selection(&self) -> bool {
        true
    }

    /// The key hook is `HandleButtonsDown` (`0x1004b200`; `-` commands `HandleButtonsUp`
    /// `0x1004b4c0`), never the `respawn` tap (issue #177).
    fn hltv_buttons(&self, host: &mut dyn HltvHost, buttons: i32, down: bool) -> bool {
        HLTV.0.borrow_mut().buttons(host, buttons, down);
        true
    }

    /// 2.1's `SetModes` (`0x1004b530`).
    fn hltv_set_modes(&self, host: &mut dyn HltvHost, mode: i32, inset: i32) -> bool {
        HLTV.0.borrow_mut().set_modes(host, mode, inset);
        true
    }

    fn hltv_reset(&self, init: bool) {
        HLTV.0.borrow_mut().reset(init);
    }

    /// `Draw` (`0x1004a8d0`): the deltas, then the name tags of the map main mode (issue #181).
    fn hltv_draw(&self, host: &mut dyn HltvHost) -> bool {
        let h = HLTV.0.borrow();
        h.draw(host);
        h.draw_names(host);
        true
    }

    /// `V_CalcRefdef` (`0x100ba6b0`) runs the spectator view only for `pparams->spectator`
    /// (`0x100ba6dc..0x100ba6e9`, HLTV): there is no `g_iUser1` test, so a player who observes
    /// gets the normal view (`0x100b6c90`) at their own (movement-tracked) origin, with its bob
    /// and view model (issue #183).
    fn spectator_refdef(&self, spectator: bool, _observer_mode: i32) -> bool {
        spectator
    }

    /// 2.1's `V_CalcSpectatorRefdef` mode passes (`0x100b96a0`, issue #181), on the main and
    /// inset modes. Only HLTV viewers get here ([`Self::spectator_refdef`]).
    fn hltv_calc_refdef(&self, host: &mut dyn HltvHost, p: &mut goldsrc::sys::ref_params_t) -> bool {
        HLTV.0.borrow().calc_refdef(host, p);
        true
    }

    /// The overview's camera icon follows 2.1's in-eye inset or main mode and its
    /// `V_GetInEyePos` (`0x1004cb50`, issue #186).
    fn hltv_overview_camera(&self, host: &dyn HltvHost, origin: &mut Vec3, angles: &mut Vec3) -> bool {
        HLTV.0.borrow().overview_camera(host, origin, angles);
        true
    }

    /// `DrawOverview` (`0x1004dd10`) draws in the main view only in the map main mode (5).
    fn hltv_overview_in_main_view(&self) -> bool {
        HLTV.0.borrow().main == 5
    }

    /// `UpdateSpectatorPanel` (`0x100af0c0`, `0x100af12d`) numbers an HLTV viewer's line by the
    /// main mode (`ds:0x102daab4`), not the observer mode.
    fn hltv_panel_mode(&self) -> Option<i32> {
        Some(HLTV.0.borrow().main)
    }

    /// `0x100af194..0x100af1a7`: `fcomp 0.0; test ah, 0x44; jp hide`, so a NaN `nohud` hides the
    /// panel too (3.0 shows it).
    fn spectator_panel_shows(&self, nohud: f32) -> bool {
        nohud == 0.0
    }

    /// The weapon element's kung-fu bar (0x1008a0f0, issue #111), 128 wide from `w - 136`: the
    /// full part `ftol(1.28f × power)` wide at alpha 0x40 and the empty rest at 0x1e, both 19
    /// high at `h - 28`, both in the HUD colour (3.0: a frame and a fill scaled by the power);
    /// above it a 0x23 box with "Kung-Fu" in the HUD colour ÷ 1.5 (the `double` at 0x10270800).
    /// The status panel (0x10085d90, issue #111): the stance sprite whole in the HUD colour / 2
    /// (drawn at 0x10085ea3); the armour number at (0x48, y + 0x18) with the kevlar icon at 0x68
    /// (0x1008610c); the team bar's box in the HUD colour at alpha 0x23, then `ScaleColors(..,
    /// 200)` on the panel's colour (0x100861b8), which the name, the slots and credits rows and
    /// the icons then use; "free slots" and "Credits" as written (0x102716ac, 0x102716a0); the
    /// kung-fu, movement and slow-motion icons in the colour / 1.5 (`double` 0x10270800; drawn
    /// at 0x100865c0, 0x10086648, 0x100866e6).
    fn status_look(&self) -> StatusLook {
        StatusLook {
            stance_health_fill: false,
            armor_number: true,
            team_bar_scale: Some(200),
            labels: Some(("free slots", "Credits")),
            icons_divide: true,
        }
    }

    fn kungfu_label(&self, power: f32, hud: [i32; 3], w: i32, h: i32) -> Option<Vec<HudOp>> {
        let (x, y) = (w - 0x88, h - 8 - 0x14);
        let full = ftol(1.28f32 as f64 * power as f64);
        let mut ops = vec![HudOp::Fill { x, y, w: full, h: 0x13, rgb: hud, a: 0x40 }];
        if full < 0x80 {
            ops.push(HudOp::Fill { x: x + full, y, w: 0x80 - full, h: 0x13, rgb: hud, a: 0x1e });
        }
        let y = y - 0x14;
        ops.push(HudOp::Fill { x, y, w: 0x80, h: 0x13, rgb: hud, a: 0x23 });
        ops.push(HudOp::Text { x: x + 2, y, text: "Kung-Fu", rgb: hud.map(|c| ftol(c as f64 / 1.5)) });
        Some(ops)
    }

    /// 2.1's weapon panel (`0x1008a320`, issue #176).
    fn weapon_panel(&self, p: &WeaponPanel) -> Option<Vec<HudOp>> {
        Some(weapon_panel(p))
    }

    /// `CashBased()` (0x1006b2f0) is `m_Teamplay ≥ 2 || SrvSett 1`, so the menu spends the live
    /// `TSCash` in round modes and with `usecash` (0x1006b390).
    fn buy_cash_based(&self, teamplay: i32, cashbased: i32) -> bool {
        teamplay >= 2 || cashbased != 0
    }

    /// The 2.1 menu (`CTSBuyMenu` at 0x102dc084, 0x1006aa70..0x10071000): its handguns and random
    /// pistols; kevlar and stealth shoes cost no slots and skip the slot check (add 0x1006f5b0,
    /// remove 0x1006f340), and are offered only when `m_Teamplay > 1` (0x1006cde0, 0x1006e830),
    /// with no restriction of their own; no `cl_buykevlar` (a reset or random config starts
    /// empty); no akimbo fix-up, so key 6 buys any gun "akimbo" at twice the price and slots;
    /// keys 1..10 only.
    ///
    /// Its look (issue #111): hard-coded English text, not `titles.txt` ([`BUY_TEXT`]); the
    /// items page red by money only (0x1006db50); the category column's mouse rows
    /// `(m_Teamplay != 0) + 5` (0x1006e1b0), so in mode 1 the drawn "0 Go Back" is key 6; and the
    /// centre-printed "A hole in the menu!" where the click finds no row (0x1006e1b0's end,
    /// `pfnCenterPrint` through `FUN_10068c10`); the summary's item names in the halved HUD
    /// colour, never red (0x1006b9a0, issue #262). `Open` (0x1006b3f0) opens during demo playback
    /// too (issue #263).
    fn buy_menu(&self) -> MenuRules {
        MenuRules {
            handguns: &HANDGUNS,
            random_pistols: &RANDOM_PISTOLS,
            random_pistols_from: 1,
            items_use_slots: false,
            items_need_teamplay: true,
            stealth_needs_teamplay: false,
            kevlar_cvar: false,
            akimbo_fixup: false,
            max_key: 10,
            text: Some(&BUY_TEXT),
            items_red_by_cash_only: true,
            category_rows_by_teamplay: true,
            hole_message: Some(HOLE_MESSAGE),
            summary_items_dim: true,
            refused_in_demo: false,
        }
    }

    // Audit round 2: A1

    /// `HUD_AddEntity` (`0x1007b4c0`) culls ground items by distance and the view cone as 3.0
    /// does but never sets their `rendermode`/`renderamt` (issue #244).
    fn ground_items_fade(&self) -> bool {
        false
    }

    /// [`spectator::add_entity_tail`]: HLTV viewers only.
    fn add_entity_spectator(&self) -> SpectatorTail {
        spectator::add_entity_tail(&HLTV.0.borrow(), &ts_client::spectator::EngineHltv)
    }

    /// [`effects::LASER`] (issue #204).
    fn laser_style(&self) -> &'static ts_client::effects::LaserStyle {
        &effects::LASER
    }

    /// [`effects::tsfrags`] (`0x10088020`, issue #232).
    fn tsfrags_effects(&self, ps: &mut PSys, now: f64, frags: &ts_client::effects::TsFrags) -> bool {
        effects::tsfrags(ps, now, frags);
        true
    }

    /// [`effects::blood_impact`] (`0x10082cd0`, issue #232).
    fn blood_impact(&self, ps: &mut PSys, now: f64, d: &ts_client::effects::PTakeDam) -> bool {
        effects::blood_impact(ps, now, d);
        true
    }

    // Audit round 2: A2

    /// The weapon element's `Draw` (0x10089d90) calls the list draw (0x1008af40) only while the
    /// menu is open (`+0x73c`), and that draw ends each row after `SPR_DrawAdditive` and `y +=
    /// 0x30`: no clip line, no clip bar. `SelectNextPrev` (0x1008cc40) opens the menu and then
    /// always reloads the selection (`+0x740 = info[0x85]`, `+0x744 = info[0x84]`), and with fast
    /// switch sends any id `> -1`.
    fn slot_menu(&self) -> SlotMenu {
        SlotMenu { closed_list: false, clip_info: false, next_prev_reloads: true, next_prev_sends_kungfu: true }
    }

    /// 2.1's `SelectSlot` (0x1008c6f0, [`hud::select_slot`]).
    fn select_slot(&self, h: &mut ts_client::hud::state::Hud, n: i32, fastswitch: bool) -> Option<Option<String>> {
        Some(hud::select_slot(h, n, fastswitch))
    }

    /// `Spectator` (0x100b28c0): 2.1 has no `cl_thirdperson`; whether entering or leaving
    /// observer mode, `if (CL_IsThirdPerson()) ThirdPersonToggle()` (0x1008f090), so a respawn is
    /// always in first person.
    fn spectator_toggles_third_person(&self, _observer: bool, third_person: bool, _cl_thirdperson: f32) -> bool {
        third_person
    }

    /// `AddTSMessage` (0x100873d0, from `TSMessage` 0x100882e0): the text copied into a 64-byte
    /// buffer (`[63] = 0`), no `LookupString` or local-name grey, bottom messages at `h − 0x20`,
    /// x by 6 pixels a character (centred `w/2 + (strlen >> 1) × −6`, right `w − strlen × 6 −
    /// 32`), expiring at `(float)t / 10.0 + time`.
    fn ts_message_layout(&self) -> TsMessageLayout {
        TsMessageLayout { lookup_and_highlight: false, max_chars: 63, bottom_margin: 0x20, char_width: Some(6), expiry_double: true }
    }

    /// `DrawPowerup` (0x10085c00): the names hard-coded (0x10271690, 0x10271680, 0x10271670,
    /// 0x1027165c).
    fn powerup_label(&self, flag: u32) -> Option<&'static str> {
        hud::powerup_label(flag)
    }

    /// The scoreboard's status column (0x100a4670): award 2 "KILLER", 3 "DEMOLITION"
    /// (0x1027549c, 0x1027548c); the others are 3.0's.
    fn award_title(&self, award: u8) -> Option<&'static str> {
        hud::award_title(award)
    }

    /// `TSSlowMo` (0x100881e0): `prev = s; s = coord / 100; if (prev == 0 && s != 0)` play: the
    /// test is on the raw factor as sent, not the eased scale.
    fn go_slow_sound(&self, _time_scale: f32, _target: f32, prev_factor100: f32, factor100: f32) -> bool {
        prev_factor100 / 100.0 == 0.0 && factor100 / 100.0 != 0.0
    }

    /// `V_CalcNormalRefdef`'s camera offsets (0x100b7700..0x100b7ab9, [`view::camera_offsets`]):
    /// the SDK cvars, no `cl_camtype`; integer offsets stepped by `ft × 100`.
    fn camera_offsets(&self, ofs: &mut ts_client::view::CamOffsets, c: &ts_client::view::CamInput) -> (f32, f32, f32) {
        view::camera_offsets(ofs, c)
    }

    /// `V_CalcViewRoll` (0x100b6910, [`view::view_roll`]): added at once, 0.5 or 30 in a dive.
    fn view_roll(
        &self,
        _roll: &mut f32,
        p: &mut goldsrc::sys::ref_params_t,
        view: Option<ts_client::view::RollEntity>,
        local: Option<ts_client::view::RollEntity>,
    ) {
        view::view_roll(p, view, local)
    }

    /// False: 2.1's `V_CalcNormalRefdef` never writes the local entity's `rendermode` or
    /// `renderamt`, and it has no `cl_thirdtransparency`.
    fn local_translucency(&self) -> bool {
        false
    }

    // Audit round 3: render

    /// [`studio::STYLE`].
    fn studio_style(&self) -> ts_client::studio::StudioStyle {
        studio::STYLE
    }

    // Audit round 3: hud

    /// [`hud::SAY_TEXT`] (issue #260).
    fn say_text_style(&self) -> SayTextStyle {
        hud::SAY_TEXT
    }

    /// 2.1's particle system (`0x10091020`, `0x100902f0`, `0x10090a00`; issue #261).
    fn particle_model(&self) -> ParticleModel {
        ParticleModel::Decelerate
    }

    // Audit round 4: render

    /// None: 2.1's `client.dll` imports nothing from `opengl32.dll` and has no `gl_fog` cvars
    /// or `ts_fog` (issue #281).
    fn gl_fog(&self) -> bool {
        false
    }

    // Audit round 4: unify

    /// TS 2.1's `client.dll` is an unoptimised build: the source's divisions, and every `float`
    /// assignment stored. The HUD message (`0x10054d90`/`0x10055800`), `DrawTSMessages`
    /// (`0x10086ec0`); the VGUI `XRES`/`YRES` `ScreenWidth / 640.0f`, `ScreenHeight / 480.0f`
    /// (`fdiv DWORD 0x1026f548` / `0x1026f544`), no spills (team menu 0x100b2ca0, message window
    /// 0x100a0ad0, scoreboard 0x100a2d60, spectator bar 0x100b04d0; #266); the renderer's `/0.1`
    /// (`0x10064575`), `/0.2` (`0x1006527b`), `/255.0` (`0x10064e46`), `/3.0` (`0x10063d3f`)
    /// (#276, #283); the HLTV overview's `(90 − pitch) / 90` (0x1004c475, 0x1004cbc2), name-tag
    /// projections and `Length` (0x1004d40d, 0x10097fc0), `AngleMatrix` (0x10061ee0) (#271, #272).
    fn dll_build(&self) -> ts_client::gameplay::DllBuild {
        ts_client::gameplay::DllBuild::Debug
    }
}

/// The 2.1 buy menu's text, hard-coded in `client.dll` (the port draws title keys, so each key
/// gets 2.1's words; the menu's numbers and `%i`s are the same): the team page (0x1006b6a0),
/// summary (0x1006b9a0: "(Total: %i)", "Remain Cash : %i (Total: %i)", "Free Slots : %i"),
/// attachment summary (0x1006b830), save and load (0x1006c540), main page (0x1006ca30, "7 Buy
/// Random" where 3.0 says "Random"), categories (`Init` 0x1006aa70, 0x1006cde0), options
/// (0x1006d470), weapon box (0x1006ddd0) and the messages (0x1006e830, 0x1006ff20).
pub static BUY_TEXT: [(&str, &str); 47] = [
    ("Chg_ChangeTeam", "Change Team:"),
    ("Chg_Join", "Join"),
    ("Chg_Cancel", "Cancel"),
    ("Buy_Total", "(Total: %i)"),
    ("Buy_Total_CashBased", "Remain Cash : %i (Total: %i)"),
    ("Buy_FreeSlots", "Free Slots : %i"),
    ("Buy_Credits", "%i Credits"),
    ("Buy_Slots", "%i Slots"),
    ("Buy_Sil", "Sil"),
    ("Buy_Las", "Las"),
    ("Buy_Fla", "Fla"),
    ("Buy_Sco", "Sco"),
    ("Buy_SaveEquipment", "Save Equipment"),
    ("Buy_LoadEquipment", "Load Equipment"),
    ("Buy_SlotEmpty", "Slot #%i (Empty)"),
    ("Buy_SlotAvailable", "Slot #%i (Available)"),
    ("Buy_GoBack", "Go Back"),
    ("Buy_EmptyConfig", "Empty Config"),
    ("Buy_UseLastConfig", "Use Last Config"),
    ("Buy_UseNewConfig", "Use New Config"),
    ("Buy_Buy", "Buy"),
    ("Buy_RemoveItem", "Remove Item"),
    ("Buy_ResetConfig", "Reset Config"),
    ("Buy_SaveConfig", "Save Config"),
    ("Buy_LoadConfig", "Load Config"),
    ("Buy_Random", "Buy Random"),
    ("Buy_Cancel", "Cancel"),
    ("Buy_Handguns", "Handguns"),
    ("Buy_SMGs", "SMGs"),
    ("Buy_Rifles", "Rifles"),
    ("Buy_Shotguns", "Shotguns"),
    ("Buy_SpecialPurpose", "Special Purpose"),
    ("Buy_PlayerAccessory", "Player Accessory"),
    ("Buy_Akimbo", "Buy Akimbo"),
    ("Buy_FullLoad", "Full Load"),
    ("HUD_Silencer", "Silencer"),
    ("HUD_Lasersight", "Lasersight"),
    ("HUD_Flashlight", "Flashlight"),
    ("HUD_Scope", "Scope"),
    ("Buy_ConfigReset", "Config Reset"),
    ("Buy_SavedConfig", "Saved Config on slot #%i"),
    ("Buy_ConfigLoaded", "Config #%i Loaded"),
    ("Buy_Removed", "%s Removed"),
    ("Buy_ConfigEmpty", "Your config is empty!"),
    ("Buy_NoCash", "No cash left!"),
    ("Buy_UsingNewConfig", "Using new config."),
    ("Buy_UsingLastConfig", "Using last config."),
];

/// `FUN_1006e1b0`'s message (0x10270cdc) when a click finds no row (a full remove list).
pub const HOLE_MESSAGE: &str = "A hole in the menu! TELL VINO IMMEDIATELY!";

#[cfg(test)]
mod tests;
