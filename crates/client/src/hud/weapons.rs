//! `CHudTSWeapons` (0x100d0ca0): weapon info, weapon list / slot menu, kung-fu bar, crosshair,
//! wire panel, slot selection and the Think sounds. Reference: architecture-hud.md §4.2.

use super::Dc;
use super::draw::{RED, ftol, half, num};
use super::state::{GRID_ROWS, Hud};
use super::text::localise;
use crate::input;
use goldsrc::client::engine;
use goldsrc::ext::hud::SND_CHANGE_VOL;
use shared::weapons::tables;
use std::cell::Cell;

// ---------------------------------------------------------------------------------------------
// Slot selection (pure)

fn slot_id(h: &Hud, row: i32, bucket: i32) -> i32 {
    h.slot_id(row, bucket)
}

/// `SelectSlot(n)` (1004f220) after the text-menu and buy-menu checks. Returns a server command.
/// After the wire panel's keys a ruleset may run its own selection
/// ([`ClientRules::select_slot`](crate::gameplay::ClientRules::select_slot)).
pub fn select_slot(h: &mut Hud, n: i32, fastswitch: bool) -> Option<String> {
    if h.wires.iter().any(|&w| w != 0) {
        return (1..=3).contains(&n).then(|| format!("cutwire_{n}"));
    }
    if let Some(cmd) = crate::gameplay::hud_rules(h.gameplay).select_slot(h, n, fastswitch) {
        return cmd;
    }
    if n == 10 {
        let cmd = (!h.menu_open).then(|| {
            h.menu_row = 0;
            h.menu_bucket = 0;
            "weapon_0".to_owned()
        });
        h.menu_open = false;
        return cmd;
    }
    if !(1..6).contains(&n) {
        return None;
    }
    let last = GRID_ROWS as i32 - 1;
    if !fastswitch {
        h.menu_open = true;
        if n == h.menu_bucket {
            // Cycle down the bucket.
            let mut row = h.menu_row + 1;
            if row > last {
                row = 1;
            }
            let found = (row..=last).find(|&r| slot_id(h, r, n) != -1);
            match found {
                Some(r) => h.menu_row = r,
                None => {
                    h.menu_row = 1;
                    if slot_id(h, 1, n) == -1
                        && let Some(r) = (2..=last).find(|&r| slot_id(h, r, n) != -1)
                    {
                        h.menu_row = r;
                    }
                }
            }
        } else {
            h.menu_row = 0;
            if slot_id(h, 0, n) == -1 {
                h.menu_row = (1..=last).find(|&r| slot_id(h, r, n) != -1).unwrap_or(1);
            }
        }
        h.menu_bucket = n;
        return None;
    }
    // hud_fastswitch: pick the next weapon in the bucket and select it at once.
    let start = if h.menu_row + 1 > last { 0 } else { h.menu_row + 1 };
    let mut row = start;
    loop {
        if slot_id(h, row, n) != -1 {
            break;
        }
        row = if row + 1 > last { 0 } else { row + 1 };
        if row == start {
            break;
        }
    }
    h.menu_row = row;
    h.menu_open = false;
    let id = slot_id(h, row, n);
    (id >= 0).then(|| format!("weapon_{id}"))
}

/// `SelectNextPrev(dir)` (1004f500): walk the grid, skipping single guns whose akimbo version
/// is owned. With fast switch the weapon is selected immediately. The ruleset's
/// [`SlotMenu`](crate::gameplay::SlotMenu) says whether the selection reloads from the held
/// weapon on every press and whether fast switch sends kung fu.
pub fn select_next_prev(h: &mut Hud, next: bool, fastswitch: bool) -> Option<String> {
    let style = crate::gameplay::hud_rules(h.gameplay).slot_menu();
    select_next_prev_as(h, next, fastswitch, style)
}

fn select_next_prev_as(h: &mut Hud, next: bool, fastswitch: bool, style: crate::gameplay::SlotMenu) -> Option<String> {
    let last = GRID_ROWS as i32 - 1;
    if !h.menu_open || style.next_prev_reloads {
        if let Some((r, b)) = super::state::grid_pos(h.weapon) {
            h.menu_row = r as i32;
            h.menu_bucket = b as i32;
        }
        h.menu_open = true;
    }
    if !(1..=5).contains(&h.menu_bucket) {
        h.menu_bucket = 1;
    }
    if !(0..=last).contains(&h.menu_row) {
        h.menu_row = 0;
    }
    let step = |row: &mut i32, b: &mut i32, first_bucket: &mut i32| {
        if next {
            *row += 1;
            if *row > last {
                *row = 0;
                if *first_bucket == -1 {
                    *first_bucket = *b;
                }
                *b = if *b + 1 > 5 { 1 } else { *b + 1 };
            }
        } else {
            *row -= 1;
            if *row < 0 {
                *row = last;
                if *first_bucket == -1 {
                    *first_bucket = *b;
                }
                *b = if *b - 1 < 1 { 5 } else { *b - 1 };
            }
        }
    };
    let (orig_row, mut first_bucket) = (h.menu_row, -1);
    let mut row = if next {
        if h.menu_row + 1 > last { 0 } else { h.menu_row + 1 }
    } else if h.menu_row - 1 < 0 {
        last
    } else {
        h.menu_row - 1
    };
    let mut b = h.menu_bucket;
    loop {
        let id = slot_id(h, row, b);
        if id > 0 {
            let akimbo = tables().weapon(id as u8).map_or(-1, |w| w.akimbo);
            if akimbo == -1 {
                break;
            }
            let owned_akimbo = super::state::grid_pos(akimbo).is_some_and(|(r, c)| h.grid[r][c].id >= 0);
            if !owned_akimbo {
                break;
            }
        }
        step(&mut row, &mut b, &mut first_bucket);
        if b == first_bucket && row == orig_row {
            break;
        }
    }
    h.menu_bucket = b;
    h.menu_row = row;
    if fastswitch {
        h.menu_open = false;
        let id = slot_id(h, row, b);
        let first = if style.next_prev_sends_kungfu { 0 } else { 1 };
        return (id >= first).then(|| format!("weapon_{id}"));
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Think sounds

/// Heartbeat (slow motion) / breathing (low kung-fu power) state: 0 idle, 2 playing, 1 fading.
static HEART: goldsrc::Global<Cell<(i32, f32)>> = goldsrc::Global(Cell::new((0, 0.0)));
static BREATH: goldsrc::Global<Cell<(i32, f32)>> = goldsrc::Global(Cell::new((0, 0.0)));

/// `CHudTSWeapons::VidInit` (`0x1004cb50`) zeroes both sounds' state and volume
/// (`+0x3708..+0x3714`), so a sound playing or fading is no longer updated, and the next cause
/// starts it afresh.
pub fn reset_think_sounds() {
    HEART.0.set((0, 0.0));
    BREATH.0.set((0, 0.0));
}

/// `Think` (1004ce10) sound part (`0x1004cf8b..`): `sound/player/heartbeat1.wav` (channel 501)
/// while the time scale is below 1, `sound/player/breathe2.wav` (502, volume 0.7) while kung-fu
/// power is below 50. Starting (or restarting while fading) plays it, with `SND_CHANGE_VOL` when
/// fading; once the cause ends it fades by 0.01 / 0.005 a frame with `SND_CHANGE_VOL` and stops
/// being updated at 0 (no last call). Every call's pitch is `RandomLong(-5, 5) + 25 +
/// trunc(75 * timescale)` (`25 - __ftol(timescale * -75)`), attenuation 0.8, on the local player.
/// The sample names keep TS's `sound/` prefix.
pub fn think_sounds(_dt: f32) {
    let e = engine();
    let (ts, power) = (crate::state::with(|s| s.time_scale), super::with(|h| h.kfu_power));
    let play = |ch: i32, sample: &str, vol: f32, flags: i32| {
        let r = goldsrc::ext::render::random_long(-5, 5);
        // `__ftol` and C int arithmetic: a wild time scale wraps instead of overflowing.
        let pitch = r.wrapping_add(25i32.wrapping_sub((ts as f64 * -75.0) as i32));
        if let Some(me) = e.local_player() {
            let (index, origin) = (me.index, me.curstate.origin);
            e.ev_play_sound(index, origin, ch, sample, vol, 0.8, flags, pitch);
        }
    };
    let run = |cell: &Cell<(i32, f32)>, active: bool, start_vol: f32, fade: f64, ch: i32, sample: &str| {
        let (mut st, mut vol) = cell.get();
        if !active {
            if st == 2 {
                st = 1;
            }
        } else if st == 0 || st == 1 {
            vol = start_vol;
            play(ch, sample, vol, if st == 1 { SND_CHANGE_VOL } else { 0 });
            st = 2;
        }
        if st == 1 {
            // `fsubl` of a double constant, stored back as a float.
            vol = (vol as f64 - fade) as f32;
            if vol <= 0.0 {
                st = 0;
            } else {
                play(ch, sample, vol, SND_CHANGE_VOL);
            }
        }
        cell.set((st, vol));
    };
    run(&HEART.0, ts < 1.0, 1.0, 0.01, 501, "sound/player/heartbeat1.wav");
    run(&BREATH.0, power < 50.0, 0.7, 0.005, 502, "sound/player/breathe2.wav");
}

// ---------------------------------------------------------------------------------------------
// Drawing

/// `CHudTSWeapons::Draw` (1004d3f0) minus the buy menu (drawn by `menus`).
pub fn draw(dc: &Dc, nohud: bool) {
    let hidden = super::dead_or_observer();
    let items = super::with(|h| items_flags(h, nohud, hidden));
    // `DrawLaserDots` runs first and reads `+0x14` as the previous `Draw` left it; `Draw` then
    // writes this frame's value.
    let previous = crate::state::with(|s| std::mem::replace(&mut s.hud_items, items));
    draw_laser_dots(dc, previous);
    if nohud {
        return;
    }
    crate::menus::draw_buy_menu(dc);
    if hidden {
        return;
    }
    let buy_open = crate::menus::buy_open();
    // Port addition: the client extensions' HUD control (issue #314, `super::elements`).
    use super::elements::draw as element;
    use ts_client_ext_api::hud as el;
    super::with(|h| {
        let style = crate::gameplay::hud_rules(h.gameplay).slot_menu();
        if style.closed_list {
            let demo = engine().demo_playing();
            element(el::WEAPON_SELECT, || draw_weapon_list(dc, h, !h.menu_open || demo, true));
        } else if h.menu_open {
            element(el::WEAPON_SELECT, || draw_weapon_list(dc, h, false, style.clip_info));
        }
        if h.weapon == 0 {
            element(el::KUNGFU, || draw_kungfu_label(dc, h));
        } else {
            element(el::AMMO, || draw_weapon_info(dc, h));
        }
        if buy_open {
            return;
        }
        if h.wires.iter().any(|&w| w != 0) {
            element(el::CROSSHAIR, || draw_wire_panel(dc, h));
        } else {
            element(el::CROSSHAIR, || draw_crosshair(dc, h));
        }
    });
}

/// `CHudTSWeapons+0x14` as `Draw` sets it: the local player's `ActItems` bits while a weapon is
/// held and the HUD draws it (with `nohud`, whenever a weapon is held), otherwise 0. TS sets it
/// after drawing the dots, so the dots use last frame's value, as here.
fn items_flags(h: &Hud, nohud: bool, hidden: bool) -> i32 {
    if h.weapon == 0 || (!nohud && hidden) {
        return 0;
    }
    crate::state::with(|s| s.active_items.get(engine().local_player().map_or(0, |p| p.index as usize)).copied().unwrap_or(0)) as i32
}

/// The crosshair centre: the screen centre, or in third person the projection of the view
/// trace's end (`DrawCrosshairID` 1004acc0 stores it at `+0x38`), (−1, −1) when off screen.
fn crosshair_centre(dc: &Dc) -> (i32, i32) {
    if !crate::view::is_third_person() {
        return (dc.w() / 2, dc.h() / 2);
    }
    let e = engine();
    let Some(me) = e.local_player() else { return (-1, -1) };
    let start = crate::view::eye();
    let end = goldsrc::ext::render::vec3::ma(start, 8192.0, crate::view::camera().forward);
    e.crosshair_trace(start, end, me.index).and_then(|(_, hit, _)| e.world_to_screen(hit, &dc.g.screen)).unwrap_or((-1, -1))
}

/// `DrawLaserDots` (1004e6f0; 2.1 0x1008b410): 2×2 dots, coloured by [`laser_dot_paint`]. While
/// the gun may fire (`state.items_ready`) and `cl_laser` is below 1, the dot sits on the
/// crosshair (akimbo: 4 px right and left of it) with a rare ±1 px jitter; otherwise it is drawn
/// where the trace hit (`state.laser_dots`). A dot whose hit point is off screen is not drawn.
fn draw_laser_dots(dc: &Dc, items: i32) {
    use goldsrc::ext::render::random_long;
    let local = engine().local_player().map_or(0, |p| p.index as usize);
    let (dots, dists, ready, color) =
        crate::state::with(|s| (s.laser_dots, s.laser_dist, s.items_ready, s.laser_color.get(local).copied().unwrap_or(0)));
    let rgb = crate::effects::laser_color(color as i32).map(|c| (c * 255.0).round() as i32);
    let range = crate::gameplay::current().laser_style().dot_range;
    // C integer division: ±1 only for ±10.
    let (jx, jy) = (random_long(-10, 10) / 10, random_long(-10, 10) / 10);
    let centre = crosshair_centre(dc);
    let centred = ready != 0 && input::cvars().cl_laser.value() < 1.0;
    for (gun, x, y) in laser_dot_positions(items, centred, centre, (jx, jy), dots, (dc.w(), dc.h())) {
        if let Some((rgb, a)) = laser_dot_paint(range, gun, dists[gun], rgb) {
            dc.fill(x, y, 2, 2, rgb, a);
        }
    }
}

/// A laser dot's colour and alpha, `None` for no dot ([`LaserStyle::dot_range`]
/// (crate::effects::LaserStyle::dot_range)). TS 3.0: the `PlInfo` colour `rgb` at 255. With a
/// range `r` (2.1 `0x1008b4c7..0x1008b53a`): only while the hit's distance `d < r` (a NaN draws
/// nothing), red, the first gun's alpha `ftol((r − d) / r × 255)`, the second's 255.
fn laser_dot_paint(range: Option<f32>, gun: usize, d: f32, rgb: [i32; 3]) -> Option<([i32; 3], i32)> {
    let Some(r) = range else { return Some((rgb, 255)) };
    if d.partial_cmp(&r) != Some(std::cmp::Ordering::Less) {
        return None;
    }
    let a = if gun == 0 { crate::hud::draw::ftol((r as f64 - d as f64) / r as f64 * 255.0) } else { 255 };
    Some(([255, 0, 0], a))
}

/// The gun (0, or 1 the akimbo second) and top-left corner of each `DrawLaserDots` fill (pure,
/// for tests). `dots` are `WorldToScreen` coordinates; `centred` pins them to `centre` (plus
/// `jitter`, swapped for the second gun).
fn laser_dot_positions(
    items: i32,
    centred: bool,
    (cx, cy): (i32, i32),
    (jx, jy): (i32, i32),
    dots: [Option<[f32; 2]>; 2],
    (w, h): (i32, i32),
) -> Vec<(usize, i32, i32)> {
    if items & 2 == 0 {
        return Vec::new();
    }
    // `WorldToScreen` (`0x100579c0`, 2.1 `0x10097650`): `ftol((x + 1) × w × 0.5)` on the x87,
    // unrounded.
    let to_px = |d: [f32; 2]| (ftol((d[0] as f64 + 1.0) * w as f64 * 0.5), ftol((1.0 - d[1] as f64) * h as f64 * 0.5));
    let pinned: &[(i32, i32)] = if items & 0x40 == 0 { &[(cx + jx, cy + jy)] } else { &[(cx + 4 + jx, cy + jy), (cx - 4 + jy, cy + jx)] };
    // A hit point projected from near the eye plane can saturate the cast; C would wrap.
    pinned
        .iter()
        .zip(dots)
        .enumerate()
        .filter_map(|(gun, (&p, d))| d.map(|d| (gun, if centred { p } else { to_px(d) })))
        .map(|(gun, (x, y))| (gun, x.wrapping_sub(1), y.wrapping_sub(1)))
        .collect()
}

fn realhud() -> bool {
    input::cvars().cl_realhud.value() != 0.0
}

/// `0x1007d770`, the double 2/3 most of the weapon box's text and numbers are scaled by.
const TWO_THIRDS: f64 = 0.666_666_666_666_666_6;

/// `ftol(c × k)` per channel, as TS's `fild; fmul; __ftol` sequences compute it.
fn scaled(c: [i32; 3], k: f64) -> [i32; 3] {
    c.map(|v| ftol(v as f64 * k))
}

/// `ScaleColors` (`0x10021c60`): `ftol(c × (a × (1/255)f))`.
fn scale_colors(c: [i32; 3], a: i32) -> [i32; 3] {
    let k = a as f64 * (1.0f32 / 255.0) as f64;
    scaled(c, k)
}

/// `DrawKungFuBar` (1004d5f0): 128×19 frame plus a fill proportional to `KFuPower`:
/// `p = ftol(power × 1.28f)` (`0x1007d78c`), `FillRGBA(x+2, y+2, p−2, 16, r·128/255, p·g/255,
/// p·b/255, 255−p)`.
fn draw_kungfu_bar(dc: &Dc, h: &Hud, x: i32, y: i32) {
    let mut rgb = dc.hud;
    if crate::gameplay::hud_rules(h.gameplay).kungfu_exhausted(h.kfu_power) {
        rgb = RED;
    } else {
        let p = ftol(h.kfu_power as f64 * 1.28f32 as f64);
        // Guarded: below 2 TS passes a negative width; the port draws nothing there.
        let fill = [rgb[0].wrapping_mul(128) / 255, p.wrapping_mul(rgb[1]) / 255, p.wrapping_mul(rgb[2]) / 255];
        dc.fill(x + 2, y + 2, (p - 2).max(0), 16, fill, 255 - p);
    }
    dc.fill(x, y, 128, 19, rgb, 30);
}

/// `DrawKungFuLabel` (1004d700): "Kung-Fu" in the HUD colour × 2/3; or the ruleset's own
/// ([`ClientRules::kungfu_label`](crate::gameplay::ClientRules::kungfu_label)).
fn draw_kungfu_label(dc: &Dc, h: &Hud) {
    let (w, ht) = (dc.w(), dc.h());
    if let Some(ops) = crate::gameplay::hud_rules(h.gameplay).kungfu_label(h.kfu_power, dc.hud, w, ht) {
        dc.ops(&ops);
        return;
    }
    let x = w - 136;
    draw_kungfu_bar(dc, h, x, ht - 28);
    dc.fill(x, ht - 48, 128, 19, dc.hud, 35);
    dc.string(w - 134, ht - 48, "Kung-Fu", scaled(dc.hud, TWO_THIRDS));
}

/// `DrawWeaponInfo`'s ammo numbers (`0x1004dac4..`): the HUD colour × 2/3, or red
/// (195, 45, 45) when the value is 0 or less.
fn ammo_rgb(hud: [i32; 3], n: i32) -> [i32; 3] {
    if n > 0 { scaled(hud, TWO_THIRDS) } else { RED }
}

const FIREMODE_LABELS: [&str; 6] = ["#HUD_FullAuto", "#HUD_SemiAuto", "#HUD_Burst", "#HUD_PumpAction", "#HUD_FreeSemi", "#HUD_FreeFull"];
const ATTACHMENT_LABELS: [&str; 4] = ["#HUD_Silencer", "#HUD_Lasersight", "#HUD_Flashlight", "#HUD_Scope"];

/// `DrawWeaponInfo` (1004d7e0): bottom-right stack of 128×19 rows, bottom-up: owned attachments,
/// clip/reserve (or the kung-fu bar for melee), fire mode, ammo name, weapon name.
fn draw_weapon_info(dc: &Dc, h: &Hud) {
    if realhud() {
        return;
    }
    let Some(info) = tables().weapon(h.weapon as u8) else { return };
    let x = dc.w() - 136;
    let mut y = dc.h() - 8;
    let row = 20;
    let active =
        crate::state::with(|s| s.active_items.get(engine().local_player().map_or(0, |p| p.index as usize)).copied().unwrap_or(0)) as u32;
    let panel = super::draw::WeaponPanel {
        w: dc.w(),
        h: dc.h(),
        hud: dc.hud,
        owned: h.attachments,
        active,
        name: &info.name,
        ammo: info.ammo,
        stack: info.stack,
        ammo_name: tables().ammo(info.ammo).map_or("", |a| a.name.as_str()),
        clip: h.clip,
        reserve: h.reserve[info.ammo as usize & 15],
        firemode: h.firemode,
        fov: crate::state::with(|s| s.fov) as i32,
    };
    if let Some(ops) = crate::gameplay::hud_rules(h.gameplay).weapon_panel(&panel) {
        dc.ops(&ops);
        return;
    }
    for (bit, label) in ATTACHMENT_LABELS.iter().enumerate() {
        let m = 1u32 << bit;
        if h.attachments & m == 0 {
            continue;
        }
        y -= row;
        let on = active & m != 0;
        dc.fill(x, y, 128, row - 1, dc.hud, if on { 45 } else { 15 });
        dc.string(x + 2, y, &localise(label), half(dc.hud));
        dc.spr(dc.g.ts.item_installed, x + 0x70, y, if on { dc.hud } else { half(dc.hud) });
    }
    y -= row;
    dc.fill(x, y, 128, row - 1, dc.hud, 15);
    // The server sends WeaponInfo only when the weapon or the reserve changes (CTSGun::
    // UpdateClientData); the prediction writes the clip and fire mode in between
    // (`Hud::predicted_weapon`, 0x100581c0). The reserve is WeaponInfo's alone.
    let (clip, reserve, firemode) = (h.clip, h.reserve[info.ammo as usize & 15], h.firemode as i32);
    // `ftol(x + 94.8148…)` (`0x1007d790`): the right-hand number column.
    let right = ftol(x as f64 + 94.814_814_814_814_81);
    let two_thirds = scaled(dc.hud, TWO_THIRDS);
    if info.ammo == 0 {
        draw_kungfu_bar(dc, h, x, y);
        y -= row;
        if info.stack {
            dc.number(right, y, num::DIGITS_3 | num::DRAW_ZERO, clip, ammo_rgb(dc.hud, clip));
        }
    } else {
        dc.number(x + 2, y, num::DIGITS_3 | num::DRAW_ZERO, clip, ammo_rgb(dc.hud, clip));
        dc.string(x + 0x40, y, "|", [dc.hud[0] / 4, dc.hud[1] / 4, dc.hud[2] / 4]);
        dc.number(right, y, num::DIGITS_3 | num::DRAW_ZERO, reserve, ammo_rgb(dc.hud, reserve));
        y -= row;
        dc.fill(x, y, 128, row - 1, dc.hud, 15);
        let label = FIREMODE_LABELS.get(firemode as usize).copied().unwrap_or(FIREMODE_LABELS[0]);
        dc.string(x + 2, y, &localise(label), two_thirds);
        y -= row;
        dc.fill(x, y, 128, row - 1, dc.hud, 15);
        let ammo = tables().ammo(info.ammo).map_or("", |a| a.name.as_str());
        dc.string(x + 2, y, ammo, two_thirds);
    }
    y -= row;
    dc.fill(x, y, 128, row - 1, dc.hud, 35);
    dc.string(x + 2, y, &info.name, two_thirds);
    // Zoom factor while scoped: `sprintf("%ix", 80 / m_iFOV)` at (24, 24) (1004dfbe).
    let fov = crate::state::with(|s| s.fov) as i32;
    if (1..40).contains(&fov) {
        dc.string(24, 24, &format!("{}x", 80 / fov), dc.hud);
    }
}

/// `DrawWeaponList` (1004e030): the slot menu (header row per bucket, weapon sprites, name and
/// `clip/clip size` of the selection, clip bar) or, with the menu closed, the equipment list
/// when `cl_showequipment` is 1..3. Every colour goes through `ScaleColors` (`0x10021c60`) and
/// then × `m`: 1 with the menu open, else 0.25, 0.5 or 1 for `cl_showequipment` 1, 2, 3
/// (`0x1004e03d..0x1004e07d`). Without `clip_info` the selection has no clip line and no
/// weapon has a clip bar (a ruleset's [`SlotMenu::clip_info`](crate::gameplay::SlotMenu)).
fn draw_weapon_list(dc: &Dc, h: &Hud, closed: bool, clip_info: bool) {
    let m: f64 = if closed {
        match ftol(input::cvars().cl_showequipment.value() as f64) {
            1 => 0.25,
            2 => 0.5,
            3 => 1.0,
            _ => return,
        }
    } else {
        1.0
    };
    let t = tables();
    for bucket in 1..=5i32 {
        let x = bucket * 128 - 120;
        if !closed {
            let sel = bucket == h.menu_bucket;
            dc.fill(x, 8, 128, 20, scaled(dc.hud, m), if sel { 100 } else { 50 });
            let rgb = scaled(scale_colors(dc.hud, if sel { 200 } else { 100 }), m);
            dc.number(x, 8, num::DIGITS_2, bucket, rgb);
        }
        let mut y = 28;
        for row in 0..GRID_ROWS {
            let slot = h.grid[row][bucket as usize];
            if slot.id == -1 {
                continue;
            }
            let id = slot.id;
            // Closed, kung fu isn't drawn but still takes its 48 px (`0x1004e207`).
            if closed && id == 0 {
                y += 48;
                continue;
            }
            let selected = !closed && row as i32 == h.menu_row && bucket == h.menu_bucket;
            if selected && let Some(info) = t.weapon(id as u8) {
                let rgb = scaled(scale_colors(dc.hud, 200), m);
                dc.string(x + 128, y, &info.name, rgb);
                // `0x1004e324`: `"%d/%d"` of the cell's clip and the weapon's clip size.
                let ammo = match id {
                    24 | 25 | 35 => Some(format!("{}", slot.clip)),
                    0 | 29 | 34 => None,
                    _ => Some(format!("{}/{}", slot.clip, info.clip)),
                };
                if let Some(a) = ammo.filter(|_| clip_info) {
                    dc.string(x + 128, y + 16, &a, rgb);
                }
            }
            let base = if slot.clip < 1 { RED } else { dc.hud };
            let rgb = scaled(scale_colors(base, if selected { 255 } else { 100 }), m);
            if let Some(&spr) = dc.g.ts.weapons.get(id as usize) {
                dc.spr(spr, x, y, rgb);
            }
            // Clip bar (`0x1004e534..`): 5 px wide at x+123, 44 px tall, in the sprite's
            // colour; the clip is the weapon's own grid cell's.
            if clip_info
                && !matches!(id, 0 | 29 | 34)
                && let Some(info) = t.weapon(id as u8)
            {
                let own = h.grid.get(info.hud[1] as usize).and_then(|r| r.get(info.hud[0] as usize)).map_or(0, |s| s.clip) as f32;
                let max = (if info.stack { info.pellets } else { info.clip }) as f32;
                let filled = ftol(own as f64 * 44.0 / max as f64).max(0);
                if max > 0.0 {
                    dc.fill(x + 0x7b, y - filled + 46, 5, filled, rgb, 100);
                }
                if own < max {
                    dc.fill(x + 0x7b, y + 2, 5, 44 - filled, rgb, 50);
                }
            }
            y += 48;
        }
    }
}

/// `DrawWirePanel` (1004d550): three wire sprites 32 px apart above the crosshair.
fn draw_wire_panel(dc: &Dc, h: &Hud) {
    let e = engine();
    let (mut x, y) = (dc.w() / 2 - 48, dc.h() / 2 - 128);
    for i in 0..3 {
        let spr = if h.wires[i] != 0 { dc.g.ts.wires[i] } else { dc.g.ts.wires_cut[i] };
        if spr != 0 {
            e.spr_set(spr, [255; 3]);
            e.spr_draw_holes(0, x, y, None);
        }
        x += 32;
    }
}

/// The scoped flag `DrawCrosshair` sets (`0x1018a704`): FOV below 40, cleared in third person, so
/// the third-person crosshair keeps its lines, divesteps and unraised laser number while zoomed.
fn crosshair_scoped(fov: f32, third_person: bool) -> bool {
    (fov as i32) < 40 && !third_person
}

/// `DrawCrosshair` (1004e9f0) + divestep helper (1004ef50). Everything is drawn around
/// [`crosshair_centre`]: the screen centre, or the projected view-trace end in third person.
/// Which parts show for the held weapon, and in what order, is the ruleset's
/// ([`ClientRules::crosshair_parts`](crate::gameplay::ClientRules::crosshair_parts)).
fn draw_crosshair(dc: &Dc, h: &Hud) {
    if realhud() {
        return;
    }
    let e = engine();
    let rules = crate::gameplay::hud_rules(h.gameplay);
    let fov = crate::state::with(|s| s.fov);
    let parts = rules.crosshair_parts(h.weapon, fov);
    let scoped = crosshair_scoped(fov, crate::view::is_third_person());
    let (cx, cy) = crosshair_centre(dc);
    let divesteps = parts.divesteps && !scoped;
    if divesteps && !parts.divesteps_last {
        // Its own element, though the crosshair draws it (issue #314).
        super::elements::draw(ts_client_ext_api::hud::STUNT, || draw_divesteps(dc, h, cx, cy));
    }
    if !parts.crosshair {
        return;
    }
    let id_team = h.crosshair_id.map(|(_, t)| t);
    // 1 = teammate under the crosshair (green, "don't shoot" sprite), 2 = enemy (red).
    let rgb = match id_team {
        Some(1) => [45, 195, 45],
        Some(2) => RED,
        _ => dc.hud,
    };
    let active = crate::state::with(|s| s.active_items.get(e.local_player().map_or(0, |p| p.index as usize)).copied().unwrap_or(0));
    if !scoped || id_team == Some(1) {
        let base = if active & 2 != 0 { 12 } else { 22 };
        let spread = crate::state::with(|s| s.spread);
        let accuracy = tables().weapon(h.weapon as u8).map_or(0.0, |w| w.accuracy);
        let gap = rules.crosshair_gap(base, spread, accuracy);
        if id_team == Some(1) {
            // `0x1004ebc2`: × 0.7.
            dc.spr(dc.g.ts.cross_t, cx - 32, cy - 32, scaled(rgb, 0.7));
        } else {
            dc.fill(cx - gap, cy - 1, 5, 2, rgb, 75);
            dc.fill(cx + gap - 5, cy - 1, 5, 2, rgb, 75);
            dc.fill(cx - 1, cy - gap, 2, 5, rgb, 75);
            dc.fill(cx - 1, cy + gap - 5, 2, 5, rgb, 75);
            let solid = input::cvars().cl_solidcross.value();
            if solid_cross(solid, goldsrc::ext::studio::hardware_kind() != 0, dc.g.ts.cross2 != 0) {
                e.spr_set(dc.g.ts.cross2, if solid > 1.0 { [0; 3] } else { rgb });
                e.spr_draw_holes(0, cx - 32, cy - 32, None);
            } else {
                // `0x1004ed93`: × 0.75.
                dc.spr(dc.g.ts.cross_e, cx - 32, cy - 32, scaled(rgb, 0.75));
            }
        }
    }
    // Laser on: its range (`state.laser_range`) right of the crosshair, in the crosshair colour
    // ×0.7 with a player under it, else ×0.5; 8 px higher while scoped (1004ee02).
    if active & 2 != 0 {
        let k = if id_team.is_some() { 0.7 } else { 0.5 };
        // `m_iFontHeight / 2` (`0x1004ee65`), the `number_0` height.
        let y = cy - if scoped { 8 } else { 0 } - dc.g.sprites.font_height() / 2;
        let range = crate::state::with(|s| s.laser_range);
        dc.number(cx + 28, y, num::DRAW_ZERO, range, scaled(rgb, k));
    }
    if divesteps && parts.divesteps_last {
        // Its own element, though the crosshair draws it (issue #314).
        super::elements::draw(ts_client_ext_api::hud::STUNT, || draw_divesteps(dc, h, cx, cy));
    }
}

/// `DrawCrosshair`'s enemy/neutral sprite choice (`0x1004ecf3..0x1004ed15`): the solid `cross2`
/// holes sprite only on a hardware renderer (`IEngineStudio.IsHardware()`) with `cl_solidcross > 0`;
/// a software renderer always gets the additive `cross_e` (issue #153). `loaded` (the sprite
/// handle) is the port's guard.
fn solid_cross(cl_solidcross: f32, hardware: bool, loaded: bool) -> bool {
    hardware && cl_solidcross > 0.0 && loaded
}

/// Divestep sprites around the crosshair (1004ef50): top/centre/bottom dive indicators plus a
/// pitch dot. See [`divestep_layout`].
fn draw_divesteps(dc: &Dc, h: &Hud, cx: i32, cy: i32) {
    let rules = crate::gameplay::hud_rules(h.gameplay);
    let cooldown = rules.stunt_cooldown(h.stunt_cooldown(), super::now());
    let blocked = rules.divesteps_blocked(81 - h.slots as i32, h.armor, h.gamemode);
    let (sprites, (dx, dy), dot) = divestep_layout(rules.dll_build(), cooldown, blocked, engine().view_angles()[0], dc.hud, (cx, cy));
    for (i, rgb) in sprites.into_iter().enumerate() {
        dc.spr(dc.g.ts.divestep[i], cx - 40, cy - 40, rgb);
    }
    dc.fill(dx, dy, 2, 2, dot, 100);
}

/// Colours of the three divestep sprites, and the pitch dot's position and colour (pure, from the
/// TS binary at 1004ef50 and the threshold helpers 1004eec0/1004eef0/1004ef20).
///
/// Sprite `i` is red when the load is too heavy for that dive (`blocked[i]`: `81 − free slots`
/// ≥ 42, 52, 62 in TS 3.0, [`ClientRules::divesteps_blocked`](crate::gameplay::ClientRules::divesteps_blocked)).
/// Otherwise it has the base colour (red while the stunt cooldown runs, else the HUD colour), at
/// full strength for the band the view pitch points at (< −10°, −10..10°, > 10°) and halved for
/// the other two. The dot sits 32 px from the centre at angle `clamp(3·pitch, ±90°)`, on the left:
/// `(cx − 32·cos a, cy + 32·sin a)`, in the base colour.
///
/// The pitch is stored as a float `(float)(pitch × 3.0)`, then the angle and the position are
/// worked out in double: 3.0 compares the unrounded product with 90 (`0x1004f180`), multiplies
/// by the double π/180 (`0x1004f1c0`) and uses the x87's `fsin`/`fcos`; 2.1 compares the
/// stored float (`0x1008c50d`) and takes `p / 180.0 × π` (`0x1008c571`) into the CRT's
/// `sin`/`cos`.
fn divestep_layout(
    build: crate::gameplay::DllBuild,
    cooldown: bool,
    blocked: [bool; 3],
    pitch: f32,
    hud: [i32; 3],
    (cx, cy): (i32, i32),
) -> ([[i32; 3]; 3], (i32, i32), [i32; 3]) {
    let base = if cooldown { RED } else { hud };
    let band = if pitch < -10.0 {
        0
    } else if pitch > 10.0 {
        2
    } else {
        1
    };
    let sprites = [0, 1, 2].map(|i| {
        if blocked[i] {
            RED
        } else if i == band {
            base
        } else {
            half(base)
        }
    });
    let scaled = pitch as f64 * 3.0;
    let mut p = scaled as f32;
    let over = match build {
        crate::gameplay::DllBuild::Optimised => scaled > 90.0,
        crate::gameplay::DllBuild::Debug => p > 90.0,
    };
    if over {
        p = 90.0;
    }
    if p < -90.0 {
        p = -90.0;
    }
    let a = match build {
        crate::gameplay::DllBuild::Optimised => p as f64 * (std::f64::consts::PI / 180.0),
        crate::gameplay::DllBuild::Debug => p as f64 / 180.0 * std::f64::consts::PI,
    };
    let dot = (ftol(cx as f64 - a.cos() * 32.0), ftol(a.sin() * 32.0 + cy as f64));
    (sprites, dot, base)
}

#[cfg(test)]
mod tests {
    use super::super::state::{Slot, grid_pos};
    use super::*;
    use crate::gameplay::DllBuild;

    #[test]
    fn solid_cross_needs_a_hardware_renderer() {
        assert!(solid_cross(1.0, true, true));
        assert!(solid_cross(2.0, true, true));
        assert!(!solid_cross(0.0, true, true));
        assert!(!solid_cross(1.0, false, true), "software: cl_solidcross is ignored");
        assert!(!solid_cross(f32::NAN, true, true));
    }

    #[test]
    fn colour_scaling_follows_ts() {
        // 2/3 of the default HUD colour (`0x1007d770`), and red for an empty clip.
        assert_eq!(scaled([255, 160, 0], TWO_THIRDS), [170, 106, 0]);
        assert_eq!(ammo_rgb([255, 160, 0], 0), RED);
        assert_eq!(ammo_rgb([255, 160, 0], 7), [170, 106, 0]);
        // `ScaleColors` with the slot menu's 100 / 200 / 255.
        assert_eq!(scale_colors([255, 160, 0], 255), [255, 160, 0]);
        assert_eq!(scale_colors([255, 160, 0], 200), [200, 125, 0]);
        assert_eq!(scale_colors([255, 160, 0], 100), [100, 62, 0]);
        // `DrawKungFuBar`: full power is ftol(100 × 1.28f) = 127, not 128.
        assert_eq!(ftol(100.0f32 as f64 * 1.28f32 as f64), 127);
        assert_eq!(ftol(50.0f32 as f64 * 1.28f32 as f64), 63);
    }

    fn give(h: &mut Hud, id: i32) {
        let (r, b) = grid_pos(id).unwrap();
        h.grid[r][b] = Slot { id, clip: 5 };
    }

    #[test]
    fn crosshair_scoped_flag() {
        assert!(crosshair_scoped(20.0, false));
        assert!(!crosshair_scoped(40.0, false), "40 is not scoped");
        assert!(crosshair_scoped(39.9, false), "the FOV is truncated, as TS's int");
        assert!(!crosshair_scoped(20.0, true), "third person clears it");
        assert!(!crosshair_scoped(90.0, false));
    }

    /// TS 3.0's divestep limits ([`crate::gameplay::ClientRules::divesteps_blocked`]).
    #[test]
    fn divesteps_follow_ts_binary() {
        let hud = [100, 200, 60];
        let c = (320, 240);
        // Level view, light load: centre band full, the others halved; dot 32 px left.
        let (s, dot, base) = divestep_layout(DllBuild::Optimised, false, [false; 3], 0.0, hud, c);
        assert_eq!(s, [half(hud), hud, half(hud)]);
        assert_eq!((dot, base), ((288, 240), hud));
        // Looking down 30° (clamped to 90°): bottom band, dot straight below.
        let (s, dot, _) = divestep_layout(DllBuild::Optimised, false, [false; 3], 30.0, hud, c);
        assert_eq!((s[2], dot), (hud, (320, 272)));
        // Top and centre dives too heavy; cooldown turns the rest red.
        let (s, _, base) = divestep_layout(DllBuild::Optimised, true, [true, true, false], -20.0, hud, c);
        assert_eq!((s, base), ([RED, RED, half(RED)], RED));
        // Just under 90°: 32·sin a is 31.99999..., truncated to 31 in double, where a float
        // sine rounds it to 32 (`0x1004f1db`, 2.1 `0x1008c583`).
        let p = f32::from_bits(0x41ef_f9ae);
        assert_eq!(divestep_layout(DllBuild::Optimised, false, [false; 3], p, hud, c).1, (319, 271));
        assert_eq!(divestep_layout(DllBuild::Debug, false, [false; 3], p, hud, c).1, (319, 271));
    }

    #[test]
    fn laser_dots_follow_ts_layout() {
        let dots = [Some([0.5, 0.5]), Some([-0.5, 0.0])];
        let (c, j, sz) = ((320, 240), (1, 0), (640, 480));
        assert_eq!(laser_dot_positions(0, true, c, j, dots, sz), [], "laser off");
        assert_eq!(laser_dot_positions(2, true, c, j, dots, sz), [(0, 320, 239)], "pinned to the crosshair, jitter x");
        assert_eq!(laser_dot_positions(2, false, c, j, dots, sz), [(0, 479, 119)], "at the projected hit");
        assert_eq!(laser_dot_positions(0x42, true, c, j, dots, sz), [(0, 324, 239), (1, 315, 240)], "akimbo: +3/-5 px, jitter swapped");
        assert_eq!(laser_dot_positions(0x42, true, c, j, [None, dots[1]], sz), [(1, 315, 240)], "off-screen hit: no dot");
        // `(x + 1) × w × 0.5` = 385.99999 unrounded (`0x100579c0`); a float sum rounds it to 386.
        let x = f32::from_bits(0x3e53_3331);
        assert_eq!(laser_dot_positions(2, false, c, j, [Some([x, 0.0]), None], sz), [(0, 384, 239)]);
    }

    /// 3.0 (`0x1004e6f0`): the laser colour at 255 at any range. 2.1 (`0x1008b410`, issue #204):
    /// red, faded out by 4096 units for the first gun, 255 for the second, none from 4096.
    #[test]
    fn laser_dot_paint_by_range() {
        let green = [0, 205, 0];
        assert_eq!(laser_dot_paint(None, 0, 9000.0, green), Some((green, 255)));
        assert_eq!(laser_dot_paint(Some(4096.0), 0, 0.0, green), Some(([255, 0, 0], 255)));
        assert_eq!(laser_dot_paint(Some(4096.0), 0, 1024.0, green), Some(([255, 0, 0], 191)));
        assert_eq!(laser_dot_paint(Some(4096.0), 0, 4095.0, green), Some(([255, 0, 0], 0)));
        assert_eq!(laser_dot_paint(Some(4096.0), 1, 4000.0, green), Some(([255, 0, 0], 255)));
        assert_eq!(laser_dot_paint(Some(4096.0), 0, 4096.0, green), None);
        assert_eq!(laser_dot_paint(Some(4096.0), 1, f32::NAN, green), None);
    }

    /// Projections from near the eye plane (infinite or NaN) and odd screens give no overflow.
    #[test]
    fn laser_dots_with_wild_projections() {
        let wild = [Some([f32::INFINITY, f32::NEG_INFINITY]), Some([f32::NAN, -f32::MAX])];
        for items in [2, 0x42] {
            for size in [(i32::MAX, i32::MAX), (i32::MIN, i32::MIN), (0, 0)] {
                let _ = laser_dot_positions(items, false, (0, 0), (0, 0), wild, size);
            }
        }
        for blocked in [[false; 3], [true; 3]] {
            let _ = divestep_layout(DllBuild::Debug, true, blocked, f32::NAN, [0; 3], (i32::MAX, i32::MIN));
        }
        assert!(!crosshair_scoped(f32::NAN, true));
    }

    /// Slot selection with every bucket and weapon ids the server may have put in the grid
    /// (255, unknown akimbo links) always ends.
    #[test]
    fn slot_selection_with_any_grid() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud::default();
        for (r, row) in h.grid.iter_mut().enumerate() {
            for (b, s) in row.iter_mut().enumerate() {
                *s = Slot { id: ((r * 11 + b) * 7 % 256) as i32, clip: -5 };
            }
        }
        for n in [-1, 0, 1, 5, 6, 10, 11, i32::MIN, i32::MAX] {
            for fast in [false, true] {
                let _ = select_slot(&mut h, n, fast);
                let _ = select_next_prev(&mut h, n % 2 == 0, fast);
                assert!((0..GRID_ROWS as i32).contains(&h.menu_row));
            }
        }
        h.grid = [[super::super::state::EMPTY; super::super::state::GRID_BUCKETS]; GRID_ROWS];
        assert_eq!(select_next_prev(&mut h, true, true), None, "an empty grid ends the walk");
        assert_eq!(select_slot(&mut h, 3, true), None);
    }

    #[test]
    fn slot_menu_cycles() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud::default();
        give(&mut h, 1); // glock [2,1]
        give(&mut h, 12); // deagle [2,3]
        assert_eq!(select_slot(&mut h, 2, false), None);
        assert!(h.menu_open);
        assert_eq!((h.menu_bucket, h.menu_row), (2, 1));
        select_slot(&mut h, 2, false);
        assert_eq!(h.menu_row, 3);
        select_slot(&mut h, 2, false);
        assert_eq!(h.menu_row, 1, "wraps");
        assert_eq!(select_slot(&mut h, 10, false), None, "slot 10 closes an open menu");
        assert!(!h.menu_open);
        assert_eq!(select_slot(&mut h, 10, false).as_deref(), Some("weapon_0"));
    }

    #[test]
    fn fast_switch_and_wires() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud::default();
        give(&mut h, 13);
        assert_eq!(select_slot(&mut h, 4, true).as_deref(), Some("weapon_13"));
        assert!(!h.menu_open);
        h.wires = [1, 1, 0];
        assert_eq!(select_slot(&mut h, 2, false).as_deref(), Some("cutwire_2"));
        assert_eq!(select_slot(&mut h, 5, false), None);
    }

    #[test]
    fn next_prev_skips_single_when_akimbo_owned() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud::default();
        give(&mut h, 9); // mk23 [2,2]
        give(&mut h, 10); // mk23 akimbo [5,2]
        give(&mut h, 13); // ak [4,3]
        h.weapon = 13;
        let c = select_next_prev(&mut h, true, true);
        assert_eq!(c.as_deref(), Some("weapon_10"), "AK -> akimbo MK23, single MK23 skipped");
        h.weapon = 10;
        let c = select_next_prev(&mut h, true, true);
        assert_eq!(c.as_deref(), Some("weapon_13"), "wraps through bucket 1..3 to the AK");
    }

    /// A ruleset whose `SelectNextPrev` reloads the selection on every press (TS 2.1
    /// 0x1008cc40) steps one weapon from the held one each time; and its fast switch sends kung
    /// fu when the walk comes back round to it.
    #[test]
    fn next_prev_reload_and_kungfu_send() {
        ts30_gameplay::tables::make_default();
        let reloads =
            crate::gameplay::SlotMenu { closed_list: false, clip_info: false, next_prev_reloads: true, next_prev_sends_kungfu: true };
        let core = crate::gameplay::SlotMenu { next_prev_reloads: false, next_prev_sends_kungfu: false, ..reloads };
        let mut h = Hud::default();
        give(&mut h, 1); // glock [2,1]
        give(&mut h, 12); // deagle [2,3]
        give(&mut h, 13); // ak [4,3]
        h.weapon = 1;
        select_next_prev_as(&mut h, true, false, reloads);
        assert_eq!((h.menu_bucket, h.menu_row), (2, 3));
        select_next_prev_as(&mut h, true, false, reloads);
        assert_eq!((h.menu_bucket, h.menu_row), (2, 3), "reloaded from the held Glock again");
        select_next_prev_as(&mut h, true, false, core);
        assert_eq!((h.menu_bucket, h.menu_row), (4, 3), "the core walks on from the selection");

        let mut h = Hud::default();
        h.grid[0][1] = Slot { id: 0, clip: 0 };
        h.weapon = 0;
        assert_eq!(select_next_prev_as(&mut h, true, true, core), None);
        h.menu_open = false;
        assert_eq!(select_next_prev_as(&mut h, true, true, reloads).as_deref(), Some("weapon_0"));
    }
}
